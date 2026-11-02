//! Packet assembly, correction, and decoding without host I/O or clocks.
use log::debug;
use rustradio::Result;
use rustradio::block::{Block, BlockRet};
use rustradio::stream::{NCWriteStream, ReadStream};
use std::collections::VecDeque;

/// A decoded Sparsnäs measurement. Values are retained even when CRC fails.
#[derive(Clone, Debug, PartialEq)]
pub struct Measurement {
    /// Sensor serial used to decrypt this packet.
    pub sensor_id: u32,
    /// Packet sequence number.
    pub sequence_number: u16,
    /// Instantaneous power in watts.
    pub watts: f32,
    /// Cumulative energy in watt-hours.
    pub watt_hours: u32,
    /// Reported battery status.
    pub battery_status: u8,
    /// Whether the packet CRC is valid after single-bit correction.
    pub crc_ok: bool,
}

impl Measurement {
    /// Format the existing CSV record using a caller-supplied Unix timestamp.
    #[must_use]
    pub fn to_csv(&self, timestamp: u64) -> String {
        let status = if self.crc_ok { "OK" } else { "BAD" };
        format!(
            "{timestamp},{},{:.3},{}.{:03},{},{status}",
            self.sequence_number,
            self.watts,
            self.watt_hours / 1000,
            self.watt_hours % 1000,
            self.battery_status,
        )
    }
}

/// Assemble sliced bits into measurements, applying output backpressure.
///
/// This block performs no file, network, metrics, or clock operations.
#[derive(rustradio::rustradio_macros::Block)]
#[rustradio(new, custom_name)]
pub struct Decoder {
    #[rustradio(in)]
    src: ReadStream<u8>,
    #[rustradio(out)]
    dst: NCWriteStream<Measurement>,
    sensor_id: u32,
    #[rustradio(default)]
    history: VecDeque<u8>,
}

impl Decoder {
    fn custom_name(&self) -> &'static str {
        let _ = self;
        "Sparsnäs decoder"
    }
}

