#![allow(clippy::missing_panics_doc)]
use std::collections::VecDeque;
use std::io::Write;
use std::net::SocketAddr;

use anyhow::anyhow;
use log::debug;

use rustradio::block::{Block, BlockRet};
use rustradio::blocks::{
    AddConst, BinarySlicer, FftFilter, FileSource, IqStreamSink, IqStreamSource, QuadratureDemod,
    RationalResampler, Rotator, RtlSdrDecode, RtlSdrSource, TcpSource, Tee, ZeroCrossing,
};
use rustradio::graph::GraphRunner;
use rustradio::iq_stream::{IqServer, IqStreamStringSink, StreamOptions, proto};
use rustradio::stream::{NCWriteStream, ReadStream, new_nocopy_stream};
use rustradio::window::WindowType;
use rustradio::{Complex, Result, blockchain};

use std::sync::LazyLock;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

static REGISTRY: LazyLock<prometheus::Registry> = LazyLock::new(prometheus::Registry::new);

static WATTS: LazyLock<prometheus::Gauge> = LazyLock::new(|| {
    let metric =
        prometheus::Gauge::new("electricity_watts", "The instantenous watts used.").unwrap();
    REGISTRY.register(Box::new(metric.clone())).unwrap();
    metric
});

static BATTERY: LazyLock<prometheus::IntGauge> = LazyLock::new(|| {
    let metric = prometheus::IntGauge::new(
        "electricity_meter_battery",
        "Battery status of electricity meter.",
    )
    .unwrap();
    REGISTRY.register(Box::new(metric.clone())).unwrap();
    metric
});

static KWH: LazyLock<prometheus::Counter> = LazyLock::new(|| {
    let metric = prometheus::Counter::new("electricity_kwh", "Kwh counter.").unwrap();
    REGISTRY.register(Box::new(metric.clone())).unwrap();
    metric
});

static DECODES: LazyLock<prometheus::IntCounterVec> = LazyLock::new(|| {
    let metric = prometheus::IntCounterVec::new(
        prometheus::Opts::new("electricity_sparsnas_decodes", "Number of decodes."),
        &["status"],
    )
    .unwrap();
    REGISTRY.register(Box::new(metric.clone())).unwrap();
    metric
});

#[derive(clap::Parser, Debug)]
#[command(version=concat!(
        env!("CARGO_PKG_VERSION"),
        // This doesn't work for cargo install builds.
        //"\nCommit: ",
        //env!("GIT_VERSION"),
        //
        // This makes the build non-reproducible.
        // "\nBuilt at: ",
        // env!("BUILD_TIMESTAMP"),
        ), about)]
pub struct Opt {
    /// Serial number of the sensor.
    #[arg(short, long = "serial")]
    sensor_id: u32,

    /// Output file that will be appended to.
    #[arg(short, long = "output", default_value = "sparslog.csv")]
    output: String,

    /// Read 32bit Complex float stream by connecting with TCP.
    #[arg(short, long = "connect")]
    connect: Option<String>,

    /// Read complex samples from an `IqStream` gRPC URL, using its advertised rate.
    #[arg(long, conflicts_with_all = ["read", "connect", "rtlsdr"])]
    iq_connect: Option<String>,

    /// `IqStream` resource to read. Centered streams need --tune-offset 0.
    #[arg(long, default_value = "raw", requires = "iq_connect")]
    iq_source: String,

    /// Read I/Q from file. Can be combined with --rtlsdr.
    #[arg(short, long = "read")]
    read: Option<String>,

    /// Read from RTLSDR dongle, or in its format from a file when used with
    /// --read.
    #[arg(long = "rtlsdr")]
    rtlsdr: bool,

    /// Log level: error, warn, info, debug, or trace.
    #[arg(short, default_value = "error", value_parser = rustradio::parse_verbosity)]
    pub verbose: usize,

    /// Input gain. Used with --rtlsdr.
    #[arg(long = "gain", default_value = "30")]
    gain: f32,

    /// File, TCP, or dongle sample rate (`IqStream` uses metadata); k/M/G supported.
    #[arg(long, default_value = "1.024M", value_parser = parse_frequency::<u32>)]
    sample_rate: u32,

    /// Desired channel frequency in Hz; accepts k/M/G suffixes.
    #[arg(long = "freq", default_value = "868M", value_parser = parse_frequency::<u64>)]
    freq: u64,

    /// Live RTL-SDR or `IqStream` tuning offset in Hz; k/M/G supported.
    /// Zero disables translation.
    #[arg(long, default_value = "100k", allow_hyphen_values = true, value_parser = parse_frequency::<i64>)]
    tune_offset: i64,

    /// FSK offset value.
    #[arg(long = "offset", default_value = "0.4")]
    offset: f32,

    /// Run multithreaded.
    #[arg(long)]
    pub multithread: bool,

    /// Serve I/Q and decoded streams on IP:PORT, including raw I/Q for a live dongle.
    #[arg(long)]
    iq_listen: Option<SocketAddr>,

    /// Prometheus gateway server to push metrics to.
    #[arg(long, requires = "where_")]
    prometheus: Option<String>,

    /// Name for location we're measuring.
    #[arg(long = "where")]
    where_: Option<String>,
}

#[derive(rustradio::rustradio_macros::Block)]
#[rustradio(new, custom_name)]
struct Decode {
    #[rustradio(in)]
    src: ReadStream<u8>,
    sensor_id: u32,
    output: String,
    messages: Option<NCWriteStream<String>>,

    #[rustradio(default)]
    history: VecDeque<u8>,
}

impl Decode {
    fn custom_name(&self) -> &'static str {
        let _ = self;
        "Sparsnäs decoder"
    }
}

#[allow(clippy::cast_precision_loss)]
fn f32_to_i32(value: f32) -> anyhow::Result<i32> {
    let value = f64::from(value);
    if value.is_finite() && value >= f64::from(i32::MIN) && value <= f64::from(i32::MAX) {
        #[allow(clippy::cast_possible_truncation)]
        Ok(value as i32)
    } else {
        Err(anyhow!("invalid conversion from {value} to i32"))
    }
}

#[allow(clippy::cast_precision_loss)]
fn f32_to_usize(value: f32) -> anyhow::Result<usize> {
    let value = f64::from(value);
    if value.is_finite() && value >= 0.0 && value <= usize::MAX as f64 {
        #[allow(clippy::cast_possible_truncation)]
        #[allow(clippy::cast_sign_loss)]
        Ok(value as usize)
    } else {
        Err(anyhow!("invalid conversion from {value} to usize"))
    }
}

