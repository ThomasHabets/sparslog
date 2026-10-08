#![doc = include_str!("../README.md")]

#[cfg(any(target_arch = "wasm32", test))]
mod display;
#[cfg(target_arch = "wasm32")]
mod mainthread;
#[cfg(target_arch = "wasm32")]
mod worker;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[cfg(target_arch = "wasm32")]
pub(crate) struct Connection {
    url: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[cfg(target_arch = "wasm32")]
pub(crate) enum AppMessage {
    Stop,
    Connected {
        filtered_rate: f32,
        demodulated_rate: f64,
    },
}

#[cfg(target_arch = "wasm32")]
impl rustradio_ui::ApplicationSpecific for AppMessage {
    type App = Self;
    type Start = Connection;
    type End = String;
    type Ready = rustradio_ui::AppEmpty;
}

#[cfg(target_arch = "wasm32")]
type MainToWorker = rustradio_ui::MainToWorker<AppMessage>;
#[cfg(target_arch = "wasm32")]
type WorkerToMain = rustradio_ui::WorkerToMain<AppMessage>;

/// Initialize the page or its DSP worker, depending on the execution context.
///
/// # Errors
/// Returns browser setup or worker startup errors.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub async fn start() -> Result<(), wasm_bindgen::JsValue> {
    console_error_panic_hook::set_once();
    if web_sys::window().is_none() {
        worker::setup().await
    } else {
        rustradio_ui::dom_logger::init_logging::<AppMessage>("log-output", log::LevelFilter::Info)
            .map_err(|e| wasm_bindgen::JsValue::from_str(&e.to_string()))?;
        mainthread::setup()
    }
}