fn bits2byte(data: [u8; 8]) -> u8 {
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

fn parse_packet(packet: &[u8], sensor_id: u32) -> Measurement {
    assert_eq!(packet.len(), 20);
    let packet = fix_packet(packet);

    debug!("Packet: {packet:02x?}");

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
    let crc = (u16::from(packet[packet.len() - 2]) << 8) | u16::from(packet[packet.len() - 1]);
    let crc_ok = crc16(&packet[..packet.len() - 2], crc);

    let seq = (u16::from(dec[4]) << 8) | u16::from(dec[5]);
    let effect = (u16::from(dec[6]) << 8) | u16::from(dec[7]);
    let wh = (u32::from(dec[8]) << 24)
        | (u32::from(dec[9]) << 16)
        | (u32::from(dec[10]) << 8)
        | u32::from(dec[11]);
    let battery = dec[12];

    let watt = 3600.0 * 1024.0 / f32::from(effect);

    Measurement {
        sensor_id,
        sequence_number: seq,
        watts: watt,
        watt_hours: wh,
        battery_status: battery,
        crc_ok,
    }
}

impl Block for Decoder {
    fn work(&mut self) -> Result<BlockRet<'_>> {
        const CAC: [u8; 40] = [
            1u8, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 0, 0, 0, 0,
            0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 1,
        ];
        const PACKET_BITS: usize = CAC.len() + 19 * 8;
        let mut available = self.dst.remaining();
        if available == 0 {
            return Ok(BlockRet::WaitForStream(&self.dst, 1));
        }
        let (input, _) = self.src.read_buf()?;
        if input.is_empty() {
            return Ok(BlockRet::WaitForStream(&self.src, 1));
        }
        let mut consumed = 0;
        for bit in input.iter() {
            self.history.push_back(*bit);
            consumed += 1;
            if self.history.len() < PACKET_BITS {
                continue;
            }
            if CAC.iter().zip(&self.history).all(|(a, b)| a == b) {
                let bits: Vec<_> = self.history.iter().copied().collect();
                let bytes: Vec<_> = bits
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .copied()
                    .map(bits2byte)
                    .collect();
                self.dst
                    .push(parse_packet(&bytes[4..], self.sensor_id), &[]);
                available -= 1;
            }
            self.history.pop_front();
            if available == 0 {
                break;
            }
        }
        input.consume(consumed);
        Ok(BlockRet::Again)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustradio::block::BlockEOF;
    use rustradio::stream::{WriteStream, new_stream};

    const PACKET: [u8; 20] = [
        0x11, 0xa1, 0x38, 0x07, 0x0e, 0xa2, 0xde, 0x29, 0xe6, 0x8b, 0x1a, 0xfd, 0x74, 0x47, 0xcf,
        0xf2, 0x14, 0x80, 0x23, 0x7b,
    ];

    fn bits() -> Vec<u8> {
        [0xaa, 0xaa, 0xd2, 0x01]
            .into_iter()
            .chain(PACKET)
            .flat_map(|byte| (0..8).rev().map(move |bit| (byte >> bit) & 1))
            .collect()
    }

    fn write(writer: &WriteStream<u8>, bits: &[u8]) -> Result<()> {
        let mut output = writer.write_buf()?;
        output.slice()[..bits.len()].copy_from_slice(bits);
        output.produce(bits.len(), &[]);
        Ok(())
    }

    #[test]
    fn measurements_preserve_fields_crc_correction_and_csv() {
        let measurement = parse_packet(&PACKET, 576_929);
        assert_eq!(measurement.sensor_id, 576_929);
        assert_eq!(measurement.sequence_number, 17_592);
        assert_eq!(measurement.watt_hours, 20_674);
        assert_eq!(measurement.battery_status, 100);
        assert!(measurement.crc_ok);
        assert_eq!(measurement.to_csv(123), "123,17592,330.560,20.674,100,OK");
        let mut damaged = PACKET;
        damaged[8] ^= 1;
        assert_eq!(parse_packet(&damaged, 576_929), measurement);
        damaged = PACKET;
        damaged[6] ^= 1;
        damaged[19] ^= 1;
        assert_eq!(
            parse_packet(&damaged, 576_929).to_csv(456),
            "456,17592,330.560,20.674,100,BAD"
        );
    }

    #[test]
    fn decoder_handles_chunk_boundaries_and_final_packet() -> Result<()> {
        for chunk_size in [1, 7, 39, 40, 95, 191, 192, 193] {
            let (writer, input) = new_stream()?;
            let (mut decoder, measurements) = Decoder::new(input, 576_929);
            // Include noise, two adjacent packets, and no trailing sample.
            let input: Vec<_> = [0, 1, 1].into_iter().chain(bits()).chain(bits()).collect();
            for chunk in input.chunks(chunk_size) {
                write(&writer, chunk)?;
                decoder.work()?;
            }
            drop(writer);
            assert!(decoder.eof());
            for _ in 0..2 {
                let (measurement, tags) = measurements.pop().expect("decoded packet");
                assert_eq!(measurement, parse_packet(&PACKET, 576_929));
                assert_eq!(tags, []);
            }
            assert!(measurements.is_empty());
            drop(decoder);
            assert!(measurements.eof());
        }
        Ok(())
    }

    #[test]
    fn full_output_waits_without_losing_the_next_packet() -> Result<()> {
        let (writer, input) = new_stream()?;
        let (mut decoder, measurements) = Decoder::new(input, 576_929);
        let expected = parse_packet(&PACKET, 576_929);
        let capacity = decoder.dst.remaining();
        for _ in 0..capacity {
            decoder.dst.push(expected.clone(), &[]);
        }
        write(&writer, &bits())?;
        drop(writer);
        assert!(matches!(decoder.work()?, BlockRet::WaitForStream(_, 1)));
        assert!(!decoder.eof());
        for _ in 0..capacity {
            assert_eq!(measurements.pop().unwrap().0, expected);
        }
        decoder.work()?;
        assert_eq!(measurements.pop().unwrap().0, expected);
        assert!(measurements.is_empty());
        assert!(decoder.eof());
        Ok(())
    }
}
