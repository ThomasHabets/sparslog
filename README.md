# sparslog

https://github.com/ThomasHabets/sparslog

Log power meter data from an IKEA Sparsnäs, by sniffing the radio
communication from the sensor to the display.

The communication between the sensor and its display is one-way, so once you
start logging using this tool, you'll no longer need the display. Unless you
like it, of course.

## What you'll need

An RTL SDR dongle. Any one will do. This is hardly a demanding
protocol.

Of course the better the SDR you have, ideally with an antenna made for 868MHz,
the more distance and noise you'll be able to handle.

The packets are digital, so if the CRC is correct, then the data is
very likely correct.

You'll also need the serial number of your transmitter. It's under the
batteries in the device that attaches to the electricity meter.

### Running the decoder

Where `123456` is the serial number of your transmitter:

```
$ cargo build --release
$ ./target/release/sparslog -s 123456 --rtlsdr
```

It'll print stuff, and log to `sparslog.csv`.

For a live RTL-SDR dongle, `--freq` specifies the desired channel (default
868 MHz). The dongle tunes 100 kHz above that channel, and samples are translated
back to baseband before the low-pass filter to reject the hardware's DC spike.

Frequency flags `--freq`, `--sample-rate`, and `--tune-offset` accept k/M/G
suffixes and underscores, for example `--freq 868M --sample-rate 1.024M`.
Use `--tune-offset HZ` to change this signed tuning offset, or
`--tune-offset 0` for direct tuning. For example, `--tune-offset -100k` tunes
100 kHz below the channel. The offset magnitude plus the filter's 60 kHz edge must fit
within half the input sample rate. File inputs, including `--read --rtlsdr`, and
TCP inputs are already centered and are not translated. The existing `--offset`
option still adjusts the demodulated FSK signal.

The CSV format is
`timestamp,sequence_number,watts,kwh,battery_status,CRC_status`

You can also provide `--prometheus` and `--where`, to send the measurements to a
prometheus metric gateway.

### Streaming I/Q

Both `sparslog` and `sparslog-async` accept `--iq-listen IP:PORT`:

```
$ ./target/release/sparslog -s 123456 --rtlsdr --iq-listen '[::]:9000'
```

This port allows tapping at select parts of the flowgraph. The UI uses these
streams to receive the RF spectrum, the time series, and finished decodes.

The full list of streams can be seen with:

```
$ curl -s http://127.0.0.1:9000/iq/v1/streams | jq  .
[
  […]
  {
    "name": "filtered",
    "messages": false,
    "sample_rate_hz": 200000.0,
    "ended": false,
    "encoding": {
      "component_type": 1,
      "byte_order": 1,
      "layout": 2
    }
  },
  […]]
]
```

For a live RTL-SDR dongle, `raw` contains complex float32 samples immediately
after `RtlSdrDecode`, before frequency translation or filtering, at the dongle's
sample rate. It is not available for file, TCP, or IqStream inputs, including
RTL-format files.

The resource `filtered` contains complex float32 samples immediately after
the rational resampler, before demodulation, at 200 kHz.

The resource `demodulated` contains real float32 samples after demodulation and
the `--offset` adjustment (`AddConst`), before `ZeroCrossing`, also at 200 kHz.

`decoded` carries each decoded CSV record as a complete UTF-8 string, at a
nominal rate of one message every 15 seconds.

Clients must allow gaps (and the UI does): absent or slow clients do not stop
decoding, and overflow is reported as gaps. Streaming is disabled unless the
option is given.

### Receiving an IqStream

Sparslog can read complex samples from an IqStream gRPC server, such as another
sparslog running on another machine:

```
$ ./target/release/sparslog -s 123456 --iq-connect http://127.0.0.1:9000
```

The default resource is `raw`. Use `--iq-source NAME` to select another complex
resource. The advertised sample rate is used instead of `--sample-rate` and must
be a positive whole number of Hz within the u32 range. IqStream input cannot be
combined with `--read`, `--connect`, or `--rtlsdr`.

IqStream samples are translated using `--tune-offset` (default 100 kHz), matching
the default live RTL-SDR tuning offset. Match this value to the sending dongle's
offset. For an already centered resource, disable translation:

```
$ ./target/release/sparslog -s 123456 --iq-connect http://127.0.0.1:9000 \
    --iq-source filtered --tune-offset 0
```

The offset magnitude plus the filter's 60 kHz edge must fit within half the
advertised sample rate. Downloads allow gaps; dropped samples are not replayed
and connections are not automatically retried. `--iq-listen` can also be used
while receiving to serve the filtered, demodulated, and decoded results.

Building uses the published rustradio crate with its `unstable` feature enabled,
as the streaming currently requires it.

### Browser stream viewer

The [WASM stream viewer](ui/README.md) displays `filtered` as a waterfall and
aligned filtered I/Q plus `demodulated` in a time sink, and decoded records.
Build and serve it from the `ui/` subdirectory, then connect to the receiver's
`--iq-listen` address.

```
cd ui
./build-local.sh && python3 serve.py
```

### Decoder with tokio-console

This requires both the `tokio_unstable` config in `RUSTFLAGS` and the
`tokio-unstable` feature.

```
$ RUSTFLAGS="--cfg tokio_unstable" cargo build \
    -F tokio-unstable \
    --release
```

## Bonus feature: A GNURadio implementation

There's also a GNURadio implementation in the `gr/` directory.

## Related projects

* [Technical analysis](https://github.com/kodarn/Sparsnas)
* [Simple implementation](https://github.com/strigeus/sparsnas_decoder)
