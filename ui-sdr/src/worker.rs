use std::cell::RefCell;
use std::future::{Future, poll_fn};
use std::task::Poll;

use async_channel::{Receiver, Sender};
use rustradio::blocks::{ComplexToFloat, Fft, NCMap, StreamAlign, StreamChunks, Tee};
use rustradio::graph::GraphRunner;
use rustradio::{Complex, Float};
use rustradio_ui::AppEmpty;
use rustradio_ui::worker::send_message;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;

use crate::display::{DisplaySink, MessageSink, WaveformSink, waveform_size, window_chunk};
use crate::{AppMessage, Connection, MainToWorker, WorkerToMain};

pub(crate) const SPECTRUM: &str = "spectrum";
pub(crate) const TIME: &str = "time";
const FFT_SIZE: u16 = 2048;
const ROWS_PER_SECOND: f32 = 30.0;
type SourceBridge = (Sender<rustradio_ui::worker::source::Msg<u8>>, Sender<()>);
thread_local! {
    static ACTIVE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static SOURCE: RefCell<Option<SourceBridge>> = const { RefCell::new(None) };
    static STOP: RefCell<Option<Sender<()>>> = const { RefCell::new(None) };
}

// Allow Stop during both connection establishment and graph execution. Dropping
// the losing future drops its source/socket guard and cancels the IQ session.
async fn until_stop<T>(
    future: impl Future<Output = rustradio::Result<T>>,
    stop: &Receiver<()>,
) -> rustradio::Result<Option<T>> {
    let mut future = std::pin::pin!(future);
    let mut stopped = std::pin::pin!(stop.recv());
    poll_fn(|cx| {
        if stopped.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Ok(None));
        }
        future.as_mut().poll(cx).map(|result| result.map(Some))
    })
    .await
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Display rate and browser Unix seconds.
async fn run_graph(settings: Connection, stop: Receiver<()>) -> rustradio::Result<String> {
    ACTIVE.with(|slot| slot.set(settings.session));
    let (poke, wake) = async_channel::bounded(1);
    let mut graph = rustradio::wasm::wasm_graph::WasmGraph::new();
    let (source, bytes, tx) = rustradio_ui::worker::source::WasmSource::<AppMessage, u8>::new(
        format!("usb-{}", settings.session),
    )?;
    SOURCE.with(|slot| *slot.borrow_mut() = Some((tx, poke.clone())));
    graph.add(Box::new(source));
    let (convert, samples) = rustradio::blocks::RtlSdrDecode::new(bytes)?;
    graph.add(Box::new(convert));
    let output = sparslog::add_receiver_with_taps(
        &mut graph,
        samples,
        &sparslog::ReceiverConfig {
            sensor_id: settings.serial,
            sample_rate: settings.sample_rate,
            tune_offset: settings.tune_offset,
            fsk_offset: settings.fsk_offset,
        },
    )
    .map_err(|e| rustradio::Error::msg(e.to_string()))?;
    let sample_rate = output.sample_rate as f32;
    let demodulated_rate = output.sample_rate;
    let waveform_points = waveform_size(demodulated_rate)?;
    send_message(WorkerToMain::ApplicationSpecific(AppMessage::Connected {
        filtered_rate: sample_rate,
        demodulated_rate,
    }))
    .await?;
    let (format, messages) = rustradio::blocks::NCMap::new(
        output.measurements,
        "CSV measurements",
        |measurement: sparslog::Measurement, tags| {
            vec![(
                measurement.to_csv((js_sys::Date::now() / 1000.0) as u64),
                tags,
            )]
        },
    );
    graph.add(Box::new(format));
    let samples = output.filtered;
    let waveform = output.demodulated;
    let messages_done = message_display(&mut graph, messages, poke);
    let (filtered, rows_done) = spectrum_display(&mut graph, samples, sample_rate)?;
    let waveform_done = waveform_display(&mut graph, filtered, waveform, waveform_points)?;
    let completed = until_stop(graph.run_async(wake), &stop).await;
    // Finish posting this session's rows before End enables another connection.
    // Otherwise an old row could arrive after the UI clears its new waterfall.
    drop(graph);
    let _ = rows_done.recv().await;
    let _ = waveform_done.recv().await;
    let _ = messages_done.recv().await;
    SOURCE.with(|slot| slot.borrow_mut().take());
    Ok(match completed {
        Ok(Some(())) => "Capture completed".into(),
        Ok(None) => "Stopped".into(),
        Err(error) => format!("Receiver failed: {error}"),
    })
}

fn message_display(
    graph: &mut rustradio::wasm::wasm_graph::WasmGraph,
    messages: rustradio::stream::NCReadStream<String>,
    poke: Sender<()>,
) -> Receiver<()> {
    let (message_tx, message_rx) = async_channel::bounded(32);
    graph.add(Box::new(MessageSink {
        src: messages,
        messages: message_tx,
    }));
    let (notify_done, messages_done) = async_channel::bounded(1);
    spawn_local(async move {
        while let Ok(message) = message_rx.recv().await {
            // Wake the graph after freeing space in the display queue.
            let _ = poke.try_send(());
            if send_message(WorkerToMain::ApplicationSpecific(AppMessage::Decoded(
                message,
            )))
            .await
            .is_err()
            {
                break;
            }
        }
        let _ = notify_done.send(()).await;
    });
    messages_done
}

