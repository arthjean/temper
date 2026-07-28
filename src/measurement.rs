use std::fmt;

use serde::Serialize;

pub(crate) const WARMUP_COUNT: usize = 2;
pub(crate) const SCREENING_SAMPLE_COUNT: usize = 7;
pub(crate) const CONFIRMATION_PAIR_COUNT: usize = 20;
pub(crate) const BOOTSTRAP_RESAMPLES: usize = 10_000;
pub(crate) const BOOTSTRAP_SEED: u64 = 0x5445_4d50_4552_0001;
pub(crate) const DISPERSION_LIMIT: f64 = 0.10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MeasurementOutcome {
    Stable,
    Accepted,
    InsufficientImprovement,
    UnstableMeasurement,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ScreeningResult {
    pub(crate) sample_durations_ns: [u64; SCREENING_SAMPLE_COUNT],
    pub(crate) median_duration_ns: u64,
    pub(crate) relative_mad: f64,
    pub(crate) outcome: MeasurementOutcome,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ConfidenceInterval {
    pub(crate) lower: f64,
    pub(crate) upper: f64,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ConfirmationResult {
    pub(crate) baseline_durations_ns: [u64; CONFIRMATION_PAIR_COUNT],
    pub(crate) candidate_durations_ns: [u64; CONFIRMATION_PAIR_COUNT],
    pub(crate) median_ratio: f64,
    pub(crate) baseline_relative_mad: f64,
    pub(crate) candidate_relative_mad: f64,
    pub(crate) confidence_interval_95: ConfidenceInterval,
    pub(crate) threshold_ratio: f64,
    pub(crate) outcome: MeasurementOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MeasurementError {
    EmptySamples,
    WrongSampleCount { expected: usize, actual: usize },
    UnequalPairCount { baseline: usize, candidate: usize },
    ZeroDuration,
    ArithmeticOverflow,
    NonFiniteValue,
    InvalidThreshold,
}

impl fmt::Display for MeasurementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySamples => formatter.write_str("measurement samples cannot be empty"),
            Self::WrongSampleCount { expected, actual } => {
                write!(formatter, "expected {expected} samples, received {actual}")
            }
            Self::UnequalPairCount {
                baseline,
                candidate,
            } => write!(
                formatter,
                "paired sample counts differ: baseline={baseline}, candidate={candidate}"
            ),
            Self::ZeroDuration => formatter.write_str("measurement durations must be nonzero"),
            Self::ArithmeticOverflow => formatter.write_str("measurement arithmetic overflowed"),
            Self::NonFiniteValue => formatter.write_str("measurement produced a non-finite value"),
            Self::InvalidThreshold => formatter
                .write_str("minimum improvement must be finite and between 0 and 100 percent"),
        }
    }
}

impl std::error::Error for MeasurementError {}

pub(crate) fn screening(samples: &[u64]) -> Result<ScreeningResult, MeasurementError> {
    if samples.is_empty() {
        return Err(MeasurementError::EmptySamples);
    }
    if samples.len() != SCREENING_SAMPLE_COUNT {
        return Err(MeasurementError::WrongSampleCount {
            expected: SCREENING_SAMPLE_COUNT,
            actual: samples.len(),
        });
    }
    validate_durations(samples)?;
    let sample_durations_ns: [u64; SCREENING_SAMPLE_COUNT] =
        samples
            .try_into()
            .map_err(|_| MeasurementError::WrongSampleCount {
                expected: SCREENING_SAMPLE_COUNT,
                actual: samples.len(),
            })?;
    let median_duration_ns = median_u64(samples)?;
    let relative_mad = relative_mad(samples)?;
    let outcome = if relative_mad > DISPERSION_LIMIT {
        MeasurementOutcome::UnstableMeasurement
    } else {
        MeasurementOutcome::Stable
    };

    Ok(ScreeningResult {
        sample_durations_ns,
        median_duration_ns,
        relative_mad,
        outcome,
    })
}

pub(crate) fn confirmation(
    baseline: &[u64],
    candidate: &[u64],
    minimum_improvement_percent: f64,
    bootstrap_seed: u64,
) -> Result<ConfirmationResult, MeasurementError> {
    if baseline.is_empty() || candidate.is_empty() {
        return Err(MeasurementError::EmptySamples);
    }
    if baseline.len() != candidate.len() {
        return Err(MeasurementError::UnequalPairCount {
            baseline: baseline.len(),
            candidate: candidate.len(),
        });
    }
    if baseline.len() != CONFIRMATION_PAIR_COUNT {
        return Err(MeasurementError::WrongSampleCount {
            expected: CONFIRMATION_PAIR_COUNT,
            actual: baseline.len(),
        });
    }
    if !minimum_improvement_percent.is_finite()
        || !(0.0..100.0).contains(&minimum_improvement_percent)
    {
        return Err(MeasurementError::InvalidThreshold);
    }
    validate_durations(baseline)?;
    validate_durations(candidate)?;

    let baseline_durations_ns: [u64; CONFIRMATION_PAIR_COUNT] =
        baseline
            .try_into()
            .map_err(|_| MeasurementError::WrongSampleCount {
                expected: CONFIRMATION_PAIR_COUNT,
                actual: baseline.len(),
            })?;
    let candidate_durations_ns: [u64; CONFIRMATION_PAIR_COUNT] =
        candidate
            .try_into()
            .map_err(|_| MeasurementError::WrongSampleCount {
                expected: CONFIRMATION_PAIR_COUNT,
                actual: candidate.len(),
            })?;
    let ratios = paired_ratios(baseline, candidate)?;
    let median_ratio = median_f64(&ratios)?;
    let baseline_relative_mad = relative_mad(baseline)?;
    let candidate_relative_mad = relative_mad(candidate)?;
    let confidence_interval_95 = bootstrap_interval(&ratios, bootstrap_seed)?;
    let threshold_ratio = 1.0 - (minimum_improvement_percent / 100.0);
    ensure_finite(&[
        median_ratio,
        baseline_relative_mad,
        candidate_relative_mad,
        confidence_interval_95.lower,
        confidence_interval_95.upper,
        threshold_ratio,
    ])?;

    let outcome = if baseline_relative_mad > DISPERSION_LIMIT
        || candidate_relative_mad > DISPERSION_LIMIT
    {
        MeasurementOutcome::UnstableMeasurement
    } else if median_ratio <= threshold_ratio && confidence_interval_95.upper <= threshold_ratio {
        MeasurementOutcome::Accepted
    } else {
        MeasurementOutcome::InsufficientImprovement
    };

    Ok(ConfirmationResult {
        baseline_durations_ns,
        candidate_durations_ns,
        median_ratio,
        baseline_relative_mad,
        candidate_relative_mad,
        confidence_interval_95,
        threshold_ratio,
        outcome,
    })
}

fn validate_durations(samples: &[u64]) -> Result<(), MeasurementError> {
    if samples.contains(&0) {
        Err(MeasurementError::ZeroDuration)
    } else {
        Ok(())
    }
}

fn paired_ratios(
    baseline: &[u64],
    candidate: &[u64],
) -> Result<[f64; CONFIRMATION_PAIR_COUNT], MeasurementError> {
    let mut ratios = [0.0; CONFIRMATION_PAIR_COUNT];
    for (index, (baseline, candidate)) in baseline.iter().zip(candidate).enumerate() {
        let ratio = (*candidate as f64) / (*baseline as f64);
        if !ratio.is_finite() {
            return Err(MeasurementError::NonFiniteValue);
        }
        ratios[index] = ratio;
    }
    Ok(ratios)
}

fn median_u64(samples: &[u64]) -> Result<u64, MeasurementError> {
    if samples.is_empty() {
        return Err(MeasurementError::EmptySamples);
    }
    let mut sorted = samples.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        Ok(sorted[middle])
    } else {
        sorted[middle - 1]
            .checked_add(sorted[middle])
            .map(|sum| sum / 2)
            .ok_or(MeasurementError::ArithmeticOverflow)
    }
}

