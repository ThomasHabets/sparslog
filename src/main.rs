use anyhow::Result;

use log::debug;
use structopt::StructOpt;

use std::collections::VecDeque;
use std::io::Write;
use std::net::SocketAddr;

use rustradio::block::{Block, BlockRet};
use rustradio::blocks::*;
use rustradio::stream::{InputStreams, OutputStreams, StreamType};
use rustradio::{Complex, Error};

#[derive(StructOpt, Debug)]
#[structopt()]
struct Opt {
    #[structopt(short = "s", long = "serial")]
    sensor_id: u32,

    #[structopt(short = "o", long = "output", default_value = "sparslog.csv")]
    output: String,

    #[structopt(short = "c", long = "connect")]
    connect: Option<String>,

    #[structopt(short = "r", long = "read")]
    read: Option<String>,

    #[structopt(long = "rtlsdr")]
    rtlsdr: bool,

    #[structopt(short = "v", default_value = "0")]
    verbose: usize,

    #[structopt(long = "gain", default_value = "30")]
    gain: f32,

    #[structopt(long = "sample_rate", default_value = "1024000")]
    sample_rate: u32,

    #[structopt(long = "freq", default_value = "868000000")]
    freq: u64,

    #[structopt(long = "offset", default_value = "0.4")]
    offset: f32,
}

struct Decode {
    sensor_id: u32,
    output: String,
    history: VecDeque<u8>,
}

impl Decode {
    fn new(sensor_id: u32, output: &str) -> Self {
        Self {
            sensor_id,
            output: output.to_string(),
            history: VecDeque::new(),
        }
    }
}

fn bits2byte(data: &[u8]) -> u8 {
    assert!(data.len() == 8);
    data[0] << 7
        | data[1] << 6
        | data[2] << 5
        | data[3] << 4
        | data[4] << 3
        | data[5] << 2
        | data[6] << 1
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
    let crc = (packet[packet.len() - 2] as u16) << 8 | packet[packet.len() - 1] as u16;
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
    let crc = (packet[packet.len() - 2] as u16) << 8 | packet[packet.len() - 1] as u16;
    let crc_ok = crc16(&packet[..packet.len() - 2], crc);

    let seq = (dec[4] as u16) << 8 | (dec[5] as u16);
    let effect = (dec[6] as u16) << 8 | (dec[7] as u16);
    let pulse =
        (dec[8] as u32) << 24 | (dec[9] as u32) << 16 | (dec[10] as u32) << 8 | dec[11] as u32;
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
    fn block_name(&self) -> &'static str {
        "Sparsnäs decoder"
    }
    fn work(&mut self, r: &mut InputStreams, _w: &mut OutputStreams) -> Result<BlockRet, Error> {
        let cac = vec![
            1u8, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0,
            0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1,
        ];
        let input = r.get(0);
        self.history.extend(input.borrow().iter());
        input.borrow_mut().clear();

        let packet_bits_len = cac.len() + 19 * 8;
        //let cac = vec![1,0,1,0,1,0,1,0,1,0,1];
        let n = self.history.len();
        //println!("Called with {n}");
        if n < packet_bits_len {
            //debug!("{} < {} len, sleeping", n, cac.len());
            return Ok(BlockRet::Ok);
        }
        //println!("Running on data size {n}");
        let input = &self.history;
        for i in 0..(n - packet_bits_len) {
            let equal = cac
                .iter()
                .zip(input.range(i..(i + cac.len())))
                .map(|(a, b)| a == b)
                .all(|x| x);
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
                    .map_err(|e| -> anyhow::Error { e.into() })?
                    .write_all(format!("{parsed}\n").as_bytes())
                    .map_err(|e| -> anyhow::Error { e.into() })?;
                println!("{}", parsed);
            }
        }
        self.history
            .drain(0..(self.history.len() - packet_bits_len));
        Ok(BlockRet::Ok)
    }
}

fn main() -> Result<()> {
    println!("Sparslog");
    let opt = Opt::from_args();
    stderrlog::new()
        .module(module_path!())
        .module("rustradio")
        .quiet(false)
        .verbosity(opt.verbose)
        .timestamp(stderrlog::Timestamp::Second)
        .init()
        .unwrap();

    let mut graph = rustradio::graph::Graph::new();

    // Source.
    let src = {
        if let Some(connect) = opt.connect {
            assert!(matches!(opt.read, None), "-c and -r can't both be used");
            let sa: SocketAddr = connect.parse()?;
            let host = format!("{}", sa.ip());
            let port = sa.port();
            println!("Connecting to host {} port {}", host, port);
            graph.add(Box::new(TcpSource::<Complex>::new(&host, port)?))
        } else if let Some(read) = opt.read {
            if opt.rtlsdr {
                graph.add(Box::new(FileSource::<u8>::new(&read, false)?))
            } else {
                graph.add(Box::new(FileSource::<Complex>::new(&read, false)?))
            }
        } else if opt.rtlsdr {
            graph.add(Box::new(RtlSdrSource::new(
                opt.freq,
                opt.sample_rate,
                opt.gain as i32,
            )?))
        } else {
            panic!("Need to provide either -r, -c, or --rtlsdr");
        }
    };

    // Filter.
    let samp_rate = opt.sample_rate as f32;
    let taps = rustradio::fir::low_pass_complex(samp_rate, 50000.0, 10000.0);
    debug!("FIR taps: {}", taps.len());
    let fir = graph.add(Box::new(FftFilter::new(&taps)));

    // Resample.
    let new_samp_rate = 200000.0;
    let rr = graph.add(Box::new(RationalResampler::new(
        new_samp_rate as usize,
        samp_rate as usize,
    )?));
    let samp_rate = new_samp_rate;

    // Quad demod.
    let quad = graph.add(Box::new(QuadratureDemod::new(1.0)));

    // Frequency adjust.
    let add = graph.add(Box::new(AddConst::new(opt.offset)));

    // Clock sync.
    let baud = 38383.5;
    let sync = graph.add(Box::new(rustradio::symbol_sync::ZeroCrossing::new(
        samp_rate / baud,
        0.1,
    )));

    // Slice.
    let slice = graph.add(Box::new(BinarySlicer::new()));

    // Decode.
    let decode = graph.add(Box::new(Decode::new(opt.sensor_id, &opt.output)));

    if opt.rtlsdr {
        // Optional RTL decoder.
        let rtlsdr = graph.add(Box::new(rustradio::rtlsdr::RtlSdrDecode::new()));

        graph.connect(StreamType::new_u8(), src, 0, rtlsdr, 0);
        graph.connect(StreamType::new_complex(), rtlsdr, 0, fir, 0);
    } else {
        graph.connect(StreamType::new_complex(), src, 0, fir, 0);
    }

    graph.connect(StreamType::new_complex(), fir, 0, rr, 0);
    graph.connect(StreamType::new_complex(), rr, 0, quad, 0);
    graph.connect(StreamType::new_float(), quad, 0, add, 0);
    graph.connect(StreamType::new_float(), add, 0, sync, 0);
    graph.connect(StreamType::new_float(), sync, 0, slice, 0);
    graph.connect(StreamType::new_u8(), slice, 0, decode, 0);
    eprintln!("Running…");
    let st = std::time::Instant::now();
    graph.run()?;
    eprintln!("{}", graph.generate_stats(st.elapsed()));
    Ok(())
}
