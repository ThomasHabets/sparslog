//! Sparsnäs DSP and packet decoding for native and browser applications.
//!
//! Use [`add_receiver`] with a caller-provided complex sample stream, or attach
//! [`Decoder`] to sliced bits. Both produce [`Measurement`] values without
//! host I/O or a clock. Disable default features to omit native CLI dependencies.
//! rustradio's browser graph support is enabled automatically on `wasm32`.

mod decoder;
mod receiver;

pub use decoder::{Decoder, Measurement};
pub use receiver::{ReceiverConfig, ReceiverStreams, add_receiver, add_receiver_with_taps};

/// Native CLI input, output, metrics, and IQ services.
#[cfg(feature = "cli")]
pub mod cli;
#[cfg(feature = "cli")]
pub use cli::{IqListener, Opt, create_graph};

/// Package version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
