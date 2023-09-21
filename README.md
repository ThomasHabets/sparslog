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

## How to run

This project is written in Rust, and doesn't yet have support for any
software defined radio. So there's a GNURadio graph that just takes
the data and shoves it out over TCP.

There's a version with and without a GUI, in the `gr/` directory.
They both listen to TCP port 2000, which the Rust tool will connect
to.

### Running the GNURadio source

It's easiest to open `gr/source_gui.grc` in `gnuradio-companion`, and
pressing play.

### Running the decoder

Where `123456` is the serial number of your transmitter:

```
$ cargo build --release
$ ./target/release/sparslog -s 123456 -c 127.0.0.1:2000
```

It'll print stuff, and log to `sparslog.csv`.

The format is
`timestamp,sequence_number,watts,kwh,battery_status,CRC_status`

## Related projects

* [Technical analysis](https://github.com/kodarn/Sparsnas)
* [Simple implementation](https://github.com/strigeus/sparsnas_decoder)
