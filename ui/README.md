# Sparslog WASM viewer

View `filtered` as a waterfall and aligned filtered I/Q plus `demodulated`
as three traces in a 50 ms time sink.
Both streams use the receiver’s existing nonblocking IQ listener.

## Build and run

Install `wasm-pack` and the nightly Rust toolchain with `rust-src` and the
`wasm32-unknown-unknown` target, then run:

```sh
cd ui
./build-local.sh
python3 serve.py
```

Open <http://127.0.0.1:8080>. In another terminal, start the receiver:

```sh
cargo run --bin sparslog -- --serial 123456 --rtlsdr --iq-listen 127.0.0.1:9000
```

The header’s **Theme** selector offers System, Light, and Dark modes and remembers
your choice. Plot canvases currently follow the system theme because rustradio-ui
does not expose a theme override.

Enter the listener’s host and port and click **Connect**. **Disconnect** closes
both streams; reconnecting clears both plots. HTTPS pages default to TLS and
require a `wss://` backend, typically provided by a reverse proxy.

`./build-local.sh release` produces optimized output in `web-dist/`. The static
server supplies the COOP/COEP headers required for the shared-memory worker.
Other hosting must supply the same headers and serve the assets from one origin.

This crate is an independent Cargo workspace using published `rustradio-ui`
0.1.25 and a local crates.io patch for `rustradio` from `../../rustradio`.
The sibling checkout provides the unreleased `StreamAlign` block and must be
present to build. The build copies UI assets from the resolved registry package.

## Displays

The waterfall uses 2048-sample Hamming windows, FFT power in dB, and at most
approximately 30 rows per second. Its frequency axis is relative to the stream
center. The filtered stream is teed into the waterfall and `StreamAlign` with
the demodulated stream. The aligned filtered stream is converted to Float I and
Q. The time sink shows all three aligned traces in successive 50 ms windows with
pause, autoscale, and Y-range controls. Alignment uses absolute sample tags and
requires equal sample rates. Both axes use the negotiated sample rates.

Both connections allow gaps. Alignment drops unmatched samples. Time windows
containing a gap in any trace are discarded together; waveform
history is cleared between windows so traces cannot span missing samples.
Display queues are bounded and may drop windows if rendering is slow. Network
missing-sample counts are reported separately for each stream when the session
ends. A failure of either connection ends the session. There is no automatic
reconnection or recording.

## Checks

Run `./presubmit.sh` for formatting, documentation, native helper tests, WASM
Clippy, and the complete WASM build. The repository’s tickbox workflow includes
this script alongside all receiver checks.