fn relative_mad(samples: &[u64]) -> Result<f64, MeasurementError> {
    let median = median_u64(samples)?;
    if median == 0 {
        return Err(MeasurementError::ZeroDuration);
    }
    let deviations: Vec<u64> = samples
        .iter()
        .map(|sample| sample.abs_diff(median))
        .collect();
    let mad = median_u64(&deviations)?;
    let relative = (mad as f64) / (median as f64);
    ensure_finite(&[relative])?;
    Ok(relative)
}

fn median_f64(samples: &[f64]) -> Result<f64, MeasurementError> {
    if samples.is_empty() {
        return Err(MeasurementError::EmptySamples);
    }
    ensure_finite(samples)?;
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    let median = if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    };
    ensure_finite(&[median])?;
    Ok(median)
}

fn bootstrap_interval(
    ratios: &[f64; CONFIRMATION_PAIR_COUNT],
    seed: u64,
) -> Result<ConfidenceInterval, MeasurementError> {
    let mut generator = SplitMix64::new(seed);
    let mut estimates = Vec::with_capacity(BOOTSTRAP_RESAMPLES);
    let mut resample = [0.0; CONFIRMATION_PAIR_COUNT];

    for _ in 0..BOOTSTRAP_RESAMPLES {
        for value in &mut resample {
            let index = (generator.next_u64() as usize) % CONFIRMATION_PAIR_COUNT;
            *value = ratios[index];
        }
        estimates.push(median_f64(&resample)?);
    }
    estimates.sort_by(f64::total_cmp);
    let lower_index = ((BOOTSTRAP_RESAMPLES - 1) as f64 * 0.025).floor() as usize;
    let upper_index = ((BOOTSTRAP_RESAMPLES - 1) as f64 * 0.975).ceil() as usize;
    let interval = ConfidenceInterval {
        lower: estimates[lower_index],
        upper: estimates[upper_index],
    };
    ensure_finite(&[interval.lower, interval.upper])?;
    Ok(interval)
}

