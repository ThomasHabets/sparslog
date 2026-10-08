use std::cell::RefCell;
use std::future::{Future, poll_fn};
use std::task::Poll;

use async_channel::{Receiver, Sender};
use rustradio::blocks::{ComplexToFloat, Fft, NCMap, StreamAlign, StreamChunks, Tee};
use rustradio::graph::GraphRunner;
use rustradio::iq_stream::{StreamOptions, proto};
use rustradio::{Complex, Float};
use rustradio_ui::AppEmpty;
use rustradio_ui::worker::{IqStreamSource, send_message};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::spawn_local;

use crate::display::{DisplaySink, WaveformSink, waveform_size, window_chunk};
use crate::{AppMessage, Connection, MainToWorker, WorkerToMain};

pub(crate) const SPECTRUM: &str = "spectrum";
pub(crate) const TIME: &str = "time";
const FFT_SIZE: u16 = 2048;
const ROWS_PER_SECOND: f32 = 30.0;
thread_local! {
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

async fn run_graph(settings: Connection, stop: Receiver<()>) -> rustradio::Result<String> {
    let (poke, wake) = async_channel::bounded(1);
    let options = StreamOptions {
        loss_policy: proto::LossPolicy::AllowGaps,
        ..Default::default()
    };
    let Some((source, samples, handle)) = until_stop(
        IqStreamSource::<Complex>::connect(
            &settings.url,
            "filtered",
            options.clone(),
            poke.clone(),
        ),
        &stop,
    )
    .await?
    else {
        return Ok("Disconnected".into());
    };
    let Some((demodulated, waveform, demodulated_handle)) = until_stop(
        IqStreamSource::<Float>::connect(&settings.url, "demodulated", options, poke),
        &stop,
    )
    .await?
    else {
        return Ok("Disconnected".into());
    };
    let demodulated_rate = demodulated_handle.description().sample_rate_hz;
    let waveform_points = waveform_size(demodulated_rate)?;
    // These negotiated rates must describe the same clock, without tolerance.
    #[allow(clippy::float_cmp)]
    if handle.description().sample_rate_hz != demodulated_rate {
        return Err(rustradio::Error::msg(
            "StreamAlign requires filtered and demodulated sample rates to match",
        ));
    }
    // The waterfall API takes f32; reject rates outside its finite range below.
    #[allow(clippy::cast_possible_truncation)]
    let sample_rate = handle.description().sample_rate_hz as f32;
    if !sample_rate.is_finite() || sample_rate <= 0.0 {
        return Err(rustradio::Error::msg(
            "Sample rate is outside the waterfall's range",
        ));
    }
    send_message(WorkerToMain::ApplicationSpecific(AppMessage::Connected {
        filtered_rate: sample_rate,
        demodulated_rate,
    }))
    .await?;
    let mut graph = rustradio::wasm::wasm_graph::WasmGraph::new();
    graph.add(Box::new(source));
    graph.add(Box::new(demodulated));
    let (tee, spectrum, filtered) = Tee::new(samples);
    graph.add(Box::new(tee));
    let (chunks, chunks_out) = StreamChunks::new(spectrum, usize::from(FFT_SIZE));
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
    let rows_done = display(&mut graph, power_out, SPECTRUM);
    let waveform_done = waveform_display(&mut graph, filtered, waveform, waveform_points);
    let completed = until_stop(graph.run_async(wake), &stop).await;
    // Finish posting this session's rows before End enables another connection.
    // Otherwise an old row could arrive after the UI clears its new waterfall.
    drop(graph);
    let _ = rows_done.recv().await;
    let _ = waveform_done.recv().await;
    let lost = handle.lost_samples();
    let demodulated_lost = demodulated_handle.lost_samples();
    let outcome = match completed {
        Ok(Some(())) => "Streams completed".to_owned(),
        Ok(None) => "Disconnected".to_owned(),
        Err(error) => format!("Stream failed: {error}"),
    };
    Ok(format!(
        "{outcome} · filtered: {lost} missing samples · demodulated: {demodulated_lost} missing samples"
    ))
}

fn waveform_display(
    graph: &mut rustradio::wasm::wasm_graph::WasmGraph,
    filtered: rustradio::stream::ReadStream<Complex>,
    demodulated: rustradio::stream::ReadStream<Float>,
    points: usize,
) -> Receiver<()> {
    let (align, filtered, demodulated) = StreamAlign::new(filtered, demodulated);
    graph.add(Box::new(align));
    let (convert, in_phase, quadrature) = ComplexToFloat::new(filtered);
    graph.add(Box::new(convert));
    let (chunks, in_phase) = StreamChunks::new(in_phase, points);
    graph.add(Box::new(chunks));
    let (chunks, quadrature) = StreamChunks::new(quadrature, points);
    graph.add(Box::new(chunks));
    let (chunks, demodulated) = StreamChunks::new(demodulated, points);
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
    finished
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
                    _ => {}
                }
            }
        });
    })
    .await
}
