//! Source-independent receiver graph construction.
use crate::{Decoder, Measurement};
use anyhow::anyhow;
use log::info;
use rustradio::blocks::{
    AddConst, BinarySlicer, FftFilter, FirFilter, QuadratureDemod, Rotator, Tee, ZeroCrossing,
};
use rustradio::graph::GraphRunner;
use rustradio::stream::{NCReadStream, ReadStream};
use rustradio::window::WindowType;
use rustradio::{Complex, Result, blockchain};

/// Settings for the DSP receiver, independent of any capture device or UI.
#[derive(Clone, Debug)]
pub struct ReceiverConfig {
    /// Sensor serial used for packet decryption.
    pub sensor_id: u32,
    /// Input sample rate in whole samples per second; must be at least 200 kHz.
    pub sample_rate: u32,
    /// Receiver tuning frequency minus the desired channel frequency, in Hz.
    /// Use zero for an already centered input.
    pub tune_offset: i64,
    /// Offset added after quadrature demodulation.
    pub fsk_offset: f32,
}

impl ReceiverConfig {
    /// Configure an already centered input with the default FSK offset.
    #[must_use]
    pub fn new(sensor_id: u32, sample_rate: u32) -> Self {
        Self {
            sensor_id,
            sample_rate,
            tune_offset: 0,
            fsk_offset: 0.4,
        }
    }
}

/// Add filtering, demodulation, symbol slicing, and packet decoding to a graph.
///
/// The caller supplies complex samples and consumes the returned measurements.
/// No capture devices, files, listeners, metrics, threads, or clocks are created.
/// Streams retain the names `decimated`, `filtered`, `demodulated`, `soft-bits`,
/// and `bits` for rustradio's existing stream inspection facilities.
///
/// ```
/// use rustradio::blocks::VectorSource;
/// use rustradio::graph::{Graph, GraphRunner};
/// use rustradio::Complex;
/// use sparslog::{ReceiverConfig, add_receiver};
///
/// let mut graph = Graph::new();
/// let (source, samples) = VectorSource::new(vec![Complex::default(); 4096])?;
/// graph.add(Box::new(source));
/// let config = ReceiverConfig::new(576_929, 1_000_000);
/// let measurements = add_receiver(&mut graph, samples, &config)?;
/// // Attach an application sink to `measurements`, then run the graph.
/// # Ok::<(), anyhow::Error>(())
/// ```
///
/// # Errors
/// Returns an error for unsupported sample rates, tuning offsets, nonfinite
/// FSK offsets, or if constructing a DSP block fails.
pub fn add_receiver(
    graph: &mut (impl GraphRunner + ?Sized),
    input: ReadStream<Complex>,
    config: &ReceiverConfig,
) -> anyhow::Result<NCReadStream<Measurement>> {
    Ok(build_receiver(graph, input, config, false)?.0)
}

/// Receiver outputs for local spectrum and waveform displays.
pub struct ReceiverStreams {
    /// Decoded measurement records; the caller must consume these.
    pub measurements: NCReadStream<Measurement>,
    /// Filtered complex samples; the caller must consume these.
    pub filtered: ReadStream<Complex>,
    /// Demodulated samples; the caller must consume these.
    pub demodulated: ReadStream<f32>,
    /// Sample rate shared by both display streams.
    pub sample_rate: f64,
}

/// Build the shared receiver with additional display outputs.
///
/// # Errors
/// Returns the same configuration and construction errors as [`add_receiver`].
///
/// # Panics
/// Panics if the internal receiver builder fails to return requested taps.
pub fn add_receiver_with_taps(
    graph: &mut (impl GraphRunner + ?Sized),
    input: ReadStream<Complex>,
    config: &ReceiverConfig,
) -> anyhow::Result<ReceiverStreams> {
    let (measurements, taps) = build_receiver(graph, input, config, true)?;
    let (filtered, demodulated) = taps.expect("requested receiver taps");
    Ok(ReceiverStreams {
        measurements,
        filtered,
        demodulated,
        sample_rate: f64::from(config.sample_rate) / f64::from(config.sample_rate / 200_000),
    })
}