fn bits2byte(data: &[u8]) -> u8 {
    assert_eq!(data.len(), 8);
    (data[0] << 7)
        | (data[1] << 6)
        | (data[2] << 5)
        | (data[3] << 4)
        | (data[4] << 3)
        | (data[5] << 2)
        | (data[6] << 1)
        | data[7]
}
fn calc_crc(mut s: u8, mut reg: u16) -> u16 {
    let poly: u16 = 0x8005;
    for _i in 0..8 {
        let regbit = reg & 0x8000 != 0;
        let databit = s & 0x80 != 0;
        if regbit ^ databit {
            reg = (reg << 1) ^ poly;
        } else {
            reg <<= 1;
        }
        s <<= 1;
    }
    reg
}

fn crc16(input: &[u8], expected: u16) -> bool {
    let mut checksum = 0xffffu16;
    for i in input {
        checksum = calc_crc(*i, checksum);
    }
    //eprintln!("Got checksum {:04x}, want {:04x}", checksum, expected);
    checksum == expected
}

// packet: from length to and including the CRC.
fn fix_packet(packet: &[u8]) -> Vec<u8> {
    let crc = (u16::from(packet[packet.len() - 2]) << 8) | u16::from(packet[packet.len() - 1]);
    if crc16(&packet[..packet.len() - 2], crc) {
        return packet.to_vec();
    }
    for i in 0..(packet.len() * 8) {
        let mut test = packet.to_vec();
        let bit = 1 << (i % 8);
        test[i / 8] ^= bit;
        if crc16(&test[..packet.len() - 2], crc) {
            return test.clone();
        }
    }
    packet.to_vec()
}

fn parsepacket(packet: &[u8], sensor_id: u32) -> String {
    assert_eq!(packet.len(), 20);
    //let sensor = packet[0];
    //let app = packet[1];
    let packet = fix_packet(packet);

    // This is the correct packet.
    println!("Packet: {packet:02x?}");

    let sensor_id_sub = {
        let magic = 0x5D38_E8CB;
        if sensor_id >= magic {
            sensor_id - magic
        } else {
            4_294_967_295 - (magic - sensor_id - 1)
        }
    };
    let enc_key = [
        ((sensor_id_sub >> 24) & 0xff) as u8,
        (sensor_id_sub & 0xff) as u8,
        ((sensor_id_sub >> 8) & 0xff) as u8,
        0x47u8,
        ((sensor_id_sub >> 16) & 0xff) as u8,
    ];
    let mut dec = Vec::new();
    for i in 0..13 {
        dec.push(packet[i + 5] ^ enc_key[i % 5]);
    }
    //println!("Decoded: {:02x?}", dec);
    //let mut prep = vec![0x11];
    //prep.extend(&packet[..packet.len()-2]);
    let crc = (u16::from(packet[packet.len() - 2]) << 8) | u16::from(packet[packet.len() - 1]);
    let crc_ok = crc16(&packet[..packet.len() - 2], crc);

    let seq = (u16::from(dec[4]) << 8) | u16::from(dec[5]);
    let effect = (u16::from(dec[6]) << 8) | u16::from(dec[7]);
    let wh = (u32::from(dec[8]) << 24)
        | (u32::from(dec[9]) << 16)
        | (u32::from(dec[10]) << 8)
        | u32::from(dec[11]);
    let kwh = format!("{}.{:03}", wh / 1000, wh % 1000);
    let battery = dec[12];

    let watt = 3600.0 * 1024.0 / f32::from(effect);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();

    if crc_ok {
        WATTS.set(watt.into());
        BATTERY.set(battery.into());
        if let Ok(v) = kwh.parse() {
            let old = KWH.get();
            if old > v {
                KWH.reset();
                KWH.inc_by(v);
            } else {
                KWH.inc_by(v - old);
            }
        }
    }
    let status = if crc_ok { "OK" } else { "BAD" };
    DECODES.with_label_values(&[status]).inc();
    format!("{now},{seq},{watt:.3},{kwh},{battery},{status}")
}

impl Block for Decode {
    fn work(&mut self) -> Result<BlockRet<'_>> {
        let cac = [
            1u8, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0,
            0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1,
        ];
        let (input, _) = self.src.read_buf()?;
        if input.is_empty() {
            return Ok(BlockRet::WaitForStream(&self.src, 1));
        }
        //eprintln!("Decode got {}", input.available());
        self.history.extend(input.iter());
        {
            let n = input.len();
            input.consume(n);
        }

        let packet_bits_len = cac.len() + 19 * 8;
        //let cac = vec![1,0,1,0,1,0,1,0,1,0,1];
        let n = self.history.len();
        //println!("Called with {n}");
        if n < packet_bits_len {
            //debug!("{} < {} len, sleeping", n, cac.len());
            return Ok(BlockRet::WaitForStream(&self.src, packet_bits_len - n));
        }
        //println!("Running on data size {n}");
        let input = &self.history;
        for i in 0..(n - packet_bits_len) {
            let equal = cac
                .iter()
                .zip(input.range(i..(i + cac.len())))
                .all(|(a, b)| a == b);
            //if &cac == input.range(i..(i + cac.len())) {
            if equal {
                debug!("Found CAC");
                let bits = &input
                    .range(i..(i + cac.len() + 19 * 8))
                    .copied()
                    .collect::<Vec<u8>>();
                let mut bytes = Vec::new();
                for j in (0..bits.len()).step_by(8) {
                    bytes.push(bits2byte(&bits[j..j + 8]));
                }
                //println!("bytes: {:02x?}", bytes);
                let packet = &bytes[4..];
                //println!("packet: {:02x?}", packet);
                let parsed = parsepacket(packet, self.sensor_id);
                std::fs::OpenOptions::new()
                    .append(true)
                    .create(true)
                    .open(&self.output)
                    .map_err(|e| -> rustradio::Error { e.into() })?
                    .write_all(format!("{parsed}\n").as_bytes())
                    .map_err(|e| -> rustradio::Error { e.into() })?;
                println!("{parsed}");
                if let Some(messages) = &self.messages {
                    if messages.remaining() > 0 {
                        messages.push(parsed, &[]);
                    } else {
                        log::warn!("Decoded stream full; dropping viewer message");
                    }
                }
            }
        }
        self.history
            .drain(0..(self.history.len() - packet_bits_len));
        Ok(BlockRet::Again)
    }
}

static HOSTNAME: LazyLock<String> = LazyLock::new(|| {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .map_or_else(|_| "unknown".to_owned(), |s| s.trim_end().to_owned())
        })
});