fn ensure_finite(values: &[f64]) -> Result<(), MeasurementError> {
    if values.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        Err(MeasurementError::NonFiniteValue)
    }
}

struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BOOTSTRAP_SEED, CONFIRMATION_PAIR_COUNT, MeasurementError, MeasurementOutcome,
        confirmation, screening,
    };

    fn cohort(
        base: u64,
        candidate_percent: u64,
        candidate_jitter_offset: usize,
    ) -> ([u64; 20], [u64; 20]) {
        let mut baseline = [0_u64; CONFIRMATION_PAIR_COUNT];
        let mut candidate = [0_u64; CONFIRMATION_PAIR_COUNT];
        let jitter_percent = [99_u64, 100, 101, 100, 99, 101, 100, 100, 101, 99];
        for index in 0..CONFIRMATION_PAIR_COUNT {
            baseline[index] = base * jitter_percent[index % jitter_percent.len()] / 100;
            let candidate_jitter =
                jitter_percent[(index + candidate_jitter_offset) % jitter_percent.len()];
            candidate[index] = base * candidate_jitter / 100 * candidate_percent / 100;
        }
        (baseline, candidate)
    }

    #[test]
    fn deterministic_validation_cohorts_hold_the_protocol_bounds() {
        let false_promotions = (0..100)
            .filter(|dataset| {
                let offset = (*dataset as usize % 9) + 1;
                let (baseline, candidate) = cohort(1_000_000 + dataset * 10_000, 100, offset);
                confirmation(&baseline, &candidate, 2.0, BOOTSTRAP_SEED)
                    .is_ok_and(|result| result.outcome == MeasurementOutcome::Accepted)
            })
            .count();
        assert_eq!(
            false_promotions, 0,
            "the pre-tag A/A control requires zero promotions"
        );

        let accepted_regressions = (0..20)
            .filter(|dataset| {
                let offset = (*dataset as usize % 9) + 1;
                let (baseline, candidate) = cohort(1_000_000 + dataset * 10_000, 105, offset);
                confirmation(&baseline, &candidate, 2.0, BOOTSTRAP_SEED)
                    .is_ok_and(|result| result.outcome == MeasurementOutcome::Accepted)
            })
            .count();
        assert_eq!(accepted_regressions, 0);

        let accepted_improvements = (0..20)
            .filter(|dataset| {
                let offset = (*dataset as usize % 9) + 1;
                let (baseline, candidate) = cohort(1_000_000 + dataset * 10_000, 95, offset);
                confirmation(&baseline, &candidate, 2.0, BOOTSTRAP_SEED)
                    .is_ok_and(|result| result.outcome == MeasurementOutcome::Accepted)
            })
            .count();
        assert!(accepted_improvements >= 18);
    }

    #[test]
    fn deterministic_sample_fixture_accepts_a_confirmed_improvement() {
        let baseline = [1_000_000; CONFIRMATION_PAIR_COUNT];
        let candidate = [950_000; CONFIRMATION_PAIR_COUNT];

        let result = confirmation(&baseline, &candidate, 2.0, BOOTSTRAP_SEED)
            .expect("deterministic confirmation");

        assert_eq!(result.outcome, MeasurementOutcome::Accepted);
        assert!(result.confidence_interval_95.upper <= result.threshold_ratio);
        assert_eq!(result.baseline_durations_ns, baseline);
        assert_eq!(result.candidate_durations_ns, candidate);
    }

    #[test]
    fn unstable_measurements_override_a_faster_median() {
        let baseline = [1_000_000; CONFIRMATION_PAIR_COUNT];
        let mut candidate = [500_000; CONFIRMATION_PAIR_COUNT];
        for value in candidate.iter_mut().skip(CONFIRMATION_PAIR_COUNT / 2) {
            *value = 1_100_000;
        }
        let result =
            confirmation(&baseline, &candidate, 2.0, BOOTSTRAP_SEED).expect("valid samples");
        assert!(result.median_ratio < 1.0);
        assert_eq!(result.outcome, MeasurementOutcome::UnstableMeasurement);
    }

    #[test]
    fn invalid_measurements_return_typed_errors() {
        assert!(matches!(
            screening(&[]),
            Err(MeasurementError::EmptySamples)
        ));
        assert!(matches!(
            screening(&[1, 2]),
            Err(MeasurementError::WrongSampleCount {
                expected: 7,
                actual: 2,
            })
        ));
        assert!(matches!(
            screening(&[1, 1, 1, 0, 1, 1, 1]),
            Err(MeasurementError::ZeroDuration)
        ));
        let maximums = [u64::MAX; CONFIRMATION_PAIR_COUNT];
        assert!(matches!(
            confirmation(&maximums, &maximums, 2.0, BOOTSTRAP_SEED),
            Err(MeasurementError::ArithmeticOverflow)
        ));
    }
}
