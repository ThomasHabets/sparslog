use std::cell::{OnceCell, RefCell};

use rustradio_ui::mainthread::spectrum_sink::{WaterfallSink, WaterfallSinkOptions};
use rustradio_ui::mainthread::time_sink::{
    TimeSink, TimeSinkOptions, TimeSinkTrigger, TriggerEdge,
};
use rustradio_ui::mainthread::{get_button, get_element, get_input, send_message};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::{AppMessage, Connection, MainToWorker, WorkerToMain};

thread_local! {
    static DECODED: RefCell<std::collections::VecDeque<String>> = const { RefCell::new(std::collections::VecDeque::new()) };
    static TIME: RefCell<Option<TimeSink>> = const { RefCell::new(None) };
    static WATERFALL: OnceCell<WaterfallSink> = const { OnceCell::new() };
}

fn status(text: &str) -> Result<(), JsValue> {
    get_element("status")?.set_text_content(Some(text));
    Ok(())
}

fn controls(active: bool) -> Result<(), JsValue> {
    get_button("connect")?.set_disabled(active);
    get_button("disconnect")?.set_disabled(!active);
    for name in [
        "serial",
        "frequency",
        "sample-rate",
        "gain",
        "tune-offset",
        "offset",
    ] {
        get_input(name)?.set_disabled(active);
    }
    Ok(())
}

fn number<T: std::str::FromStr>(id: &str) -> Result<T, JsValue> {
    get_input(id)?
        .value()
        .trim()
        .parse()
        .map_err(|_| JsValue::from_str(&format!("Invalid {id}")))
}

