# Browser RTL-SDR receiver

Capture and decode Sparsnäs locally using `WebUSB`, rtlsdr-pure, and the shared
sparslog receiver. No IQ server is required. Use a `WebUSB`-capable browser over
HTTPS or localhost. A kernel DVB driver may need to release the dongle first.

Run `./build-local.sh`, then `python3 serve.py`. Nightly, rust-src, the WASM
 target, wasm-pack, and `web_sys_unstable_apis` are used by the build configuration.
Select your sensor serial, optionally adjust advanced settings, and click Start
to choose a USB dongle. Stop releases the capture session. Measurements retain
the latest 200 CSV records. The waterfall shows the filtered signal; the time
sink shows filtered I/Q and demodulated samples with a rising trigger.

`../build-pages.sh` builds both applications at `ui/` and `ui-sdr/` in pages-dist.
Run `./presubmit.sh` for all crate checks.
