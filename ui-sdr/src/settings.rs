//! Receiver setting checks shared with native helper tests.
pub(crate) fn validate(
    rate: u32,
    offset: i64,
    fsk: f32,
    gain: f32,
    frequency: u32,
) -> Result<(), String> {
    if rate < 200_000
        || !fsk.is_finite()
        || !gain.is_finite()
        || !(-100.0..=100.0).contains(&gain)
        || offset.unsigned_abs() + 60_000 > u64::from(rate) / 2
        || i64::from(frequency)
            .checked_add(offset)
            .and_then(|f| u32::try_from(f).ok())
            .is_none_or(|f| f == 0)
    {
        return Err(
            "Invalid receiver settings: check sample rate, frequency, gain and tuning offset"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate;
    #[test]
    fn defaults_and_invalid_capture_settings() {
        assert!(validate(1_024_000, 100_000, 0.4, 30.0, 868_000_000).is_ok());
        for (rate, offset, fsk, gain, frequency) in [
            (0, 0, 0.4, 30.0, 868_000_000),
            (1_024_000, i64::MAX, 0.4, 30.0, 868_000_000),
            (1_024_000, 100_000, f32::NAN, 30.0, 868_000_000),
            (1_024_000, 100_000, 0.4, f32::INFINITY, 868_000_000),
            (1_024_000, -100_000, 0.4, 30.0, 1),
        ] {
            assert!(validate(rate, offset, fsk, gain, frequency).is_err());
        }
    }
}
