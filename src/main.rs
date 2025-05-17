use anyhow::Result;

use clap::Parser;
use log::debug;

use std::collections::VecDeque;
use std::io::Write;
use std::net::SocketAddr;

use rustradio::block::{Block, BlockRet};
use rustradio::blocks::*;
use rustradio::graph::GraphRunner;
use rustradio::stream::ReadStream;
use rustradio::window::WindowType;
use rustradio::{Complex, Error};

#[derive(clap::Parser, Debug)]
#[command(version, about)]
struct Opt {
    #[arg(short, long = "serial")]
    sensor_id: u32,

    #[arg(short, long = "output", default_value = "sparslog.csv")]
    output: String,

    #[arg(short, long = "connect")]
    connect: Option<String>,

    #[arg(short, long = "read")]
    read: Option<String>,

    #[arg(long = "rtlsdr")]
    rtlsdr: bool,

    #[arg(short, default_value = "0")]
    verbose: usize,

    #[arg(long = "gain", default_value = "30")]
    gain: f32,

    #[arg(long = "sample_rate", default_value = "1024000")]
    sample_rate: u32,

    #[arg(long = "freq", default_value = "868000000")]
    freq: u64,

    #[arg(long = "offset", default_value = "0.4")]
    offset: f32,

    /// Run multithreaded.
    #[arg(long)]
    multithread: bool,
}

#[derive(rustradio::rustradio_macros::Block)]
#[rustradio(new, custom_name)]
struct Decode {
    #[rustradio(in)]
    src: ReadStream<u8>,
    sensor_id: u32,
    output: String,

    #[rustradio(default)]
    history: VecDeque<u8>,
}

impl Decode {
    fn custom_name(&self) -> &'static str {
        "Sparsnäs decoder"
    }
}

fn bits2byte(data: &[u8]) -> u8 {
    assert!(data.len() == 8);
    (data[0] << 7)
        | (data[1] << 6)
        | (data[2] << 5)
        | (data[3] << 4)
        | (data[4] << 3)
        | (data[5] << 2)
        | (data[6] << 1)
        | data[7]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decode() {
        let packet = vec![
            0x11, 0xa1, 0x38, 0x07, 0x0e, 0xa2, 0xde, 0x29, 0xe6, 0x8b, 0x1a, 0xfd, 0x74, 0x47,
            0xcf, 0xf2, 0x14, 0x80, 0x23, 0x7b,
        ];
        assert!(parsepacket(&packet, 576929).ends_with(",17592,330.55954,20.674,100,OK"));

        // With one bitflip.
        let packet = vec![
            0x11, 0xa1, 0x38, 0x07, 0x0e, 0xa2, 0xde, 0x29, 0xe7, 0x8b, 0x1a, 0xfd, 0x74, 0x47,
            0xcf, 0xf2, 0x14, 0x80, 0x23, 0x7b,
        ];
        assert!(parsepacket(&packet, 576929).ends_with(",17592,330.55954,20.674,100,OK"));

        // With two bitflips.
        let packet = vec![
            0x11, 0xa1, 0x38, 0x07, 0x0e, 0xa2, 0xdf, 0x29, 0xe6, 0x8b, 0x1a, 0xfd, 0x74, 0x47,
            0xcf, 0xf2, 0x14, 0x80, 0x23, 0x7a,
        ];
        assert!(parsepacket(&packet, 576929).ends_with(",17592,330.55954,20.674,100,BAD"));
    }
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
    let crc = ((packet[packet.len() - 2] as u16) << 8) | packet[packet.len() - 1] as u16;
    if crc16(&packet[..packet.len() - 2], crc) {
        return packet.to_vec();
    }
    for i in 0..(packet.len() * 8) {
        let mut test = packet.to_vec();
        let bit = 1 << (i % 8);
        test[i / 8] ^= bit;
        if crc16(&test[..packet.len() - 2], crc) {
            return test.to_vec();
        }
    }
    packet.to_vec()
}

