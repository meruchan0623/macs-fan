//! Research-only recommendations. No I/O, hardware writes, or control acknowledgement.
//! Time is milliseconds from one monotonic clock, never wall-clock time.

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub min_rpm: f64,
    pub max_rpm: f64,
    pub low_celsius: f64,
    pub high_celsius: f64,
    pub sample_timeout_ms: u64,
    pub descent_hold_ms: u64,
    pub descent_rpm_per_second: f64,
    pub recovery_samples: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    RpmRange,
    TemperatureRange,
    Timeout,
    DescentRate,
    RecoveryCount,
}

impl Config {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.min_rpm.is_finite()
            || !self.max_rpm.is_finite()
            || self.min_rpm < 0.0
            || self.min_rpm > self.max_rpm
        {
            return Err(ConfigError::RpmRange);
        }
        // Research domain only; a future actuator must impose tighter model limits.
        if !self.low_celsius.is_finite()
            || !self.high_celsius.is_finite()
            || self.low_celsius < 0.0
            || self.high_celsius > 120.0
            || self.low_celsius >= self.high_celsius
        {
            return Err(ConfigError::TemperatureRange);
        }
        if self.sample_timeout_ms == 0 || self.sample_timeout_ms > 60_000 {
            return Err(ConfigError::Timeout);
        }
        if !self.descent_rpm_per_second.is_finite() || self.descent_rpm_per_second <= 0.0 {
            return Err(ConfigError::DescentRate);
        }
        if self.recovery_samples == 0 {
            return Err(ConfigError::RecoveryCount);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Verified,
    Unverified,
    Failed,
}

#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub celsius: f64,
    pub sampled_at_ms: u64,
    pub quality: Quality,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    MissingSample,
    Quality,
    NonFinite,
    OutOfRange,
    FutureSample,
    Expired,
    ReplayedSample,
    ClockRollback,
    Recovering,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Recommendation {
    /// Desired RPM only. This is neither an applied target nor a success response.
    SuggestRpm(f64),
    /// Future executor must restore firmware control; this enum performs no action.
    ReturnToAutomatic(Fault),
}

pub struct Policy {
    config: Config,
    last_now: Option<u64>,
    last_sample: Option<u64>,
    last_rpm: Option<f64>,
    cooling_since: Option<u64>,
    recovery_count: u32,
    tripped: bool,
}

impl Policy {
    pub fn new(config: Config) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self {
            config,
            last_now: None,
            last_sample: None,
            last_rpm: None,
            cooling_since: None,
            recovery_count: 0,
            tripped: true,
        })
    }

    /// Continuous floating-point curve, including exact endpoint clamping.
    /// Calling this alone does not validate a sample or update breaker state.
    pub fn preview_rpm(&self, celsius: f64) -> Option<f64> {
        if !celsius.is_finite() || !(-20.0..=127.0).contains(&celsius) {
            return None;
        }
        let c = self.config;
        let fraction =
            ((celsius - c.low_celsius) / (c.high_celsius - c.low_celsius)).clamp(0.0, 1.0);
        Some((c.min_rpm + (c.max_rpm - c.min_rpm) * fraction).clamp(c.min_rpm, c.max_rpm))
    }

    fn trip(&mut self, fault: Fault) -> Recommendation {
        self.tripped = true;
        self.recovery_count = 0;
        self.last_rpm = None;
        self.cooling_since = None;
        Recommendation::ReturnToAutomatic(fault)
    }

    /// Call every polling tick, including failed reads (None); never feed cached
    /// readings as fresh samples. All timestamps must share the same clock epoch.
    pub fn evaluate(&mut self, sample: Option<Sample>, now_ms: u64) -> Recommendation {
        let previous_now = self.last_now;
        self.last_now = Some(now_ms);
        if previous_now.is_some_and(|previous| now_ms < previous) {
            self.last_sample = None;
            return self.trip(Fault::ClockRollback);
        }
        let Some(sample) = sample else {
            return self.trip(Fault::MissingSample);
        };
        if sample.quality != Quality::Verified {
            return self.trip(Fault::Quality);
        }
        if !sample.celsius.is_finite() {
            return self.trip(Fault::NonFinite);
        }
        if !(-20.0..=127.0).contains(&sample.celsius) {
            return self.trip(Fault::OutOfRange);
        }
        if sample.sampled_at_ms > now_ms {
            return self.trip(Fault::FutureSample);
        }
        if now_ms - sample.sampled_at_ms > self.config.sample_timeout_ms {
            return self.trip(Fault::Expired);
        }
        if self
            .last_sample
            .is_some_and(|previous| sample.sampled_at_ms <= previous)
        {
            return self.trip(Fault::ReplayedSample);
        }
        self.last_sample = Some(sample.sampled_at_ms);
        if self.tripped {
            self.recovery_count = self.recovery_count.saturating_add(1);
            if self.recovery_count < self.config.recovery_samples {
                return Recommendation::ReturnToAutomatic(Fault::Recovering);
            }
            self.tripped = false;
        }
        let target = self
            .preview_rpm(sample.celsius)
            .expect("validated finite sample");
        let rpm = match self.last_rpm {
            None => target,
            Some(previous) if target >= previous => {
                self.cooling_since = None;
                target
            }
            Some(previous) => {
                let since = *self.cooling_since.get_or_insert(now_ms);
                // No release if the deadline overflows the monotonic epoch.
                let release = since.checked_add(self.config.descent_hold_ms);
                let elapsed = release
                    .map(|release| {
                        now_ms.saturating_sub(previous_now.unwrap_or(now_ms).max(release))
                    })
                    .unwrap_or(0);
                let allowed_drop = self.config.descent_rpm_per_second * (elapsed as f64 / 1000.0);
                target.max(previous - allowed_drop)
            }
        }
        .clamp(self.config.min_rpm, self.config.max_rpm);
        self.last_rpm = Some(rpm);
        Recommendation::SuggestRpm(rpm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> Config {
        Config {
            min_rpm: 1000.0,
            max_rpm: 5000.0,
            low_celsius: 40.0,
            high_celsius: 80.0,
            sample_timeout_ms: 1000,
            descent_hold_ms: 2000,
            descent_rpm_per_second: 100.0,
            recovery_samples: 1,
        }
    }
    fn sample(celsius: f64, at: u64) -> Option<Sample> {
        Some(Sample {
            celsius,
            sampled_at_ms: at,
            quality: Quality::Verified,
        })
    }
    #[test]
    fn validates_configuration() {
        for bad in [f64::NAN, f64::INFINITY, -1.0, 5001.0] {
            let mut c = config();
            c.min_rpm = bad;
            assert_eq!(c.validate(), Err(ConfigError::RpmRange));
        }
        for (lo, hi) in [
            (40.0, 40.0),
            (80.0, 40.0),
            (-1.0, 80.0),
            (40.0, 121.0),
            (f64::NAN, 80.0),
            (40.0, f64::INFINITY),
        ] {
            let mut c = config();
            c.low_celsius = lo;
            c.high_celsius = hi;
            assert_eq!(c.validate(), Err(ConfigError::TemperatureRange));
        }
        for timeout in [0, 60_001] {
            let mut c = config();
            c.sample_timeout_ms = timeout;
            assert_eq!(c.validate(), Err(ConfigError::Timeout));
        }
        let mut c = config();
        c.max_rpm = f64::INFINITY;
        assert_eq!(c.validate(), Err(ConfigError::RpmRange));
        for rate in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let mut c = config();
            c.descent_rpm_per_second = rate;
            assert_eq!(c.validate(), Err(ConfigError::DescentRate));
        }
        let mut c = config();
        c.recovery_samples = 0;
        assert_eq!(c.validate(), Err(ConfigError::RecoveryCount));
    }
    #[test]
    fn curve_is_continuous_clamped_and_supports_fixed_range() {
        let p = Policy::new(config()).unwrap();
        for (temperature, rpm) in [
            (-20.0, 1000.0),
            (40.0, 1000.0),
            (40.125, 1012.5),
            (60.0, 3000.0),
            (80.0, 5000.0),
            (127.0, 5000.0),
        ] {
            assert_eq!(p.preview_rpm(temperature), Some(rpm));
        }
        assert_eq!(p.preview_rpm(f64::NAN), None);
        let mut c = config();
        c.max_rpm = c.min_rpm;
        assert_eq!(Policy::new(c).unwrap().preview_rpm(60.0), Some(1000.0));
    }
    #[test]
    fn rejects_invalid_missing_future_expired_and_quality() {
        for (s, now, fault) in [
            (None, 0, Fault::MissingSample),
            (sample(f64::NAN, 0), 0, Fault::NonFinite),
            (sample(f64::INFINITY, 0), 0, Fault::NonFinite),
            (sample(f64::NEG_INFINITY, 0), 0, Fault::NonFinite),
            (sample(-20.01, 0), 0, Fault::OutOfRange),
            (sample(127.01, 0), 0, Fault::OutOfRange),
            (sample(60.0, 1), 0, Fault::FutureSample),
            (sample(60.0, 0), 1001, Fault::Expired),
        ] {
            assert_eq!(
                Policy::new(config()).unwrap().evaluate(s, now),
                Recommendation::ReturnToAutomatic(fault)
            );
        }
        for quality in [Quality::Failed, Quality::Unverified] {
            let s = Sample {
                celsius: 60.0,
                sampled_at_ms: 0,
                quality,
            };
            assert_eq!(
                Policy::new(config()).unwrap().evaluate(Some(s), 0),
                Recommendation::ReturnToAutomatic(Fault::Quality)
            );
        }
        assert_eq!(
            Policy::new(config())
                .unwrap()
                .evaluate(sample(60.0, 0), 1000),
            Recommendation::SuggestRpm(3000.0)
        );
    }
    #[test]
    fn rises_immediately_but_descends_only_after_hold_with_rate_limit() {
        let mut p = Policy::new(config()).unwrap();
        assert_eq!(
            p.evaluate(sample(40.0, 0), 0),
            Recommendation::SuggestRpm(1000.0)
        );
        assert_eq!(
            p.evaluate(sample(80.0, 1), 1),
            Recommendation::SuggestRpm(5000.0)
        );
        assert_eq!(
            p.evaluate(sample(40.0, 100), 100),
            Recommendation::SuggestRpm(5000.0)
        );
        assert_eq!(
            p.evaluate(sample(40.0, 2099), 2099),
            Recommendation::SuggestRpm(5000.0)
        );
        assert_eq!(
            p.evaluate(sample(40.0, 2600), 2600),
            Recommendation::SuggestRpm(4950.0)
        );
        assert_eq!(
            p.evaluate(sample(80.0, 2601), 2601),
            Recommendation::SuggestRpm(5000.0)
        );
        assert_eq!(
            p.evaluate(sample(40.0, 3000), 3000),
            Recommendation::SuggestRpm(5000.0)
        );
        assert_eq!(
            p.evaluate(sample(40.0, 5000), 5000),
            Recommendation::SuggestRpm(5000.0)
        );
        assert_eq!(
            p.evaluate(sample(40.0, 100_000), 100_000),
            Recommendation::SuggestRpm(1000.0)
        );
    }
    #[test]
    fn breaker_requires_consecutive_distinct_samples_and_resets_on_failure() {
        let mut c = config();
        c.recovery_samples = 3;
        let mut p = Policy::new(c).unwrap();
        assert_eq!(
            p.evaluate(sample(80.0, 0), 0),
            Recommendation::ReturnToAutomatic(Fault::Recovering)
        );
        assert_eq!(
            p.evaluate(sample(80.0, 1), 1),
            Recommendation::ReturnToAutomatic(Fault::Recovering)
        );
        assert_eq!(
            p.evaluate(sample(80.0, 2), 2),
            Recommendation::SuggestRpm(5000.0)
        );
        assert_eq!(
            p.evaluate(None, 3),
            Recommendation::ReturnToAutomatic(Fault::MissingSample)
        );
        assert_eq!(
            p.evaluate(sample(60.0, 4), 4),
            Recommendation::ReturnToAutomatic(Fault::Recovering)
        );
        assert_eq!(
            p.evaluate(sample(60.0, 4), 5),
            Recommendation::ReturnToAutomatic(Fault::ReplayedSample)
        );
        for at in [6, 7] {
            assert_eq!(
                p.evaluate(sample(60.0, at), at),
                Recommendation::ReturnToAutomatic(Fault::Recovering)
            );
        }
        assert_eq!(
            p.evaluate(sample(60.0, 8), 8),
            Recommendation::SuggestRpm(3000.0)
        );
    }
    #[test]
    fn clock_rollback_trips_and_requires_recovery_in_new_epoch() {
        let mut p = Policy::new(config()).unwrap();
        assert_eq!(
            p.evaluate(sample(80.0, 100), 100),
            Recommendation::SuggestRpm(5000.0)
        );
        assert_eq!(
            p.evaluate(sample(60.0, 50), 50),
            Recommendation::ReturnToAutomatic(Fault::ClockRollback)
        );
        assert_eq!(
            p.evaluate(sample(60.0, 51), 51),
            Recommendation::SuggestRpm(3000.0)
        );
        assert_eq!(
            p.evaluate(sample(60.0, 50), 52),
            Recommendation::ReturnToAutomatic(Fault::ReplayedSample)
        );
    }
    #[test]
    fn hold_deadline_overflow_never_wraps_into_a_descent() {
        let mut p = Policy::new(config()).unwrap();
        p.evaluate(sample(80.0, u64::MAX - 2), u64::MAX - 2);
        assert_eq!(
            p.evaluate(sample(40.0, u64::MAX - 1), u64::MAX - 1),
            Recommendation::SuggestRpm(5000.0)
        );
        assert_eq!(
            p.evaluate(sample(40.0, u64::MAX), u64::MAX),
            Recommendation::SuggestRpm(5000.0)
        );
    }
}
