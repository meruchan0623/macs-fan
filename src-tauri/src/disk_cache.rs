//! Disk I/O cadence inside the isolated sampler process. Cached batches retain
//! their original read-start timestamp; callers must enforce DISPLAY_TTL_MS.
use crate::disk;
use std::time::{Duration, Instant};

const INTERVAL: Duration = Duration::from_secs(30);
pub const DISPLAY_TTL_MS: u64 = 45_000;

#[derive(Default)]
pub struct Sampler {
    last_started: Option<Instant>,
    batch: Option<disk::Batch>,
}

impl Sampler {
    pub fn sample(&mut self) -> (disk::Batch, bool) {
        self.sample_with(Instant::now(), disk::sample)
    }

    fn sample_with(
        &mut self,
        now: Instant,
        read: impl FnOnce() -> disk::Batch,
    ) -> (disk::Batch, bool) {
        let due = self.last_started.is_none_or(|last| {
            now.checked_duration_since(last)
                .is_none_or(|age| age >= INTERVAL)
        });
        let cached = !due && self.batch.is_some();
        if !cached {
            self.last_started = Some(now);
            // Failures replace the previous batch, including all previous values.
            self.batch = Some(read());
        }
        (self.batch.as_ref().expect("sampled batch").clone(), cached)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn batch(time: u64, available: bool) -> disk::Batch {
        disk::Batch {
            supported: true,
            available,
            captured_at_ms: time,
            service_count: 1,
            truncated: false,
            readings: vec![disk::Reading {
                source_id: "disk:nvme:registry:0000000000000001".into(),
                registry_id: 1,
                name: Some("SSD".into()),
                transport: "nvme".into(),
                temperature_c: available.then_some(30.0),
                quality: if available {
                    disk::Quality::Valid
                } else {
                    disk::Quality::Unavailable
                },
                message: None,
            }],
            diagnostics: vec![],
        }
    }

    #[test]
    fn interval_keeps_read_start_and_refreshes_at_boundary() {
        let start = Instant::now();
        let mut sampler = Sampler::default();
        let (first, cached) = sampler.sample_with(start, || batch(1000, true));
        assert!(!cached);
        for delta in [0, 2, 29] {
            let (current, cached) = sampler.sample_with(start + Duration::from_secs(delta), || {
                panic!("I/O before cadence")
            });
            assert!(cached);
            assert_eq!(current.captured_at_ms, first.captured_at_ms);
        }
        let (next, cached) = sampler.sample_with(start + INTERVAL, || batch(31000, true));
        assert!(!cached);
        assert_eq!(next.captured_at_ms, 31000);
    }

    #[test]
    fn error_replaces_success_and_is_also_rate_limited() {
        let start = Instant::now();
        let mut sampler = Sampler::default();
        sampler.sample_with(start, || batch(1000, true));
        let (failed, cached) = sampler.sample_with(start + INTERVAL, || batch(31000, false));
        assert!(!cached);
        assert!(!failed.available);
        assert_eq!(failed.readings[0].temperature_c, None);
        let (retry, cached) = sampler
            .sample_with(start + INTERVAL + Duration::from_secs(2), || {
                panic!("No tight error retry")
            });
        assert!(cached);
        assert!(!retry.available);
        assert_eq!(retry.readings[0].temperature_c, None);
    }

    #[test]
    fn independent_output_clock_reversal_and_long_pause() {
        let start = Instant::now();
        let mut sampler = Sampler::default();
        let (mut exported, _) = sampler.sample_with(start + INTERVAL, || batch(31000, true));
        exported.readings.clear();
        let (retained, cached) = sampler.sample_with(start + INTERVAL, || panic!("cached"));
        assert!(cached);
        assert_eq!(retained.readings.len(), 1);
        let (reset, cached) = sampler.sample_with(start, || batch(1000, false));
        assert!(!cached);
        assert_eq!(reset.captured_at_ms, 1000);
        let (resumed, cached) =
            sampler.sample_with(start + Duration::from_secs(3600), || batch(3601000, true));
        assert!(!cached);
        assert_eq!(resumed.captured_at_ms, 3601000);
    }
}