fn parsepacket(packet: &[u8], sensor_id: u32) -> String {
    assert!(packet.len() == 20);
    //let sensor = packet[0];
    //let app = packet[1];
    let packet = fix_packet(packet);

    // This is the correct packet.
    println!("Packet: {:02x?}", packet);

    let sensor_id_sub = {
        let magic = 0x5D38E8CB;
        if sensor_id >= magic {
            sensor_id - magic
        } else {
            4294967295 - (magic - sensor_id - 1)
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
    let crc = ((packet[packet.len() - 2] as u16) << 8) | packet[packet.len() - 1] as u16;
    let crc_ok = crc16(&packet[..packet.len() - 2], crc);

    let seq = ((dec[4] as u16) << 8) | (dec[5] as u16);
    let effect = ((dec[6] as u16) << 8) | (dec[7] as u16);
    let pulse = ((dec[8] as u32) << 24)
        | ((dec[9] as u32) << 16)
        | ((dec[10] as u32) << 8)
        | dec[11] as u32;
    let kwh = (pulse / 1000) as f32 + ((pulse % 1000) as f32) / 1000.0;
    let battery = dec[12];
    let watt = 3600.0 * 1024.0 / (effect as f32);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("Time went backwards")
        .as_secs();

    format!(
        "{},{seq},{watt},{kwh},{battery},{}",
        now,
        if crc_ok { "OK" } else { "BAD" }
    )
}

impl Block for Decode {
    fn work(&mut self) -> Result<BlockRet, Error> {
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
                println!("{}", parsed);
            }
        }
        self.history
            .drain(0..(self.history.len() - packet_bits_len));
        Ok(BlockRet::Again)
    }
}

macro_rules! add_block {
    ($g:ident, $cons:expr) => {{
        let (block, out) = $cons;
        $g.add(Box::new(block));
        out
    }};
}

fn main() -> Result<()> {
    println!("Sparslog");
    let opt = Opt::parse();
    stderrlog::new()
        .module(module_path!())
        .module("rustradio")
        .quiet(false)
        .verbosity(opt.verbose)
        .timestamp(stderrlog::Timestamp::Second)
        .init()
        .unwrap();

    let mut graph: Box<dyn GraphRunner> = if opt.multithread {
        Box::new(rustradio::mtgraph::MTGraph::new())
    } else {
        Box::new(rustradio::graph::Graph::new())
    };

    // Source.
    let src = {
        if let Some(connect) = opt.connect {
            assert!(opt.read.is_none(), "-c and -r can't both be used");
            let sa: SocketAddr = connect.parse()?;
            let host = format!("{}", sa.ip());
            let port = sa.port();
            println!("Connecting to host {} port {}", host, port);
            let (block, out) = TcpSource::<Complex>::new(&host, port)?;
            graph.add(Box::new(block));
            out
        } else if let Some(read) = opt.read {
            if opt.rtlsdr {
                let (src, out) = FileSource::<u8>::new(&read)?;
                let (rtlsdr, out) = RtlSdrDecode::new(out);
                graph.add(Box::new(src));
                graph.add(Box::new(rtlsdr));
                out
            } else {
                let (t, out) = FileSource::<Complex>::new(&read)?;
                graph.add(Box::new(t));
                out
            }
        } else if opt.rtlsdr {
            let (src, out) = RtlSdrSource::new(opt.freq, opt.sample_rate, opt.gain as i32)?;
            let (rtlsdr, out) = RtlSdrDecode::new(out);
            graph.add(Box::new(src));
            graph.add(Box::new(rtlsdr));
            out
        } else {
            panic!("Need to provide either -r, -c, or --rtlsdr");
        }
    };

    // Filter.
    //
    // TODO: doing this in multiple steps, with a decimating FIR filter, would
    // probably be more CPU efficient.
    let samp_rate = opt.sample_rate as f32;
    let taps = rustradio::fir::low_pass_complex(samp_rate, 50000.0, 10000.0, &WindowType::Hamming);
    debug!("FIR taps: {}", taps.len());
    let prev = add_block!(graph, FftFilter::new(src, &taps));

    // Resample.
    let new_samp_rate = 200_000.0;
    let prev = add_block![
        graph,
        RationalResampler::new(prev, new_samp_rate as usize, samp_rate as usize,)?
    ];
    let samp_rate = new_samp_rate;

    // Quad demod.
    let prev = add_block![graph, QuadratureDemod::new(prev, 1.0)];

    // Frequency adjust.
    let prev = add_block![graph, AddConst::new(prev, opt.offset)];

    // Clock sync.
    let baud = 38383.5;
    let prev = add_block![graph, ZeroCrossing::new(prev, samp_rate / baud, 0.1,)];

    /*
    // Save floats.
    let (prev,t) = add_block![graph, Tee::new(prev)];
    graph.add(Box::new(FileSink::new(t, "test.f32", rustradio::file_sink::Mode::Overwrite)?));
     */

    // Slice.
    let prev = add_block![graph, BinarySlicer::new(prev)];

    // Decode.
    let decode = Box::new(Decode::new(prev, opt.sensor_id, opt.output.clone()));
    graph.add(decode);

    // Set up to run.
    let cancel = graph.cancel_token();
    ctrlc::set_handler(move || {
        eprintln!("Received Ctrl+C!");
        cancel.cancel();
    })
    .expect("Error setting Ctrl-C handler");

    // Run.
    eprintln!("Running…");
    graph.run()?;
    eprintln!("{}", graph.generate_stats().unwrap());
    Ok(())
}
/* vim: textwidth=80
 */