fn spectrum_display(
    graph: &mut rustradio::wasm::wasm_graph::WasmGraph,
    samples: rustradio::stream::ReadStream<Complex>,
    sample_rate: f32,
) -> rustradio::Result<(rustradio::stream::ReadStream<Complex>, Receiver<()>)> {
    let (tee, spectrum, filtered) = Tee::new(samples)?;
    graph.add(Box::new(tee));
    let (chunks, chunks_out) = StreamChunks::new(spectrum, usize::from(FFT_SIZE))?;
    graph.add(Box::new(chunks));
    let window = rustradio::window::WindowType::Hamming
        .make_window(usize::from(FFT_SIZE))
        .0;
    // Limit FFT and UI work to roughly 30 rows/s even for a high-rate stream.
    // Round upward to a positive whole number of windows between display rows.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let keep_every = (sample_rate / f32::from(FFT_SIZE) / ROWS_PER_SECOND)
        .ceil()
        .max(1.0) as usize;
    let mut count = 0;
    let (select, fft_in) = NCMap::new(chunks_out, "waterfall windows", move |samples, tags| {
        count = (count + 1) % keep_every;
        if count == 0 {
            window_chunk(samples, tags, &window)
        } else {
            vec![]
        }
    });
    graph.add(Box::new(select));
    let (fft, fft_out) = Fft::from_fft_size(fft_in, usize::from(FFT_SIZE))?;
    graph.add(Box::new(fft));
    let (power, power_out) = NCMap::new(fft_out, "FFT power", |bins: Vec<Complex>, tags| {
        vec![(
            bins.iter()
                .map(|bin| 10.0 * (bin.norm_sqr() / f32::from(FFT_SIZE)).max(1.0e-20).log10())
                .collect(),
            tags,
        )]
    });
    graph.add(Box::new(power));
    let rows_done = display(graph, power_out, SPECTRUM);
    Ok((filtered, rows_done))
}

fn waveform_display(
    graph: &mut rustradio::wasm::wasm_graph::WasmGraph,
    filtered: rustradio::stream::ReadStream<Complex>,
    demodulated: rustradio::stream::ReadStream<Float>,
    points: usize,
) -> rustradio::Result<Receiver<()>> {
    let (align, filtered, demodulated) = StreamAlign::new(filtered, demodulated)?;
    graph.add(Box::new(align));
    let (convert, in_phase, quadrature) = ComplexToFloat::new(filtered)?;
    graph.add(Box::new(convert));
    let (chunks, in_phase) = StreamChunks::new(in_phase, points)?;
    graph.add(Box::new(chunks));
    let (chunks, quadrature) = StreamChunks::new(quadrature, points)?;
    graph.add(Box::new(chunks));
    let (chunks, demodulated) = StreamChunks::new(demodulated, points)?;
    graph.add(Box::new(chunks));
    let (frames, windows) = async_channel::bounded(2);
    graph.add(Box::new(WaveformSink {
        in_phase,
        quadrature,
        demodulated,
        frames,
    }));
    let (done, finished) = async_channel::bounded(1);
    spawn_local(async move {
        while let Ok(window) = windows.recv().await {
            if send_message(WorkerToMain::Floats(TIME.into(), window))
                .await
                .is_err()
            {
                break;
            }
        }
        let _ = done.send(()).await;
    });
    Ok(finished)
}

fn display(
    graph: &mut rustradio::wasm::wasm_graph::WasmGraph,
    input: rustradio::stream::NCReadStream<Vec<Float>>,
    name: &'static str,
) -> Receiver<()> {
    let (frames, rows) = async_channel::bounded(2);
    let (done, finished) = async_channel::bounded(1);
    graph.add(Box::new(DisplaySink { src: input, frames }));
    spawn_local(async move {
        while let Ok(row) = rows.recv().await {
            if send_message(WorkerToMain::Floats(name.into(), vec![row]))
                .await
                .is_err()
            {
                break;
            }
        }
        let _ = done.send(()).await;
    });
    finished
}

pub(crate) async fn setup() -> Result<(), JsValue> {
    rustradio_ui::worker::setup::<AppMessage, AppMessage, _>(|messages| {
        spawn_local(async move {
            let _ = send_message(WorkerToMain::Ready(AppEmpty {})).await;
            while let Ok(message) = messages.recv().await {
                match message {
                    MainToWorker::Start(settings) => {
                        if STOP.with(|slot| slot.borrow().is_some()) {
                            continue;
                        }
                        let (tx, rx) = async_channel::bounded(1);
                        STOP.with(|slot| *slot.borrow_mut() = Some(tx));
                        spawn_local(async move {
                            let result = run_graph(settings, rx).await;
                            SOURCE.with(|slot| slot.borrow_mut().take());
                            STOP.with(|slot| slot.borrow_mut().take());
                            let text = result.unwrap_or_else(|e| format!("Stream failed: {e}"));
                            let _ = send_message(WorkerToMain::End(text)).await;
                        });
                    }
                    MainToWorker::ApplicationSpecific(AppMessage::Stop) => {
                        STOP.with(|slot| {
                            if let Some(tx) = slot.borrow().as_ref() {
                                let _ = tx.try_send(());
                            }
                        });
                    }
                    MainToWorker::Bytes(name, streams)
                        if name == format!("usb-{}", ACTIVE.with(std::cell::Cell::get)) =>
                    {
                        let source = SOURCE.with(|slot| slot.borrow().clone());
                        if let Some((tx, poke)) = source {
                            for stream in streams {
                                let _ = tx
                                    .send(rustradio_ui::worker::source::Msg::Extend(stream.data))
                                    .await;
                            }
                            let _ = poke.try_send(());
                        }
                    }
                    _ => {}
                }
            }
        });
    })
    .await
}
