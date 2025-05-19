# sparslog

https://github.com/ThomasHabets/sparslog

Log power meter data from an IKEA Sparsnäs, by sniffing the radio
communication from the sensor to the display.

The communication is one-way, so once you start logging you'll no
longer need the display. Unless you like it, of course.

## What you'll need

An RTL SDR dongle. Any one will do. This is hardly a demanding
protocol.

Of course the better the SDR you have, the more distance and noise
you'll be able to handle.

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

The format is
`timestamp,sequence_number,watts,kwh,battery_status,CRC_status`

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
