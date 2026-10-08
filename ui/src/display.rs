use async_channel::Sender;
use rustradio::block::{Block, BlockRet};
use rustradio::iq_stream::GAP_SAMPLES;
use rustradio::stream::{NCReadStream, Tag};
use rustradio::{Complex, Float};
use rustradio_ui::TaggedVec;

#[derive(rustradio_macros::Block)]
pub(crate) struct DisplaySink {
    #[rustradio(in)]
    pub(crate) src: NCReadStream<Vec<Float>>,
    pub(crate) frames: Sender<TaggedVec<Float>>,
}

impl Block for DisplaySink {
    fn work(&mut self) -> rustradio::Result<BlockRet<'_>> {
        while let Some((data, tags)) = self.src.pop() {
            // Display congestion never holds the network graph open.
            let _ = self.frames.try_send(TaggedVec { data, tags });
        }
        Ok(BlockRet::WaitForStream(&self.src, 1))
    }
}

pub(crate) fn contiguous<T>(samples: Vec<T>, tags: Vec<Tag>) -> Vec<(Vec<T>, Vec<Tag>)> {
    if tags.iter().any(|tag| tag.key() == GAP_SAMPLES) {
        vec![]
    } else {
        vec![(samples, tags)]
    }
}

pub(crate) fn window_chunk(
    mut samples: Vec<Complex>,
    tags: Vec<Tag>,
    window: &[f32],
) -> Vec<(Vec<Complex>, Vec<Tag>)> {
    for (sample, weight) in samples.iter_mut().zip(window) {
        *sample *= *weight;
    }
    contiguous(samples, tags)
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub(crate) fn waveform_size(rate: f64) -> rustradio::Result<usize> {
    let points = (rate * 0.05).round();
    if !rate.is_finite() || rate <= 0.0 || points > f64::from(u32::MAX) {
        return Err(rustradio::Error::msg("Invalid time-sink sample rate"));
    }
    Ok(points.max(1.0) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_window_matches_negotiated_rate() -> rustradio::Result<()> {
        assert_eq!(waveform_size(200_000.0)?, 10_000);
        assert_eq!(waveform_size(48_000.0)?, 2_400);
        assert_eq!(waveform_size(1.0)?, 1);
        for invalid in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(waveform_size(invalid).is_err());
        }
        Ok(())
    }

    #[test]
    fn gap_windows_are_discarded() {
        let gap = Tag::new(1, GAP_SAMPLES, rustradio::stream::TagValue::U64(12));
        assert!(
            window_chunk(
                vec![Complex::new(1.0, 0.0); 2],
                vec![gap.clone()],
                &[1.0; 2]
            )
            .is_empty()
        );
        assert!(contiguous(vec![1.0_f32; 2], vec![gap]).is_empty());
        let windows = window_chunk(vec![Complex::new(2.0, 0.0); 2], vec![], &[0.5; 2]);
        assert_eq!(windows[0].0, vec![Complex::new(1.0, 0.0); 2]);
    }

    #[test]
    fn full_display_queue_does_not_block_graph() -> rustradio::Result<()> {
        let (tx, input) = rustradio::stream::new_nocopy_stream();
        let (frames, rows) = async_channel::bounded(1);
        let mut sink = DisplaySink { src: input, frames };
        tx.push(vec![1.0], &[]);
        tx.push(vec![2.0], &[]);
        assert!(matches!(sink.work()?, BlockRet::WaitForStream(_, 1)));
        assert_eq!(rows.try_recv().unwrap().data, vec![1.0]);
        assert!(rows.try_recv().is_err());
        assert!(matches!(sink.work()?, BlockRet::WaitForStream(_, 1)));
        Ok(())
    }
}
