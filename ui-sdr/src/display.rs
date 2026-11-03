use async_channel::Sender;
use rustradio::block::{Block, BlockRet};
use rustradio::iq_stream::GAP_SAMPLES;
use rustradio::stream::{NCReadStream, Tag};
use rustradio::{Complex, Float};
use rustradio_ui::TaggedVec;

// Time sink size in seconds.
const WAVEFORM_TIME: f64 = 0.01;

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

#[derive(rustradio_macros::Block)]
pub(crate) struct MessageSink {
    #[rustradio(in)]
    pub(crate) src: NCReadStream<String>,
    pub(crate) messages: Sender<String>,
}

impl Block for MessageSink {
    fn work(&mut self) -> rustradio::Result<BlockRet<'_>> {
        if self.messages.is_full() {
            return Ok(BlockRet::Pending);
        }
        if let Some((message, _)) = self.src.pop() {
            let _ = self.messages.try_send(message);
            return Ok(BlockRet::Again);
        }
        Ok(BlockRet::WaitForStream(&self.src, 1))
    }
}

// Post each local waveform window as one message so queue overflow drops all
// traces together. The taps share a continuous graph timeline.
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

/// Chunk the two continuous local taps using their common zero-based origin.
/// Demodulated sample n describes filtered samples n and n+1, so its stream
/// contains one fewer sample at EOF. There are no independent transport offsets
/// to align, and neither input is required to carry IQ transport position tags.
pub(crate) fn waveform_windows(
    graph: &mut impl rustradio::graph::GraphRunner,
    filtered: rustradio::stream::ReadStream<Complex>,
    demodulated: rustradio::stream::ReadStream<Float>,
    points: usize,
) -> rustradio::Result<async_channel::Receiver<Vec<TaggedVec<Float>>>> {
    use rustradio::blocks::{ComplexToFloat, StreamChunks};
    let (convert, in_phase, quadrature) = ComplexToFloat::new(filtered)?;
    graph.add(Box::new(convert));
    let (chunks, in_phase) = StreamChunks::new(in_phase, points)?;
    graph.add(Box::new(chunks));
    let (chunks, quadrature) = StreamChunks::new(quadrature, points)?;
    graph.add(Box::new(chunks));
    let (chunks, demodulated) = StreamChunks::new(demodulated, points)?;
    graph.add(Box::new(chunks));
    let (frames, windows) = async_channel::bounded(2);
    graph.add(Box::new(WaveformSink {
        in_phase,
        quadrature,
        demodulated,
        frames,
    }));
    Ok(windows)
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
    let points = (rate * WAVEFORM_TIME).round();
    if !rate.is_finite() || rate <= 0.0 || points > f64::from(u32::MAX) {
        return Err(rustradio::Error::msg("Invalid time-sink sample rate"));
    }
    Ok(points.max(1.0) as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_receiver_waveforms_accept_untagged_capture() -> Result<(), Box<dyn std::error::Error>>
    {
        use rustradio::blocks::VectorSource;
        use rustradio::graph::GraphRunner;
        use std::future::Future;
        let mut graph = rustradio::wasm::wasm_graph::WasmGraph::new();
        let (source, samples) = VectorSource::new(vec![Complex::default(); 65536])?;
        graph.add(Box::new(source));
        let taps = sparslog::add_receiver_with_taps(
            &mut graph,
            samples,
            &sparslog::ReceiverConfig::new(576_929, 1_000_000),
        )?;
        let windows = waveform_windows(&mut graph, taps.filtered, taps.demodulated, 128)?;
        // All inputs are in memory: the WASM executor must complete without
        // browser I/O or a wake. Native Graph statistics require browser clocks
        // when rustradio's wasm feature is enabled.
        let (_poke, wake) = async_channel::bounded(1);
        {
            let mut running = std::pin::pin!(graph.run_async(wake));
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(matches!(
                running.as_mut().poll(&mut context),
                std::task::Poll::Ready(Ok(()))
            ));
        }
        drop(graph);
        let frame = windows.try_recv()?;
        assert_eq!(frame.len(), 3);
        assert!(frame.iter().all(|trace| trace.data.len() == 128));
        assert!(taps.measurements.eof());
        Ok(())
    }

    #[test]
    fn decoded_messages_wait_for_display_capacity() -> rustradio::Result<()> {
        let (tx, src) = rustradio::stream::new_nocopy_stream();
        let (messages, rx) = async_channel::bounded(1);
        let mut sink = MessageSink { src, messages };
        tx.push("hello 🦀".into(), &[]);
        tx.push(String::new(), &[]);
        sink.work()?;
        assert!(matches!(sink.work()?, BlockRet::Pending));
        assert_eq!(rx.try_recv().unwrap(), "hello 🦀");
        sink.work()?;
        assert_eq!(rx.try_recv().unwrap(), "");
        assert!(sink.src.is_empty());
        Ok(())
    }

    #[test]
    fn waveform_window_matches_negotiated_rate() -> rustradio::Result<()> {
        assert_eq!(waveform_size(200_000.0)?, 2_000);
        assert_eq!(waveform_size(48_000.0)?, 480);
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