thread_local! {
    static CAPTURE_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
    static READ_ABORT: RefCell<Option<futures_util::future::AbortHandle>> = const { RefCell::new(None) };
    static RADIO: RefCell<Option<std::rc::Rc<rtlsdr_pure::RtlSdr>>> = const { RefCell::new(None) };
    static SESSION: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn connection() -> Result<Connection, JsValue> {
    let config = Connection {
        session: 0,
        serial: number("serial")?,
        sample_rate: number("sample-rate")?,
        tune_offset: number("tune-offset")?,
        fsk_offset: number("offset")?,
    };
    let frequency: u32 = number("frequency")?;
    let gain: f32 = number("gain")?;
    crate::settings::validate(
        config.sample_rate,
        config.tune_offset,
        config.fsk_offset,
        gain,
        frequency,
    )
    .map_err(|e| JsValue::from_str(&e))?;
    Ok(config)
}

#[allow(clippy::cast_possible_truncation)] // Gain is rounded to tenths of a dB.
fn connect() {
    let result = (|| {
        let settings = connection()?;
        CAPTURE_ERROR.with(|slot| slot.borrow_mut().take());
        WATERFALL
            .with(|slot| slot.get().expect("mounted waterfall").clear())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        TIME.with(|slot| slot.borrow().as_ref().expect("mounted time sink").clear())
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        DECODED.with(|messages| messages.borrow_mut().clear());
        get_element("decoded-output")?.set_text_content(Some("Waiting for decoded messages…"));
        controls(true)?;
        status("Connecting…")?;
        let frequency: u32 = number("frequency")?;
        let gain: f32 = number("gain")?;
        let session = SESSION.with(|s| {
            let n = s.get() + 1;
            s.set(n);
            n
        });
        spawn_local(async move {
            let result = async {
                let selectors: Vec<_> = rtlsdr_pure::KNOWN_DEVICES
                    .iter()
                    .map(|d| nusb::DeviceSelector::all().with_vid_pid(d.vendor_id, d.product_id))
                    .collect();
                let device = nusb::request_device(&selectors)
                    .await
                    .map_err(|e| JsValue::from_str(&e.to_string()))?
                    .ok_or_else(|| JsValue::from_str("Device selection cancelled"))?;
                if SESSION.with(std::cell::Cell::get) != session {
                    return Ok(());
                }
                let mut radio = rtlsdr_pure::RtlSdr::open(device)
                    .await
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                let mut settings = settings;
                settings.session = session;
                settings.sample_rate = radio
                    .set_sample_rate(settings.sample_rate)
                    .await
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                radio
                    .set_center_frequency(
                        u32::try_from(i64::from(frequency) + settings.tune_offset)
                            .map_err(|_| JsValue::from_str("Invalid capture frequency"))?,
                    )
                    .await
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                radio
                    .set_tuner_gain(rtlsdr_pure::GainMode::ManualTenthsDb(
                        (gain * 10.0).round() as i32
                    ))
                    .await
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                radio
                    .reset_buffer()
                    .await
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                if SESSION.with(std::cell::Cell::get) != session {
                    return Ok(());
                }
                RADIO.with(|slot| *slot.borrow_mut() = Some(std::rc::Rc::new(radio)));
                send_message(MainToWorker::Start(settings))
                    .await
                    .map_err(|e| JsValue::from_str(&e.to_string()))?;
                Ok::<_, JsValue>(())
            }
            .await;
            if SESSION.with(std::cell::Cell::get) != session {
                let _ = controls(false);
                let _ = status("Stopped");
                return;
            }
            if let Err(e) = result {
                let _ = status(&format!("Capture failed: {e:?}"));
                let _ = controls(false);
            }
        });
        Ok::<_, JsValue>(())
    })();
    if let Err(e) = result {
        let _ = status(&e.as_string().unwrap_or_else(|| format!("{e:?}")));
    }
}

#[allow(clippy::too_many_lines)]
async fn worker_msg(message: WorkerToMain) -> Result<(), JsValue> {
    match message {
        WorkerToMain::RequestData(name, size) => {
            if name != format!("usb-{}", SESSION.with(std::cell::Cell::get)) {
                return Ok(());
            }
            let radio = RADIO.with(|slot| slot.borrow().clone());
            let session = SESSION.with(std::cell::Cell::get);
            if let Some(radio) = radio {
                let (abort, registration) = futures_util::future::AbortHandle::new_pair();
                READ_ABORT.with(|slot| *slot.borrow_mut() = Some(abort));
                spawn_local(async move {
                    let result = futures_util::future::Abortable::new(
                        radio.read_bytes(size.min(65536) & !1),
                        registration,
                    )
                    .await;
                    let Ok(result) = result else {
                        return;
                    };
                    match result {
                        Ok(data) if SESSION.with(std::cell::Cell::get) == session => {
                            let _ = send_message(MainToWorker::Bytes(
                                name,
                                vec![rustradio_ui::TaggedVec { data, tags: vec![] }],
                            ))
                            .await;
                        }
                        Err(e) if SESSION.with(std::cell::Cell::get) == session => {
                            CAPTURE_ERROR.with(|slot| {
                                *slot.borrow_mut() = Some(format!("USB capture failed: {e}"));
                            });
                            let _ =
                                send_message(MainToWorker::ApplicationSpecific(AppMessage::Stop))
                                    .await;
                        }
                        _ => {}
                    }
                });
            }
        }
        WorkerToMain::Ready(_) => {
            controls(false)?;
            status("Ready to connect")?;
        }
        WorkerToMain::ApplicationSpecific(AppMessage::Connected {
            filtered_rate,
            demodulated_rate,
        }) => {
            WATERFALL
                .with(|slot| {
                    slot.get()
                        .expect("mounted waterfall")
                        .set_sample_rate(filtered_rate)
                })
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
            mount_time(demodulated_rate)?;
            status(&format!(
                "Streaming · filtered {filtered_rate} samples/s · demodulated {demodulated_rate} samples/s"
            ))?;
        }
        WorkerToMain::Floats(name, frames) if name == worker::SPECTRUM => {
            WATERFALL
                .with(|slot| slot.get().expect("mounted waterfall").update(&frames))
                .map_err(|e| JsValue::from_str(&e.to_string()))?;
        }
        WorkerToMain::Floats(name, frames) if name == worker::TIME => {
            TIME.with(|slot| {
                let slot = slot.borrow();
                let sink = slot.as_ref().expect("mounted time sink");
                if !sink.paused() {
                    // Free running displays one independent window at a time.
                    // Trigger mode must keep its detector and capture across
                    // frames, and hold the waveform while waiting for an edge.
                    if sink.trigger().is_none() {
                        sink.clear()?;
                    }
                    sink.update(frames)?;
                }
                Ok::<_, rustradio::Error>(())
            })
            .map_err(|e| JsValue::from_str(&e.to_string()))?;
        }
        WorkerToMain::ApplicationSpecific(AppMessage::Decoded(message)) => {
            let text = DECODED.with(|messages| {
                let mut messages = messages.borrow_mut();
                if messages.len() == 200 {
                    messages.pop_front();
                }
                messages.push_back(message);
                messages
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            get_element("decoded-output")?.set_text_content(Some(&text));
        }
        WorkerToMain::End(message) => {
            READ_ABORT.with(|slot| {
                if let Some(abort) = slot.borrow_mut().take() {
                    abort.abort();
                }
            });
            RADIO.with(|slot| slot.borrow_mut().take());
            let message = CAPTURE_ERROR
                .with(|slot| slot.borrow_mut().take())
                .unwrap_or(message);
            status(&message)?;
            controls(false)?;
        }
        _ => {}
    }
    Ok(())
}

fn mount_time(sample_rate: f64) -> Result<(), JsValue> {
    let max_points = crate::display::waveform_size(sample_rate)
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    let sink = TimeSink::mount_by_id(
        "time-sink",
        TimeSinkOptions {
            title: "Aligned waveforms".into(),
            subtitle: "Filtered I/Q and demodulated".into(),
            y_label: "Amplitude".into(),
            sample_rate,
            max_points,
            ..Default::default()
        },
    )
    .map_err(|e| JsValue::from_str(&e.to_string()))?;
    sink.set_trigger(Some(TimeSinkTrigger {
        level: 0.1,
        edge: TriggerEdge::Rising,
    }))
    .map_err(|e| JsValue::from_str(&e.to_string()))?;
    sink.set_trigger_delay(std::time::Duration::from_millis(1))
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    TIME.with(|slot| *slot.borrow_mut() = Some(sink));
    Ok(())
}

pub(crate) fn setup() -> Result<(), JsValue> {
    let navigator = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("navigator"))?;
    if !js_sys::Reflect::has(&navigator, &JsValue::from_str("usb"))? {
        return Err(JsValue::from_str(
            "WebUSB is unavailable. Use a WebUSB-capable browser on HTTPS or localhost.",
        ));
    }
    let waterfall = WaterfallSink::mount_by_id(
        "waterfall",
        WaterfallSinkOptions {
            title: "Filtered spectrum".into(),
            subtitle: "Waterfall · frequency relative to the stream center".into(),
            ..Default::default()
        },
    )
    .map_err(|e| JsValue::from_str(&e.to_string()))?;
    WATERFALL.with(|slot| {
        let _ = slot.set(waterfall);
    });
    mount_time(200_000.0)?;
    let connect_handler = Closure::<dyn FnMut()>::new(connect);
    get_button("connect")?
        .add_event_listener_with_callback("click", connect_handler.as_ref().unchecked_ref())?;
    connect_handler.forget();
    let disconnect_handler = Closure::<dyn FnMut()>::new(|| {
        let _ = get_button("disconnect").map(|button| button.set_disabled(true));
        SESSION.with(|s| s.set(s.get() + 1));
        READ_ABORT.with(|slot| {
            if let Some(abort) = slot.borrow_mut().take() {
                abort.abort();
            }
        });
        RADIO.with(|slot| slot.borrow_mut().take());
        let _ = status("Stopping…");
        spawn_local(async {
            if let Err(e) = send_message(MainToWorker::ApplicationSpecific(AppMessage::Stop)).await
            {
                let _ = status(&e.to_string());
                let _ = controls(false);
            }
        });
    });
    get_button("disconnect")?
        .add_event_listener_with_callback("click", disconnect_handler.as_ref().unchecked_ref())?;
    disconnect_handler.forget();
    rustradio_ui::mainthread::start_worker::<AppMessage, AppMessage, _, _>(worker_msg);
    Ok(())
}

use crate::worker;