fn push_metrics(gw: &str, wh: &str, serial: u32) -> anyhow::Result<()> {
    debug!("Pushing metrics");
    let grouping = std::collections::HashMap::from([
        ("instance".to_string(), (*HOSTNAME).clone()),
        ("where".to_string(), wh.to_string()),
        ("serial".to_string(), serial.to_string()),
    ]);

    prometheus::push_metrics(
        "sparslog", // job name
        grouping,   // grouping labels
        gw,
        REGISTRY.gather(),
        None, // optional basic auth
    )?;
    Ok(())
}

/// Owns the optional IQ listener until graph execution finishes.
#[must_use = "retain the listener until graph execution ends"]
pub struct IqListener {
    address: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
}

impl IqListener {
    fn start(
        server: IqServer,
        address: SocketAddr,
        cancel: rustradio::graph::CancellationToken,
    ) -> anyhow::Result<Self> {
        let listener = std::net::TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("iq-listener".into())
            .spawn(move || {
                let result = runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener)?;
                    server
                        .serve(listener, async {
                            let _ = stopped.await;
                        })
                        .await?;
                    Ok(())
                });
                cancel.cancel();
                result
            })?;
        let listener = Self {
            address,
            stop: Some(stop),
            thread: Some(thread),
        };
        eprintln!("IQ streams listening on {}", listener.local_addr());
        Ok(listener)
    }

    /// Bound address, including the assigned port when port zero was requested.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// Stop the listener and report any server failure.
    ///
    /// # Errors
    ///
    /// Returns an error if the server failed or its thread panicked.
    pub fn shutdown(mut self) -> anyhow::Result<()> {
        self.stop_and_join()
    }

    fn stop_and_join(&mut self) -> anyhow::Result<()> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow!("IQ listener thread panicked"))??;
        }
        Ok(())
    }
}

impl Drop for IqListener {
    fn drop(&mut self) {
        if let Err(err) = self.stop_and_join() {
            eprintln!("IQ listener failed: {err}");
        }
    }
}

fn iq_tap<T: rustradio::iq_stream::IqSample>(
    graph: &mut (impl GraphRunner + ?Sized),
    input: ReadStream<T>,
    server: Option<&IqServer>,
    name: &str,
    sample_rate: f64,
) -> Result<ReadStream<T>> {
    let Some(server) = server else {
        return Ok(input);
    };
    let (tee, decoder, tapped) = Tee::new(input)?;
    graph.add(Box::new(tee));
    graph.add(Box::new(
        IqStreamSink::builder(tapped, server, name, sample_rate)
            .blocking(false)
            .build()?,
    ));
    Ok(decoder)
}

// Keep the graph's integer-Hz interfaces while accepting rustradio's unit syntax.
fn parse_frequency<T: num::NumCast>(text: &str) -> std::result::Result<T, String> {
    let hz = rustradio::parse_frequency(text)?;
    if !hz.is_finite() || hz.fract() != 0.0 {
        return Err("frequency must be a finite whole number of Hz".into());
    }
    num::cast(hz).ok_or_else(|| "frequency is outside the allowed range".into())
}

fn rtl_tune_frequency(opt: &Opt) -> anyhow::Result<u64> {
    let frequency = opt
        .freq
        .checked_add_signed(opt.tune_offset)
        .filter(|frequency| *frequency > 0 && u32::try_from(*frequency).is_ok())
        .ok_or_else(|| {
            anyhow!(
                "RTL-SDR tuning frequency must be between 1 and {} Hz",
                u32::MAX
            )
        })?;
    validate_tune_offset(opt.sample_rate, opt.tune_offset)?;
    Ok(frequency)
}

fn validate_tune_offset(sample_rate: u32, offset: i64) -> anyhow::Result<()> {
    // Include the low-pass filter's 50 kHz passband and 10 kHz transition.
    if offset.unsigned_abs() + 60_000 > u64::from(sample_rate) / 2 {
        return Err(anyhow!(
            "tuning offset plus 60 kHz must fit within half the sample rate"
        ));
    }
    Ok(())
}

fn iq_sample_rate(rate: f64) -> anyhow::Result<u32> {
    if !rate.is_finite() || rate.fract() != 0.0 || rate < 1.0 {
        return Err(anyhow!(
            "IqStream sample rate must be a positive whole number of Hz"
        ));
    }
    num::cast(rate).ok_or_else(|| anyhow!("IqStream sample rate exceeds the u32 range"))
}

fn live_rtl_samples(
    graph: &mut (impl GraphRunner + ?Sized),
    samples: ReadStream<Complex>,
    opt: &Opt,
    server: Option<&IqServer>,
) -> Result<ReadStream<Complex>> {
    let samples = iq_tap(graph, samples, server, "raw", f64::from(opt.sample_rate))?;
    translate_rtl(graph, samples, opt.sample_rate, opt.tune_offset)
}

#[allow(clippy::cast_precision_loss)]
fn translate_rtl(
    graph: &mut (impl GraphRunner + ?Sized),
    samples: ReadStream<Complex>,
    sample_rate: u32,
    offset: i64,
) -> Result<ReadStream<Complex>> {
    if offset == 0 {
        return Ok(samples);
    }
    // Tuning above the channel places it at -offset; mix it back to DC.
    let (rotator, centered) = Rotator::new(samples, sample_rate as f32, offset as f32)?;
    graph.add(Box::new(rotator));
    Ok(centered)
}

fn add_decoder(
    graph: &mut (impl GraphRunner + ?Sized),
    prev: ReadStream<u8>,
    opt: &Opt,
    server: Option<&IqServer>,
) -> Result<()> {
    let messages = if let Some(server) = server {
        let (messages, input) = new_nocopy_stream();
        graph.add(Box::new(
            // Sparsnäs normally transmits one packet every 15 seconds.
            IqStreamStringSink::builder(input, server, "decoded", 1.0 / 15.0)
                .blocking(false)
                .build()?,
        ));
        Some(messages)
    } else {
        None
    };
    let decode = Box::new(Decode::new(
        prev,
        opt.sensor_id,
        opt.output.clone(),
        messages,
    ));
    graph.add(decode);
    Ok(())
}

