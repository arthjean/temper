# measurement-v1

Status: experimental and versioned with Temper report schema 1.

## Protocol

Temper measures the wall-clock duration of the complete workload process with a
monotonic clock. Every valid recorded duration is stored as integer nanoseconds.
An invocation is valid only when it exits with status zero before its timeout and
neither output stream exceeds its bound.

Screening runs two unrecorded warmups followed by exactly seven recorded samples
for each artifact. The screening summary is the sample median and relative median
absolute deviation (relative MAD). An artifact whose relative MAD exceeds 10% is
classified `unstable_measurement`.

Confirmation contains exactly twenty paired baseline and candidate observations.
Collection alternates order by pair: `AB`, `BA`, `AB`, `BA`, and so on, where A
is the baseline. Each pair produces the ratio `candidate_ns / baseline_ns`. The
point estimate is the median of the twenty paired ratios.

The 95% confidence interval is a paired nonparametric bootstrap:

1. Seed SplitMix64 with `0x54454d5045520001`.
2. Draw twenty pair indices with replacement and take their median ratio.
3. Repeat for exactly 10,000 resamples.
4. Sort the estimates. The lower endpoint is index 249 (floor of the 2.5th
   percentile over indices 0 through 9,999) and the upper endpoint is index
   9,750 (ceiling of the 97.5th percentile).

With the default practical-improvement threshold of 2%, a candidate is accepted
only when its median ratio and upper confidence endpoint are both at most 0.98.
The baseline and candidate confirmed-duration relative MAD values must each be at
most 10%. Dispersion failure takes precedence and is reported as
`unstable_measurement`.

Empty samples, zero durations, the wrong sample count, arithmetic overflow, an
invalid threshold, and non-finite derived values are typed measurement errors.
They never produce a decision.

## Deterministic validation

The implementation fixes both the generator inputs and bootstrap seed. Its test
cohorts exercise:

- 100 A/A controls: no more than one may be accepted.
- 20 datasets with a 5% candidate regression: none may be accepted.
- 20 datasets with a 5% candidate improvement and at most 2% relative
  dispersion: at least eighteen must be accepted.

These bounds are part of measurement-v1. Changing the sample counts, estimator,
seed, bootstrap count, interval endpoints, dispersion limit, or decision rule
requires a new protocol version or a complete rerun of all three cohorts.
