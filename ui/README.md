# Sparslog WASM viewer

View `filtered` as a waterfall and `demodulated` as a 50 ms waveform.
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

Enter the listener’s host and port and click **Connect**. **Disconnect** closes
both streams; reconnecting clears both plots. HTTPS pages default to TLS and
require a `wss://` backend, typically provided by a reverse proxy.

`./build-local.sh release` produces optimized output in `web-dist/`. The static
server supplies the COOP/COEP headers required for the shared-memory worker.
Other hosting must supply the same headers and serve the assets from one origin.

This crate is an independent Cargo workspace using published `rustradio-ui`
0.1.25 and `rustradio` 0.18.6 or compatible releases. It does not inherit the
receiver’s local dependency patches. The build copies library assets from the
resolved registry package; no sibling checkout is required.

## Displays

The waterfall uses 2048-sample Hamming windows, FFT power in dB, and at most
approximately 30 rows per second. Its frequency axis is relative to the stream
center. The time sink shows successive 50 ms windows with pause, autoscale, and
Y-range controls. Both axes use the negotiated sample rates.

Both connections allow gaps. Windows containing gaps are discarded; waveform
history is cleared between windows so traces cannot span missing samples.
Display queues are bounded and may drop windows if rendering is slow. Network
missing-sample counts are reported separately for each stream when the session
ends. A failure of either connection ends the session. There is no automatic
reconnection or recording.

## Checks

Run `./presubmit.sh` for formatting, documentation, native helper tests, WASM
Clippy, and the complete WASM build. The repository’s tickbox workflow includes
this script alongside all receiver checks.
