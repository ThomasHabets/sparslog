use anyhow::Result;

use rustradio::add_const::AddConst;
use rustradio::binary_slicer::BinarySlicer;
use rustradio::file_source::FileSource;
use rustradio::fir::FIRFilter;
use rustradio::quadrature_demod::QuadratureDemod;
use rustradio::rational_resampler::RationalResampler;
use rustradio::symbol_sync::SymbolSync;
use rustradio::{Block, Float, Sink, Source, Stream, StreamReader};

struct Decode {
    pos: u64,
}

impl Decode {
    fn new() -> Self {
        Self { pos: 0 }
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
        assert_eq!(parsepacket(&packet), "17592,330.55954,20.674,100,OK");

        // With one bitflip.
        let packet = vec![
            0x11, 0xa1, 0x38, 0x07, 0x0e, 0xa2, 0xde, 0x29, 0xe7, 0x8b, 0x1a, 0xfd, 0x74, 0x47,
            0xcf, 0xf2, 0x14, 0x80, 0x23, 0x7b,
        ];
        assert_eq!(parsepacket(&packet), "17592,330.55954,20.674,100,OK");

        // With two bitflips.
        let packet = vec![
            0x11, 0xa1, 0x38, 0x07, 0x0e, 0xa2, 0xdf, 0x29, 0xe6, 0x8b, 0x1a, 0xfd, 0x74, 0x47,
            0xcf, 0xf2, 0x14, 0x80, 0x23, 0x7a,
        ];
        assert_eq!(parsepacket(&packet), "17592,330.55954,20.674,100,BAD");
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
            reg = reg << 1;
        }
        s = (s << 1) & 0xff;
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

fn parsepacket(packet: &[u8]) -> String {
    assert!(packet.len() == 20);
    //let sensor = packet[0];
    //let app = packet[1];
    let packet = fix_packet(packet);

    // This is the correct packet.
    println!("Packet: {:02x?}", packet);

    let sensor_id: u32 = 576929;
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
    format!(
        "{seq},{watt},{kwh},{battery},{}",
        if crc_ok { "OK" } else { "BAD" }
    )
}

impl Sink<u8> for Decode {
    fn work(&mut self, r: &mut dyn StreamReader<u8>) -> Result<()> {
        let cac = vec![
            1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0,
            0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1,
        ];
        //let cac = vec![1,0,1,0,1,0,1,0,1,0,1];
        let n = r.available();
        //println!("Called with {n}");
        if n < cac.len() {
            println!("{} < {} len, sleeping", n, cac.len());
            std::thread::sleep(std::time::Duration::from_secs(1));
            return Ok(());
        }
        let oldpos = self.pos;
        //println!("Running on data size {n}");
        let input = r.buffer();
        for i in 0..(n - cac.len()) {
            if cac == input[i..(i + cac.len())] {
                println!("Found CAC at pos {}", self.pos);
                let bits = &input[i..(i + cac.len() + 19 * 8)];
                let mut bytes = Vec::new();
                for j in (0..bits.len()).step_by(8) {
                    bytes.push(bits2byte(&bits[j..j + 8]));
                }
                //println!("bytes: {:02x?}", bytes);
                let packet = &bytes[4..];
                //println!("packet: {:02x?}", packet);
                println!("{}", parsepacket(&packet));
            }
            self.pos += 1;
        }
        self.pos = oldpos + n as u64;
        r.consume(n);
        Ok(())
    }
}

fn main() -> Result<()> {
    println!("Hello, world!");

    //let mut src = TCPSource::new(Complex::new(0.0,0.0)).await?;

    // Source.
    let mut src = FileSource::new("burst.c32", false)?;
    //let mut src = FileSource::new("b200-868M-1024k-ofs-1s.c32", false)?;

    // Filter.
    let samp_rate = 1024000.0;
    let mut fir = FIRFilter::new(&rustradio::fir::low_pass(samp_rate, 50000.0, 1000.0));

    // Resample.
    let new_samp_rate = 200000.0;
    let mut rr = RationalResampler::new(new_samp_rate as usize, samp_rate as usize)?;
    let samp_rate = new_samp_rate;

    // Quad demod.
    let mut quad = QuadratureDemod::new(1.0);

    // Frequency adjust.
    let mut add = AddConst::new(0.4);

    // Clock sync.
    let baud = 38383.5;
    let mut sync: Box<dyn Block<Float, Float>> = {
        if false {
            Box::new(SymbolSync::new(samp_rate / baud, 0.1))
        } else {
            //let mut sync = ZeroCrossing::new(samp_rate / baud, 0.1);
            Box::new(rustradio::symbol_sync::ZeroCrossing::new(
                samp_rate / baud,
                0.1,
            ))
        }
    };

    // Slice.
    let mut slice = BinarySlicer::new();

    // Decode.
    let mut decode = Decode::new();

    let mut s1 = Stream::new(1000000);
    let mut s2 = Stream::new(1000000);
    let mut s3 = Stream::new(1000000);
    let mut s4 = Stream::new(1000000);
    let mut s5 = Stream::new(1000000);
    let mut s6 = Stream::new(1000000);
    let mut s7 = Stream::new(1000000);

    loop {
        src.work(&mut s1)?;
        fir.work(&mut s1, &mut s2)?;
        rr.work(&mut s2, &mut s3)?;
        quad.work(&mut s3, &mut s4)?;
        add.work(&mut s4, &mut s5)?;
        sync.work(&mut s5, &mut s6)?;
        slice.work(&mut s6, &mut s7)?;
        decode.work(&mut s7)?;
    }
    //Ok(())
}