type Taps = (ReadStream<Complex>, ReadStream<f32>);
fn build_receiver(
    graph: &mut (impl GraphRunner + ?Sized),
    input: ReadStream<Complex>,
    config: &ReceiverConfig,
    taps: bool,
) -> anyhow::Result<(NCReadStream<Measurement>, Option<Taps>)> {
    let sample_rate = config.sample_rate;
    if sample_rate < 200_000 {
        return Err(anyhow!("receiver sample rate must be at least 200 kHz"));
    }
    validate_tune_offset(sample_rate, config.tune_offset)?;
    if !config.fsk_offset.is_finite() {
        return Err(anyhow!("FSK offset must be finite"));
    }
    let prev = translate_rtl(graph, input, sample_rate, config.tune_offset)?;
    // Our goal post decimation is to end up at about 200ksps.
    let fir_decimation: usize = (sample_rate as usize) / 200_000;

    #[allow(clippy::cast_precision_loss)]
    let samp_rate = sample_rate as f32;
    #[allow(clippy::cast_precision_loss)]
    let samp_rate_2 = samp_rate / (fir_decimation as f32);
    let baud = 38383.5;

    let fir_taps = rustradio::fir::low_pass_complex(
        samp_rate,
        samp_rate_2 / 4.0,
        75_000.0,
        WindowType::Hamming,
    )?;
    info!("Initial FIR filter tap count: {}", fir_taps.len());
    let mut filtered = None;
    let mut demodulated = None;
    // Resample.
    let prev = blockchain![
        graph,
        prev,
        FirFilter::builder(fir_taps)
            .deci(fir_decimation)
            .build(prev)?,
        FftFilter::new(
            prev.with_name("decimated"),
            rustradio::fir::low_pass_complex(
                samp_rate_2,
                50_000.0,
                10_000.0,
                &WindowType::Hamming
            )?,
        )?,
        {
            let prev = prev.with_name("filtered");
            let prev = if taps {
                let (tee, signal, display) = Tee::new(prev)?;
                graph.add(Box::new(tee));
                filtered = Some(display);
                signal
            } else {
                prev
            };
            QuadratureDemod::new(prev, 1.0)?
        },
        AddConst::new(prev, config.fsk_offset)?,
        {
            let prev = prev.with_name("demodulated");
            let prev = if taps {
                let (tee, signal, display) = Tee::new(prev)?;
                graph.add(Box::new(tee));
                demodulated = Some(display);
                signal
            } else {
                prev
            };
            ZeroCrossing::new(prev, samp_rate_2 / baud, 0.1)?
        },
        BinarySlicer::new(prev.with_name("soft-bits"))?,
    ]
    .with_name("bits");

    let (decoder, measurements) = Decoder::new(prev, config.sensor_id);
    graph.add(Box::new(decoder));
    Ok((measurements, filtered.zip(demodulated)))
}

#[allow(clippy::cast_precision_loss)]
pub(crate) fn translate_rtl(
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

pub(crate) fn validate_tune_offset(sample_rate: u32, offset: i64) -> anyhow::Result<()> {
    // Include the low-pass filter's 50 kHz passband and 10 kHz transition.
    if offset.unsigned_abs() + 60_000 > u64::from(sample_rate) / 2 {
        return Err(anyhow!(
            "tuning offset plus 60 kHz must fit within half the sample rate"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustradio::blocks::VectorSource;

    #[test]
    fn display_taps_drain_and_report_fractional_rate() -> anyhow::Result<()> {
        use rustradio::blocks::VectorSink;
        let mut graph = rustradio::graph::Graph::new();
        let (source, samples) = VectorSource::new(vec![Complex::default(); 65536])?;
        graph.add(Box::new(source));
        let output = add_receiver_with_taps(
            &mut graph,
            samples,
            &ReceiverConfig::new(576_929, 1_000_003),
        )?;
        assert!((output.sample_rate - 200_000.6).abs() < 0.001);
        let filtered = VectorSink::new(output.filtered, 65536);
        let filtered_data = filtered.hook();
        graph.add(Box::new(filtered));
        let demodulated = VectorSink::new(output.demodulated, 65536);
        let demodulated_data = demodulated.hook();
        graph.add(Box::new(demodulated));
        graph.run()?;
        // Quadrature demodulation needs the preceding sample and emits one fewer.
        assert_eq!(
            filtered_data.data().samples().len(),
            demodulated_data.data().samples().len() + 1
        );
        assert!(filtered_data.data().samples().len() > 1000);
        assert!(output.measurements.eof());
        Ok(())
    }

    #[test]
    fn invalid_configuration_is_rejected_before_running() -> anyhow::Result<()> {
        for (sample_rate, tune_offset, fsk_offset) in [
            (0, 0, 0.4),
            (199_999, 0, 0.4),
            (1_000_000, i64::MIN, 0.4),
            (1_000_000, i64::MAX, 0.4),
            (1_000_000, 440_001, 0.4),
            (1_000_000, 0, f32::NAN),
            (1_000_000, 0, f32::INFINITY),
        ] {
            let mut graph = rustradio::graph::Graph::new();
            let (_source, input) = VectorSource::new(vec![Complex::default(); 1])?;
            let config = ReceiverConfig {
                tune_offset,
                fsk_offset,
                ..ReceiverConfig::new(576_929, sample_rate)
            };
            assert!(add_receiver(&mut graph, input, &config).is_err());
        }
        Ok(())
    }

    #[test]
    fn receiver_runs_on_caller_supplied_samples_without_native_output() -> anyhow::Result<()> {
        for rate in [200_000, 1_000_000, 1_024_000] {
            let mut graph = rustradio::graph::Graph::new();
            let (source, input) = VectorSource::new(vec![Complex::default(); 32_768])?;
            graph.add(Box::new(source));
            let measurements =
                add_receiver(&mut graph, input, &ReceiverConfig::new(576_929, rate))?;
            graph.run()?;
            assert!(measurements.eof());
            assert!(measurements.is_empty());
        }
        Ok(())
    }
}
