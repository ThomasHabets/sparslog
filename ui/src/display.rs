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

// Post each aligned window as one message so queue overflow drops all traces
// together. Chunking and sample alignment remain rustradio graph operations.
#[derive(rustradio_macros::Block)]
pub(crate) struct WaveformSink {
    #[rustradio(in)]
    pub(crate) in_phase: NCReadStream<Vec<Float>>,
    #[rustradio(in)]
    pub(crate) quadrature: NCReadStream<Vec<Float>>,
    #[rustradio(in)]
    pub(crate) demodulated: NCReadStream<Vec<Float>>,
    pub(crate) frames: Sender<Vec<TaggedVec<Float>>>,
}

impl Block for WaveformSink {
    fn work(&mut self) -> rustradio::Result<BlockRet<'_>> {
        loop {
            for input in [&self.in_phase, &self.quadrature, &self.demodulated] {
                if input.is_empty() {
                    return Ok(BlockRet::WaitForStream(input, 1));
                }
            }
            let window: Vec<_> = [&self.in_phase, &self.quadrature, &self.demodulated]
                .into_iter()
                .map(|input| {
                    let (data, tags) = input.pop().expect("window available");
                    TaggedVec { data, tags }
                })
                .collect();
            if window
                .iter()
                .all(|trace| trace.tags.iter().all(|tag| tag.key() != GAP_SAMPLES))
            {
                let _ = self.frames.try_send(window);
            }
        }
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
    fn waveform_windows_stay_together_across_gaps_and_overflow() -> rustradio::Result<()> {
        let (i_tx, in_phase) = rustradio::stream::new_nocopy_stream();
        let (q_tx, quadrature) = rustradio::stream::new_nocopy_stream();
        let (d_tx, demodulated) = rustradio::stream::new_nocopy_stream();
        let (frames, windows) = async_channel::bounded(1);
        let mut sink = WaveformSink {
            in_phase,
            quadrature,
            demodulated,
            frames,
        };
        i_tx.push(vec![1.0], &[]);
        q_tx.push(vec![2.0], &[]);
        sink.work()?;
        assert!(windows.try_recv().is_err());
        d_tx.push(vec![3.0], &[]);
        sink.work()?;
        // A full display queue must drop the entire next window.
        for tx in [&i_tx, &q_tx, &d_tx] {
            tx.push(vec![4.0], &[]);
        }
        sink.work()?;
        let window = windows.try_recv().unwrap();
        assert_eq!(
            window.iter().map(|trace| trace.data[0]).collect::<Vec<_>>(),
            vec![1.0, 2.0, 3.0]
        );
        assert!(windows.try_recv().is_err());
        // A marker on any one trace discards all three corresponding chunks.
        for gap_trace in 0..3 {
            for (index, tx) in [&i_tx, &q_tx, &d_tx].into_iter().enumerate() {
                let tags = if index == gap_trace {
                    vec![Tag::new(
                        0,
                        GAP_SAMPLES,
                        rustradio::stream::TagValue::U64(5),
                    )]
                } else {
                    vec![]
                };
                tx.push(vec![5.0], tags);
            }
            sink.work()?;
            assert!(windows.try_recv().is_err());
        }
        for tx in [&i_tx, &q_tx, &d_tx] {
            tx.push(vec![6.0], &[]);
        }
        sink.work()?;
        let window = windows.try_recv().unwrap();
        assert_eq!(window.len(), 3);
        assert!(window.iter().all(|trace| trace.data == vec![6.0]));
        Ok(())
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