fn input_samples(
    graph: &mut (impl GraphRunner + ?Sized),
    opt: &Opt,
    server: Option<&IqServer>,
) -> anyhow::Result<(ReadStream<Complex>, u32)> {
    let mut sample_rate = opt.sample_rate;
    // Source.
    let src = {
        if let Some(connect) = &opt.iq_connect {
            let (source, samples, status) = IqStreamSource::<Complex>::connect(
                connect.clone(),
                opt.iq_source.clone(),
                StreamOptions {
                    loss_policy: proto::LossPolicy::AllowGaps,
                    ..Default::default()
                },
            )?;
            sample_rate = iq_sample_rate(status.description().sample_rate_hz)?;
            validate_tune_offset(sample_rate, opt.tune_offset)?;
            graph.add(Box::new(source));
            translate_rtl(graph, samples, sample_rate, opt.tune_offset)?
        } else if let Some(connect) = &opt.connect {
            if opt.read.is_some() {
                return Err(anyhow::Error::msg("-c and -r can't be combined"));
            }
            let sa: SocketAddr = connect.parse()?;
            let host = format!("{}", sa.ip());
            let port = sa.port();
            println!("Connecting to host {host} port {port}");
            blockchain![graph, prev, TcpSource::<Complex>::new(sa)?]
        } else if let Some(read) = &opt.read {
            if opt.rtlsdr {
                blockchain![
                    graph,
                    prev,
                    FileSource::<u8>::new(read)?,
                    RtlSdrDecode::new(prev)?,
                ]
            } else {
                blockchain![graph, prev, FileSource::<Complex>::new(read)?]
            }
        } else if opt.rtlsdr {
            let frequency = rtl_tune_frequency(opt)?;
            let samples = blockchain![
                graph,
                prev,
                RtlSdrSource::new(frequency, opt.sample_rate, f32_to_i32(opt.gain)?)?,
                RtlSdrDecode::new(prev)?,
            ];
            live_rtl_samples(graph, samples, opt, server)?
        } else {
            return Err(anyhow::Error::msg(
                "Need to provide either -r, -c, --rtlsdr, or --iq-connect",
            ));
        }
    };

    Ok((src, sample_rate))
}

/// Create the graph to decode sparsnäs, retaining the returned IQ listener
/// until graph execution ends.
///
/// # Errors
///
/// If given incompatible cmdline options, if the IQ listener cannot start,
/// or if an `IqStream` connection or its advertised sample rate is invalid.
pub fn create_graph(
    graph: &mut (impl GraphRunner + ?Sized),
    opt: &Opt,
) -> anyhow::Result<Option<IqListener>> {
    if let Some(prom) = &opt.prometheus {
        let prom = prom.clone();
        let wh = opt.where_.clone().expect("Can't happen: clap promised!");
        let serial = opt.sensor_id;
        std::thread::Builder::new()
            .name("prometheus-pusher".to_string())
            .spawn(move || {
                loop {
                    if let Err(err) = push_metrics(&prom, &wh, serial) {
                        eprintln!("Failed to push prometheus metrics: {err}");
                    }
                    std::thread::sleep(std::time::Duration::from_mins(1));
                }
            })
            .expect("spawn prometheus pusher thread");
    }
    let server = opt.iq_listen.map(|_| IqServer::new());
    let (src, sample_rate) = input_samples(graph, opt, server.as_ref())?;

    #[allow(clippy::cast_precision_loss)]
    let samp_rate = sample_rate as f32;
    let samp_rate_2 = 200_000.0;
    let baud = 38383.5;

    let prev = src;
    // Resample.
    let prev = blockchain![
        graph,
        prev,
        // TODO: doing filtering in multiple steps, with a decimating FIR filter, would
        // probably be more CPU efficient.
        FftFilter::new(
            prev,
            rustradio::fir::low_pass_complex(samp_rate, 50000.0, 10000.0, &WindowType::Hamming)?
        )?,
        RationalResampler::new(prev, f32_to_usize(samp_rate_2)?, f32_to_usize(samp_rate)?)?,
    ];
    let prev = iq_tap(
        graph,
        prev,
        server.as_ref(),
        "filtered",
        f64::from(samp_rate_2),
    )?;
    let prev = blockchain![
        graph,
        prev,
        QuadratureDemod::new(prev, 1.0)?,
        AddConst::new(prev, opt.offset)?,
    ];
    let prev = iq_tap(
        graph,
        prev,
        server.as_ref(),
        "demodulated",
        f64::from(samp_rate_2),
    )?;
    let prev = blockchain![
        graph,
        prev,
        ZeroCrossing::new(prev, samp_rate_2 / baud, 0.1)?,
        BinarySlicer::new(prev)?,
    ];

    add_decoder(graph, prev, opt, server.as_ref())?;
    server
        .zip(opt.iq_listen)
        .map(|(server, address)| IqListener::start(server, address, graph.cancel_token()))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::time::Duration;

    #[test]
    fn decoded_stream_matches_csv_record() -> anyhow::Result<()> {
        use rustradio::blocks::VectorSource;
        let packet = [
            0xaa, 0xaa, 0xd2, 0x01, 0x11, 0xa1, 0x38, 0x07, 0x0e, 0xa2, 0xde, 0x29, 0xe6, 0x8b,
            0x1a, 0xfd, 0x74, 0x47, 0xcf, 0xf2, 0x14, 0x80, 0x23, 0x7b,
        ];
        let mut bits: Vec<u8> = packet
            .into_iter()
            .flat_map(|byte| (0..8).rev().map(move |bit| (byte >> bit) & 1))
            .collect();
        bits.push(0);
        let (mut source, input) = VectorSource::new(bits)?;
        source.work()?;
        let file = tempfile::NamedTempFile::new()?;
        let (messages, output) = new_nocopy_stream();
        let mut decoder = Decode::new(
            input,
            576_929,
            file.path().to_str().unwrap().into(),
            Some(messages),
        );
        decoder.work()?;
        let (message, tags) = output.pop().expect("decoded message");
        assert_eq!(
            std::fs::read_to_string(file.path())?,
            format!("{message}\n")
        );
        assert!(message.ends_with(",17592,330.560,20.674,100,OK"));
        assert_eq!(tags, []);
        assert!(output.pop().is_none());
        Ok(())
    }

    #[test]
    fn verbosity_flag_accepts_log_levels() -> anyhow::Result<()> {
        let default = Opt::try_parse_from(["sparslog", "--serial", "123456"])?;
        assert_eq!(default.verbose, 0);
        let debug = Opt::try_parse_from(["sparslog", "--serial", "123456", "-v", "debug"])?;
        assert_eq!(debug.verbose, 3);
        assert!(Opt::try_parse_from(["sparslog", "--serial", "123456", "-v", "3"]).is_err());
        Ok(())
    }

    #[test]
    fn frequency_flags_accept_units() -> anyhow::Result<()> {
        for (frequency, rate, offset) in [
            ("868M", "1.024M", "100k"),
            ("0.868g", "1024K", "0.1m"),
            ("868_000_000", "1_024_000", "100_000"),
        ] {
            let opt = Opt::try_parse_from([
                "sparslog",
                "--serial",
                "123456",
                "--freq",
                frequency,
                "--sample-rate",
                rate,
                "--tune-offset",
                offset,
            ])?;
            assert_eq!(opt.freq, 868_000_000);
            assert_eq!(opt.sample_rate, 1_024_000);
            assert_eq!(opt.tune_offset, 100_000);
        }
        let negative =
            Opt::try_parse_from(["sparslog", "--serial", "123456", "--tune-offset", "-100k"])?;
        assert_eq!(negative.tune_offset, -100_000);
        for flag in ["--freq", "--sample-rate", "--tune-offset"] {
            for invalid in ["invalid", "NaN", "inf", "0.1", "100Gk", "1e100"] {
                assert!(
                    Opt::try_parse_from(["sparslog", "--serial", "123456", flag, invalid,])
                        .is_err()
                );
            }
        }
        for (flag, invalid) in [
            ("--freq", "-1k"),
            ("--sample-rate", "-1k"),
            ("--sample-rate", "4294967296"),
            ("--freq", "18446744073709551616"),
            ("--tune-offset", "9223372036854775808"),
        ] {
            assert!(
                Opt::try_parse_from(["sparslog", "--serial", "123456", flag, invalid,]).is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn rtl_tuning_options() -> anyhow::Result<()> {
        let defaults = Opt::try_parse_from(["sparslog", "--serial", "123456", "--rtlsdr"])?;
        assert_eq!(defaults.tune_offset, 100_000);
        assert_eq!(rtl_tune_frequency(&defaults)?, 868_100_000);
        for (offset, expected) in [("-100000", 867_900_000), ("0", 868_000_000)] {
            let opt = Opt::try_parse_from([
                "sparslog",
                "--serial",
                "123456",
                "--rtlsdr",
                "--tune-offset",
                offset,
            ])?;
            assert_eq!(rtl_tune_frequency(&opt)?, expected);
        }
        let mut opt = defaults;
        for offset in [452_001, -452_001, i64::MAX, i64::MIN] {
            opt.tune_offset = offset;
            assert!(rtl_tune_frequency(&opt).is_err());
        }
        opt.tune_offset = -100_000;
        opt.freq = 99_999;
        assert!(rtl_tune_frequency(&opt).is_err());
        opt.tune_offset = 100_000;
        opt.freq = u64::from(u32::MAX);
        assert!(rtl_tune_frequency(&opt).is_err());
        opt.freq = u64::MAX;
        assert!(rtl_tune_frequency(&opt).is_err());
        opt.freq = 868_000_000;
        opt.sample_rate = 0;
        assert!(rtl_tune_frequency(&opt).is_err());
        Ok(())
    }

    #[tokio::test]
    #[allow(clippy::cast_precision_loss)]
    async fn rtl_translation_centers_channel_and_rejects_dc() -> anyhow::Result<()> {
        use rustradio::blocks::{VectorSink, VectorSource};
        const RATE: u32 = 1_024_000;
        for offset in [-100_000, 100_000] {
            for desired_signal in [false, true] {
                let samples: Vec<_> = (0..8192)
                    .map(|index| {
                        let phase = -2.0 * std::f32::consts::PI * offset as f32 * index as f32
                            / RATE as f32;
                        let signal = if desired_signal {
                            Complex::from_polar(1.0, phase)
                        } else {
                            Complex::default()
                        };
                        signal + Complex::new(1.0, 0.0)
                    })
                    .collect();
                let mut graph = rustradio::graph::Graph::new();
                let (source, samples) = VectorSource::new(samples)?;
                graph.add(Box::new(source));
                let centered = translate_rtl(&mut graph, samples, RATE, offset)?;
                let (filter, output) = FftFilter::new(
                    centered,
                    rustradio::fir::low_pass_complex(
                        RATE as f32,
                        50_000.0,
                        10_000.0,
                        &WindowType::Hamming,
                    )?,
                )?;
                graph.add(Box::new(filter));
                let sink = VectorSink::new(output, 16_384);
                let captured = sink.hook();
                graph.add(Box::new(sink));
                run_sync_graph(graph).await?;
                let captured = captured.data();
                let settled = &captured.samples()[1024..];
                assert!(settled.len() > 4096);
                let average = settled.iter().copied().sum::<Complex>() / settled.len() as f32;
                let expected = if desired_signal { 1.0 } else { 0.0 };
                assert!((average.norm() - expected).abs() < 0.01);
                // The passed channel must be at DC, not just inside the passband.
                assert!(
                    settled
                        .iter()
                        .all(|sample| (*sample - average).norm() < 0.02)
                );
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn rtl_recordings_ignore_tuning_offset() -> anyhow::Result<()> {
        let (_file, mut opt) = iq_fixture()?;
        opt.iq_listen = None;
        // This would fail validation or try to open hardware if applied to files.
        opt.tune_offset = i64::MIN;
        for raw_rtl in [false, true] {
            opt.rtlsdr = raw_rtl;
            let mut graph = rustradio::graph::Graph::new();
            assert!(create_graph(&mut graph, &opt)?.is_none());
            run_sync_graph(graph).await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn rtl_translation_preserves_tags_and_zero_offset() -> anyhow::Result<()> {
        use rustradio::blocks::{VectorSink, VectorSource};
        use rustradio::stream::{Tag, TagValue};
        for offset in [0, 100_000] {
            let tag = Tag::new(7, "test-position", TagValue::U64(123));
            let (source, samples) = VectorSource::builder(vec![Complex::new(1.0, 0.0); 64])
                .tags(std::slice::from_ref(&tag))
                .build()?;
            let mut graph = rustradio::graph::Graph::new();
            graph.add(Box::new(source));
            let output = translate_rtl(&mut graph, samples, 1_024_000, offset)?;
            let sink = VectorSink::new(output, 64);
            let captured = sink.hook();
            graph.add(Box::new(sink));
            run_sync_graph(graph).await?;
            let captured = captured.data();
            assert_eq!(captured.samples().len(), 64);
            assert!(captured.tags().contains(&tag));
            if offset == 0 {
                assert!(
                    captured
                        .samples()
                        .iter()
                        .all(|sample| *sample == Complex::new(1.0, 0.0))
                );
            }
        }
        Ok(())
    }

    fn iq_fixture() -> anyhow::Result<(tempfile::NamedTempFile, Opt)> {
        let mut file = tempfile::NamedTempFile::new()?;
        file.write_all(&vec![0; 32_768 * 8])?;
        let opt = Opt::try_parse_from([
            "sparslog",
            "--serial",
            "123456",
            "--read",
            file.path().to_str().unwrap(),
            "--iq-listen",
            "127.0.0.1:0",
        ])?;
        Ok((file, opt))
    }

    fn iq_options(address: SocketAddr) -> anyhow::Result<Opt> {
        Ok(Opt::try_parse_from([
            "sparslog",
            "--serial",
            "123456",
            "--iq-connect",
            &format!("http://{address}"),
        ])?)
    }

    #[test]
    fn iq_input_options() -> anyhow::Result<()> {
        let address = "127.0.0.1:9000".parse()?;
        let opt = iq_options(address)?;
        assert_eq!(opt.iq_source, "raw");
        assert_eq!(opt.tune_offset, 100_000);
        for extra in [
            vec!["--read", "recording.c32"],
            vec!["--connect", "127.0.0.1:9001"],
            vec!["--rtlsdr"],
        ] {
            let mut args = vec![
                "sparslog",
                "--serial",
                "123456",
                "--iq-connect",
                "http://127.0.0.1:9000",
            ];
            args.extend(extra);
            assert!(Opt::try_parse_from(args).is_err());
        }
        assert!(
            Opt::try_parse_from(["sparslog", "--serial", "123456", "--iq-source", "filtered",])
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn iq_sample_rate_validation() -> anyhow::Result<()> {
        assert_eq!(iq_sample_rate(200_000.0)?, 200_000);
        assert_eq!(iq_sample_rate(f64::from(u32::MAX))?, u32::MAX);
        for invalid in [
            0.0,
            -1.0,
            200_000.5,
            f64::NAN,
            f64::INFINITY,
            4_294_967_296.0,
        ] {
            assert!(iq_sample_rate(invalid).is_err());
        }
        Ok(())
    }

    #[tokio::test]
    async fn live_raw_tap_precedes_translation() -> anyhow::Result<()> {
        use rustradio::blocks::{VectorSink, VectorSource};
        use rustradio::stream::{Tag, TagValue};
        let opt = Opt::try_parse_from(["sparslog", "--serial", "123456", "--rtlsdr"])?;
        let server = IqServer::new();
        let mut graph = rustradio::graph::Graph::new();
        // Model the complex output of RtlSdrDecode, including a sample tag.
        let values = vec![Complex::new(0.5, -0.25); 64];
        let tag = Tag::new(7, "test-position", TagValue::U64(123));
        let (source, samples) = VectorSource::builder(values.clone())
            .tags(std::slice::from_ref(&tag))
            .build()?;
        graph.add(Box::new(source));
        let centered = live_rtl_samples(&mut graph, samples, &opt, Some(&server))?;
        let sink = VectorSink::new(centered, 64);
        let translated = sink.hook();
        graph.add(Box::new(sink));
        let listener = IqListener::start(server, "127.0.0.1:0".parse()?, graph.cancel_token())?;
        let (source, samples, status) = IqStreamSource::<Complex>::connect(
            format!("http://{}", listener.local_addr()),
            "raw",
            StreamOptions {
                loss_policy: proto::LossPolicy::AllowGaps,
                ..Default::default()
            },
        )?;
        assert_eq!(
            status.description().sample_rate_hz,
            f64::from(opt.sample_rate)
        );
        assert_eq!(status.description().source_id, "raw");
        let sink = VectorSink::new(samples, 64);
        let raw = sink.hook();
        let mut receiver = rustradio::graph::Graph::new();
        receiver.add(Box::new(source));
        receiver.add(Box::new(sink));
        let (sent, downloaded) = tokio::join!(run_sync_graph(graph), run_sync_graph(receiver));
        sent?;
        downloaded?;
        assert_eq!(raw.data().samples(), values);
        assert!(raw.data().tags().contains(&tag));
        assert_ne!(translated.data().samples(), values);
        assert!(translated.data().tags().contains(&tag));
        listener.shutdown()
    }

    #[tokio::test]
    async fn recordings_do_not_expose_raw() -> anyhow::Result<()> {
        for rtl in [false, true] {
            let (_file, mut opt) = iq_fixture()?;
            opt.rtlsdr = rtl;
            let mut graph = rustradio::graph::Graph::new();
            let listener = create_graph(&mut graph, &opt)?.unwrap();
            assert!(
                IqStreamSource::<Complex>::connect(
                    format!("http://{}", listener.local_addr()),
                    "raw",
                    StreamOptions {
                        loss_policy: proto::LossPolicy::AllowGaps,
                        ..Default::default()
                    },
                )
                .is_err()
            );
            run_sync_graph(graph).await?;
            listener.shutdown()?;
        }
        Ok(())
    }

    fn iq_filtered_observer(
        listener: &IqListener,
    ) -> anyhow::Result<(
        rustradio::graph::Graph,
        rustradio::vector_sink::Hook<Complex>,
    )> {
        use rustradio::blocks::VectorSink;
        let uri = format!("http://{}", listener.local_addr());
        let options = StreamOptions {
            loss_policy: proto::LossPolicy::AllowGaps,
            ..Default::default()
        };
        assert!(IqStreamSource::<Complex>::connect(uri.clone(), "raw", options.clone()).is_err());
        let (source, samples, _) = IqStreamSource::<Complex>::connect(uri, "filtered", options)?;
        let sink = VectorSink::new(samples, 16_384);
        let captured = sink.hook();
        let mut observer = rustradio::graph::Graph::new();
        observer.add(Box::new(source));
        observer.add(Box::new(sink));
        Ok((observer, captured))
    }

    #[tokio::test]
    #[allow(clippy::cast_precision_loss)]
    async fn iq_input_all_executors() -> anyhow::Result<()> {
        use rustradio::blocks::VectorSource;
        for mode in 0..3 {
            for offset in [0, 100_000] {
                let server = IqServer::new();
                let mut sender = rustradio::graph::Graph::new();
                let values: Vec<_> = (0..8192)
                    .map(|index| {
                        Complex::from_polar(
                            1.0,
                            -2.0 * std::f32::consts::PI * offset as f32 * index as f32 / 400_000.0,
                        )
                    })
                    .collect();
                let (source, samples) = VectorSource::new(values)?;
                sender.add(Box::new(source));
                sender.add(Box::new(
                    IqStreamSink::builder(samples, &server, "channel", 400_000.0)
                        .blocking(false)
                        .build()?,
                ));
                let upstream =
                    IqListener::start(server, "127.0.0.1:0".parse()?, sender.cancel_token())?;
                let mut opt = iq_options(upstream.local_addr())?;
                opt.iq_source = "channel".into();
                opt.tune_offset = offset;
                // Finite IqStream sinks currently complete reliably only in Graph.
                // MTGraph/AsyncGraph can drop a sink on upstream closure before
                // it sends End. Test input on every executor and inspect output
                // samples using Graph, without working around the upstream bug.
                opt.iq_listen = if mode == 0 {
                    Some("127.0.0.1:0".parse()?)
                } else {
                    None
                };
                // Deliberately differs from the advertised rate.
                assert_ne!(opt.sample_rate, 400_000);
                let mut sync_graph = rustradio::graph::Graph::new();
                let mut mt_graph = rustradio::mtgraph::MTGraph::new();
                let mut async_graph = rustradio::agraph::AsyncGraph::new();
                let graph: &mut dyn GraphRunner = match mode {
                    1 => &mut mt_graph,
                    2 => &mut async_graph,
                    _ => &mut sync_graph,
                };
                let downstream = create_graph(graph, &opt)?;
                let (observer, captured) = if let Some(downstream) = &downstream {
                    let (observer, captured) = iq_filtered_observer(downstream)?;
                    (observer, Some(captured))
                } else {
                    (rustradio::graph::Graph::new(), None)
                };
                let decoding = async {
                    if mode == 2 {
                        let cancel = async_graph.cancel_token();
                        let result =
                            tokio::time::timeout(Duration::from_secs(5), async_graph.run_async())
                                .await;
                        cancel.cancel();
                        result??;
                    } else if mode == 1 {
                        run_sync_graph(mt_graph).await?;
                    } else {
                        run_sync_graph(sync_graph).await?;
                    }
                    Ok::<_, anyhow::Error>(())
                };
                let (sent, decoded, downloaded) =
                    tokio::join!(run_sync_graph(sender), decoding, run_sync_graph(observer));
                sent?;
                decoded?;
                downloaded?;
                if let Some(captured) = captured {
                    let data = captured.data();
                    assert!(data.samples().len() > 2048);
                    assert!(data.samples().len() < 8192);
                    assert!(
                        data.samples()[1024..]
                            .iter()
                            .all(|sample| (*sample - Complex::new(1.0, 0.0)).norm() < 0.02)
                    );
                }
                if let Some(downstream) = downstream {
                    downstream.shutdown()?;
                }
                upstream.shutdown()?;
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn iq_input_failures_and_cancellation() -> anyhow::Result<()> {
        use rustradio::stream::new_stream;
        let server = IqServer::new();
        let mut streams = Vec::new();
        for (name, rate) in [
            ("raw", 200_000.0),
            ("fractional", 200_000.5),
            ("offset", 200_000.0),
            ("cancel", 200_000.0),
        ] {
            let (writer, input) = new_stream::<Complex>()?;
            let sink = IqStreamSink::builder(input, &server, name, rate)
                .blocking(false)
                .build()?;
            streams.push((writer, sink));
        }
        let (_real_writer, input) = new_stream::<f32>()?;
        let _real_sink = IqStreamSink::builder(input, &server, "real", 200_000.0)
            .blocking(false)
            .build()?;
        let upstream = IqListener::start(
            server,
            "127.0.0.1:0".parse()?,
            rustradio::graph::Graph::new().cancel_token(),
        )?;
        let mut opt = iq_options(upstream.local_addr())?;
        for (resource, offset, expected) in [
            ("missing", 0, "unknown source"),
            ("real", 0, "unsupported encoding"),
            ("fractional", 0, "whole number"),
            ("offset", 100_000, "half the sample rate"),
        ] {
            opt.iq_source = resource.into();
            opt.tune_offset = offset;
            let mut graph = rustradio::graph::Graph::new();
            let error = create_graph(&mut graph, &opt).err().expect("invalid input");
            assert!(error.to_string().contains(expected), "{error}");
        }
        opt.iq_source = "cancel".into();
        opt.tune_offset = 0;
        // The idle source has no samples; cancellation must still stop the graph.
        let mut graph = rustradio::graph::Graph::new();
        assert!(create_graph(&mut graph, &opt)?.is_none());
        let cancel = graph.cancel_token();
        let running = tokio::spawn(run_sync_graph(graph));
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
        running.await??;
        // A server disappearing after connection must fail graph execution.
        opt.iq_source = "raw".into();
        let mut graph = rustradio::graph::Graph::new();
        assert!(create_graph(&mut graph, &opt)?.is_none());
        upstream.shutdown()?;
        assert!(run_sync_graph(graph).await.is_err());
        let mut graph = rustradio::graph::Graph::new();
        assert!(create_graph(&mut graph, &opt).is_err());
        Ok(())
    }

    async fn run_sync_graph(mut graph: impl GraphRunner + Send + 'static) -> anyhow::Result<()> {
        let cancel = graph.cancel_token();
        let task = tokio::task::spawn_blocking(move || graph.run());
        let result = tokio::time::timeout(Duration::from_secs(5), task).await;
        cancel.cancel();
        result???;
        Ok(())
    }

    #[tokio::test]
    async fn iq_without_clients_all_executors() -> anyhow::Result<()> {
        let (_file, mut opt) = iq_fixture()?;
        let mut graph = rustradio::graph::Graph::new();
        let listener = create_graph(&mut graph, &opt)?.unwrap();
        let address = listener.local_addr();
        run_sync_graph(graph).await?;
        listener.shutdown()?;
        // Shutdown joins the server thread and releases the listening socket.
        let _rebound = std::net::TcpListener::bind(address)?;

        let mut graph = rustradio::mtgraph::MTGraph::new();
        let listener = create_graph(&mut graph, &opt)?.unwrap();
        run_sync_graph(graph).await?;
        listener.shutdown()?;

        let mut graph = rustradio::agraph::AsyncGraph::new();
        let listener = create_graph(&mut graph, &opt)?.unwrap();
        let cancel = graph.cancel_token();
        let result = tokio::time::timeout(Duration::from_secs(5), graph.run_async()).await;
        cancel.cancel();
        result??;
        listener.shutdown()?;

        opt.iq_listen = None;
        let mut graph = rustradio::graph::Graph::new();
        assert!(create_graph(&mut graph, &opt)?.is_none());
        run_sync_graph(graph).await
    }

    #[test]
    fn iq_bind_failure() -> anyhow::Result<()> {
        let (_file, mut opt) = iq_fixture()?;
        let occupied = std::net::TcpListener::bind("127.0.0.1:0")?;
        opt.iq_listen = Some(occupied.local_addr()?);
        let mut graph = rustradio::graph::Graph::new();
        assert!(create_graph(&mut graph, &opt).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn iq_stream_metadata_and_samples() -> anyhow::Result<()> {
        check_iq_stream(false).await
    }

    #[tokio::test]
    async fn iq_slow_client_reports_gaps() -> anyhow::Result<()> {
        check_iq_stream(true).await
    }

    async fn check_iq_stream(slow: bool) -> anyhow::Result<()> {
        use rustradio::blocks::{IqStreamSource, VectorSink};
        use rustradio::iq_stream::{SourceStatus, StreamOptions, proto};

        let (_file, opt) = iq_fixture()?;
        let mut graph = rustradio::graph::Graph::new();
        let listener = create_graph(&mut graph, &opt)?.unwrap();
        let mut options = StreamOptions {
            loss_policy: proto::LossPolicy::AllowGaps,
            ..Default::default()
        };
        if slow {
            // Leave room for a sample and its stream tags while forcing gaps.
            options.limits.max_frame_bytes = 512;
            options.limits.max_in_flight_frames = 1;
        }
        let (source, input, status) = IqStreamSource::<Complex>::connect_async(
            format!("http://{}", listener.local_addr()),
            "filtered",
            options.clone(),
        )
        .await?;
        assert_eq!(status.description().sample_rate_hz, 200_000.0);
        assert_eq!(status.description().source_id, "filtered");
        let sink = VectorSink::new(input, 32_768);
        let samples = sink.hook();
        let mut receiver = rustradio::graph::Graph::new();
        receiver.add(Box::new(source));
        receiver.add(Box::new(sink));
        let (source, input, demodulated_status) = IqStreamSource::<f32>::connect_async(
            format!("http://{}", listener.local_addr()),
            "demodulated",
            options,
        )
        .await?;
        assert_eq!(demodulated_status.description().sample_rate_hz, 200_000.0);
        assert_eq!(demodulated_status.description().source_id, "demodulated");
        let sink = VectorSink::new(input, 32_768);
        let demodulated_samples = sink.hook();
        receiver.add(Box::new(source));
        receiver.add(Box::new(sink));
        let receiving = async {
            if slow {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            run_sync_graph(receiver).await
        };
        let (sending, receiving) = tokio::join!(run_sync_graph(graph), receiving);
        sending?;
        receiving?;
        assert_eq!(status.status(), SourceStatus::Complete);
        assert_eq!(demodulated_status.status(), SourceStatus::Complete);
        if slow {
            assert!(status.lost_samples() > 0);
            assert!(demodulated_status.lost_samples() > 0);
        }
        assert_ne!(demodulated_samples.data().samples(), &[] as &[f32]);
        assert!(
            demodulated_samples
                .data()
                .samples()
                .iter()
                .all(|s| s.to_bits() == opt.offset.to_bits())
        );
        assert_ne!(samples.data().samples(), []);
        assert!(
            samples
                .data()
                .samples()
                .iter()
                .all(|s| *s == Complex::new(0.0, 0.0))
        );
        listener.shutdown()
    }

    #[tokio::test]
    async fn iq_disconnect_and_cancellation() -> anyhow::Result<()> {
        use rustradio::blocks::IqStreamSource;
        use rustradio::iq_stream::{StreamOptions, proto};

        let (_file, mut opt) = iq_fixture()?;
        let mut graph = rustradio::graph::Graph::new();
        let listener = create_graph(&mut graph, &opt)?.unwrap();
        let (source, input, _) = IqStreamSource::<Complex>::connect_async(
            format!("http://{}", listener.local_addr()),
            "filtered",
            StreamOptions {
                loss_policy: proto::LossPolicy::AllowGaps,
                ..Default::default()
            },
        )
        .await?;
        let sending = tokio::spawn(run_sync_graph(graph));
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(source);
        drop(input);
        sending.await??;
        listener.shutdown()?;

        opt.read = Some("/dev/zero".into());
        let mut graph = rustradio::graph::Graph::new();
        let listener = create_graph(&mut graph, &opt)?.unwrap();
        let address = listener.local_addr();
        let cancel = graph.cancel_token();
        let sending = tokio::spawn(run_sync_graph(graph));
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
        sending.await??;
        drop(listener);
        let _rebound = std::net::TcpListener::bind(address)?;
        Ok(())
    }

    #[test]
    fn convert_i32() -> anyhow::Result<()> {
        for (i, o) in [
            (0.0, 0),
            (-1.0, -1),
            (-1.1, -1),
            (-1.9, -1),
            (1.0, 1),
            (1.1, 1),
            (1.9, 1),
        ] {
            assert_eq!(f32_to_i32(i)?, o);
        }
        Ok(())
    }

    #[test]
    fn convert_usize() -> anyhow::Result<()> {
        for (i, o) in [
            (0.0, Some(0)),
            (-1.0, None),
            (-1.1, None),
            (1.0, Some(1)),
            (1.1, Some(1)),
            (1.9, Some(1)),
        ] {
            match o {
                None => {
                    assert!(f32_to_usize(i).is_err());
                }
                Some(v) => {
                    assert_eq!(f32_to_usize(i)?, v);
                }
            }
        }
        Ok(())
    }

    #[test]
    fn decode() {
        let packet = vec![
            0x11, 0xa1, 0x38, 0x07, 0x0e, 0xa2, 0xde, 0x29, 0xe6, 0x8b, 0x1a, 0xfd, 0x74, 0x47,
            0xcf, 0xf2, 0x14, 0x80, 0x23, 0x7b,
        ];
        let got = parsepacket(&packet, 576_929);
        let want = ",17592,330.560,20.674,100,OK";
        assert!(got.ends_with(want), "got: {got}, want {want}");

        // With one bitflip.
        let packet = vec![
            0x11, 0xa1, 0x38, 0x07, 0x0e, 0xa2, 0xde, 0x29, 0xe7, 0x8b, 0x1a, 0xfd, 0x74, 0x47,
            0xcf, 0xf2, 0x14, 0x80, 0x23, 0x7b,
        ];
        let got = parsepacket(&packet, 576_929);
        let want = ",17592,330.560,20.674,100,OK";
        assert!(got.ends_with(want), "got: {got}, want {want}");

        // With two bitflips.
        let packet = vec![
            0x11, 0xa1, 0x38, 0x07, 0x0e, 0xa2, 0xdf, 0x29, 0xe6, 0x8b, 0x1a, 0xfd, 0x74, 0x47,
            0xcf, 0xf2, 0x14, 0x80, 0x23, 0x7a,
        ];
        let got = parsepacket(&packet, 576_929);
        let want = ",17592,330.560,20.674,100,BAD";
        assert!(got.ends_with(want), "got: {got}, want {want}");
    }
}
