//! Host-local sampling. No wire values, wall clock, hardware writes or registrations.
//!
//! A detached worker owns the reader. The host exchanges one owned batch through
//! an atomic pointer; it never waits for the reader or takes a sampling mutex.
use macs_fan_control_runtime::{
    policy::{Quality, Sample},
    SensorId, SensorReading,
};
use macs_fan_smc_adapter::{
    temperature::{TemperatureReader, TemperatureReading},
    Identity,
};
use std::collections::BTreeMap;
use std::ptr;
use std::sync::{
    atomic::{AtomicBool, AtomicPtr, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const PERIOD: Duration = Duration::from_millis(100);
const MAX_SOURCES: usize = 16;
// A returned call exceeding this budget is discarded. This is not cancellation
// of a synchronous syscall/IOKit call: the detached worker may remain blocked.
const MAX_READ_TIME: Duration = Duration::from_millis(250);
const CANDIDATE_PERIOD: Duration = Duration::from_secs(1);
// Canonical ID order, deliberately matching spawn's BTreeMap order. These four
// raw keys were read on Mac16,8 in evidence/temperature-reader-0.12, not calibrated.
const CANDIDATES: [(&str, &str); 4] = [
    ("smc:TAOL", "TAOL"),
    ("smc:TB0T", "TB0T"),
    ("smc:TCMb", "TCMb"),
    ("smc:Tp01", "Tp01"),
];

trait CandidateReader {
    fn read(&mut self, key: &str) -> Result<TemperatureReading, ()>;
}
impl CandidateReader for TemperatureReader {
    fn read(&mut self, key: &str) -> Result<TemperatureReading, ()> {
        TemperatureReader::read(self, key).map_err(|_| ())
    }
}

fn candidate_identity(identity: &Identity) -> bool {
    identity.model == "Mac16,8" && identity.architecture == "aarch64"
}

fn host_identity() -> Result<Identity, ()> {
    // Fixed local command, no shell, wire parameters or inherited PATH lookup.
    // This runs only in the worker, before opening any temperature connection.
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "hw.model"])
            .output()
            .map_err(|_| ())?;
        if !output.status.success() || output.stdout.len() > 256 {
            return Err(());
        }
        let text = std::str::from_utf8(&output.stdout).map_err(|_| ())?;
        let model = text.strip_suffix('\n').unwrap_or(text);
        let identity = Identity {
            model: model.into(),
            architecture: std::env::consts::ARCH.into(),
        };
        if !candidate_identity(&identity) {
            return Err(());
        }
        Ok(identity)
    }
    #[cfg(not(target_os = "macos"))]
    Err(())
}
const FAKE_ID: &str = "test:temperature";

#[derive(Clone, Debug)]
pub(super) struct Batch {
    pub readings: Vec<SensorReading>,
    pub sampled_at_ms: u64,
    pub sequence: u64,
}
impl Batch {
    fn failed(&self) -> bool {
        self.readings
            .iter()
            .any(|r| r.sample.quality == Quality::Failed)
    }
}

struct Mailbox {
    pending: AtomicPtr<Batch>,
    alive: AtomicBool,
    stop: AtomicBool,
}
impl Mailbox {
    fn take(&self) -> Option<Batch> {
        let batch = self.pending.swap(ptr::null_mut(), Ordering::AcqRel);
        if batch.is_null() {
            None
        } else {
            // SAFETY: swap transfers exclusive ownership of this Box. No code
            // borrows the pointee while it is in the slot, including publisher.
            Some(*unsafe { Box::from_raw(batch) })
        }
    }
    fn publish(&self, batch: Batch) {
        if self.stop.load(Ordering::Acquire) {
            return;
        }
        let old = self
            .pending
            .swap(Box::into_raw(Box::new(batch)), Ordering::AcqRel);
        if !old.is_null() {
            // SAFETY: the atomic exchange uniquely owns the displaced Box.
            drop(unsafe { Box::from_raw(old) });
        }
        // Pair with stop(): a stop racing the exchange cannot leave a batch.
        if self.stop.load(Ordering::Acquire) {
            drop(self.take());
        }
    }
}
impl Drop for Mailbox {
    fn drop(&mut self) {
        // Last Arc owner: no concurrent publisher or consumer can remain.
        drop(self.take());
    }
}
// Read-only checkpoints between synchronous initialization operations. A check
// never cancels an operation already in progress, and stop can race its return.
struct Initialization<'a> {
    stop: &'a AtomicBool,
    started_at: Instant,
}
impl Initialization<'_> {
    fn active(&self) -> bool {
        !self.stop.load(Ordering::Acquire) && self.started_at.elapsed() <= MAX_READ_TIME
    }
}

struct AliveGuard(Arc<Mailbox>);
impl Drop for AliveGuard {
    fn drop(&mut self) {
        self.0.alive.store(false, Ordering::Release);
    }
}

pub(super) struct Sampler {
    sources: Vec<(String, bool)>,
    mailbox: Arc<Mailbox>,
    worker: Option<JoinHandle<()>>,
}
impl Sampler {
    pub(super) fn unavailable() -> Self {
        Self {
            sources: vec![],
            mailbox: Arc::new(Mailbox {
                pending: AtomicPtr::new(ptr::null_mut()),
                alive: AtomicBool::new(false),
                stop: AtomicBool::new(false),
            }),
            worker: None,
        }
    }

    pub(super) fn fake(origin: Instant) -> Self {
        // Explicit test-backend admission only. There is no production
        // calibration/admission table; SMC directory/raw values are not trusted.
        Self::spawn(origin, vec![(FAKE_ID.into(), true)], PERIOD, || {
            Ok(vec![60.0])
        })
    }

    pub(super) fn supports(&self, id: &str) -> bool {
        self.is_alive()
            && self
                .sources
                .iter()
                .any(|(source, admitted)| *admitted && source == id)
    }

    pub(super) fn candidates(origin: Instant) -> Self {
        Self::candidate_with(origin, host_identity, || {
            TemperatureReader::open().map_err(|_| ())
        })
    }

    // The factory crosses the thread boundary; the reader it constructs does
    // not. In particular, TemperatureReader needs no Send assertion or wrapper.
    fn candidate_with<I, O, R>(origin: Instant, identity: I, open: O) -> Self
    where
        I: FnOnce() -> Result<Identity, ()> + Send + 'static,
        O: FnOnce() -> Result<R, ()> + Send + 'static,
        R: CandidateReader + 'static,
    {
        Self::spawn_initialized(
            origin,
            CANDIDATES
                .iter()
                .map(|(id, _)| ((*id).into(), false))
                .collect(),
            CANDIDATE_PERIOD,
            move |initialization| {
                let expected = identity()?;
                // Identity may block. Do not start another I/O operation after
                // it returns if cancellation or the initialization budget is known.
                if !candidate_identity(&expected) || !initialization.active() {
                    return Err(());
                }
                let mut reader = open()?;
                Ok(move || {
                    let mut values = Vec::with_capacity(CANDIDATES.len());
                    for (_, key) in CANDIDATES {
                        let reading = reader.read(key)?;
                        if reading.identity != expected
                            || reading.key != key
                            || reading.suspect
                            || !reading.candidate_celsius.is_finite()
                            || reading.candidate_celsius == 0.0
                            || !(-40.0..=150.0).contains(&reading.candidate_celsius)
                        {
                            return Err(());
                        }
                        values.push(reading.candidate_celsius);
                    }
                    Ok(values)
                })
            },
        )
    }

    pub(super) fn stop(&mut self) {
        self.mailbox.stop.store(true, Ordering::Release);
        drop(self.mailbox.take());
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            // A hung initialization/read cannot delay host stop or drop.
            drop(worker);
        }
    }

    pub(super) fn is_alive(&self) -> bool {
        !self.mailbox.stop.load(Ordering::Acquire) && self.mailbox.alive.load(Ordering::Acquire)
    }

    pub(super) fn latest(&mut self) -> Option<Batch> {
        let batch = self.mailbox.take()?;
        // A panicked/exited reader must not leave a usable successful cache.
        if self.mailbox.stop.load(Ordering::Acquire) || (!self.is_alive() && !batch.failed()) {
            None
        } else {
            Some(batch)
        }
    }

    // Only host-internal constructors can admit sources. The reader supplies
    // values in the canonical source order, never timestamps, IDs or quality.
    fn spawn<F>(origin: Instant, sources: Vec<(String, bool)>, period: Duration, read: F) -> Self
    where
        F: FnMut() -> Result<Vec<f64>, ()> + Send + 'static,
    {
        Self::spawn_initialized(origin, sources, period, move |_| Ok(read))
    }

    fn spawn_initialized<I, F>(
        origin: Instant,
        sources: Vec<(String, bool)>,
        period: Duration,
        initialize: I,
    ) -> Self
    where
        I: FnOnce(&Initialization<'_>) -> Result<F, ()> + Send + 'static,
        F: FnMut() -> Result<Vec<f64>, ()> + 'static,
    {
        let mut canonical = BTreeMap::new();
        for (id, admitted) in sources {
            if id.is_empty() {
                return Self::unavailable();
            }
            canonical
                .entry(id)
                .and_modify(|v| *v &= admitted)
                .or_insert(admitted);
        }
        if canonical.is_empty() || canonical.len() > MAX_SOURCES {
            return Self::unavailable();
        }
        let sources: Vec<_> = canonical.into_iter().collect();
        let admitted_sources = sources.clone();
        let mailbox = Arc::new(Mailbox {
            pending: AtomicPtr::new(ptr::null_mut()),
            alive: AtomicBool::new(true),
            stop: AtomicBool::new(false),
        });
        let shared = mailbox.clone();
        let worker = thread::Builder::new()
            .name("local-temperature-sampler".into())
            .spawn(move || {
                let _alive = AliveGuard(shared.clone());
                let initialization = Initialization {
                    stop: &shared.stop,
                    started_at: Instant::now(),
                };
                if !initialization.active() {
                    return;
                }
                let Ok(mut read) = initialize(&initialization) else {
                    return;
                };
                if !initialization.active() {
                    return;
                }
                let mut sequence = 0u64;
                let mut previous_start = None;
                while !shared.stop.load(Ordering::Acquire) {
                    let sampled_at_ms =
                        u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX);
                    if previous_start.is_some_and(|last| sampled_at_ms <= last) {
                        // Wait for a real clock advance; never manufacture last+1.
                        thread::park_timeout(Duration::from_millis(1));
                        continue;
                    }
                    previous_start = Some(sampled_at_ms);
                    let read_started = Instant::now();
                    let values = read(); // Potentially blocking; exclusively worker-owned.
                    if shared.stop.load(Ordering::Acquire) || read_started.elapsed() > MAX_READ_TIME
                    {
                        break;
                    }
                    let values = values.ok().filter(|v| {
                        v.len() == sources.len()
                            && v.iter().zip(&sources).all(|(c, (_, admitted))| {
                                c.is_finite()
                                    && if *admitted {
                                        (-20.0..=127.0).contains(c)
                                    } else {
                                        *c != 0.0 && (-40.0..=150.0).contains(c)
                                    }
                            })
                    });
                    let failed = values.is_none();
                    sequence = match sequence.checked_add(1) {
                        Some(n) => n,
                        None => break,
                    };
                    let readings = sources
                        .iter()
                        .enumerate()
                        .map(|(index, (id, admitted))| SensorReading {
                            sensor: SensorId(id.clone()),
                            sample: Sample {
                                celsius: values.as_ref().map_or(f64::NAN, |v| v[index]),
                                sampled_at_ms,
                                quality: if failed {
                                    Quality::Failed
                                } else if *admitted {
                                    Quality::Verified
                                } else {
                                    Quality::Unverified
                                },
                            },
                        })
                        .collect();
                    shared.publish(Batch {
                        readings,
                        sampled_at_ms,
                        sequence,
                    });
                    // Failure is terminal, so an unobserved failure cannot be
                    // overwritten with later success. Explicit new host required.
                    if failed {
                        break;
                    }
                    thread::park_timeout(period);
                }
            });
        match worker {
            Ok(worker) => Self {
                sources: admitted_sources,
                mailbox,
                worker: Some(worker),
            },
            Err(_) => Self::unavailable(),
        }
    }

    #[cfg(test)]
    pub(super) fn test_candidates(origin: Instant) -> Self {
        Self::candidate_with(origin, || Ok(test_identity()), || Ok(TestCandidateReader))
    }

    #[cfg(test)]
    pub(super) fn test_candidate_initializing(
        origin: Instant,
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> Self {
        Self::candidate_with(
            origin,
            || Ok(test_identity()),
            move || {
                entered.send(()).map_err(|_| ())?;
                release.recv().map_err(|_| ())?;
                Ok(TestCandidateReader)
            },
        )
    }

    #[cfg(test)]
    pub(super) fn test_controlled(origin: Instant) -> (Self, TestControl) {
        let control = TestControl(Arc::new(std::sync::atomic::AtomicU8::new(0)));
        let reader = control.clone();
        let sampler = Self::spawn(origin, vec![(FAKE_ID.into(), true)], PERIOD, move || loop {
            match reader.0.load(Ordering::Acquire) {
                0 => return Ok(vec![60.0]),
                1 => return Err(()),
                2 => thread::sleep(Duration::from_millis(1)),
                _ => panic!("injected sampler panic"),
            }
        });
        (sampler, control)
    }
}
impl Drop for Sampler {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
fn test_identity() -> Identity {
    Identity {
        model: "Mac16,8".into(),
        architecture: "aarch64".into(),
    }
}

#[cfg(test)]
struct TestCandidateReader;
#[cfg(test)]
impl CandidateReader for TestCandidateReader {
    fn read(&mut self, key: &str) -> Result<TemperatureReading, ()> {
        let index = CANDIDATES.iter().position(|(_, k)| *k == key).ok_or(())?;
        Ok(TemperatureReading {
            identity: test_identity(),
            key: key.into(),
            metadata: macs_fan_smc_adapter::Metadata {
                kind: *b"flt ",
                size: 4,
                attributes: 0,
            },
            candidate_celsius: 30.0 + index as f64,
            suspect: false,
        })
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(super) struct TestControl(Arc<std::sync::atomic::AtomicU8>);
#[cfg(test)]
impl TestControl {
    pub(super) fn fail(&self) {
        self.0.store(1, Ordering::Release);
    }
    pub(super) fn hang(&self) {
        self.0.store(2, Ordering::Release);
    }
    pub(super) fn resume(&self) {
        self.0.store(0, Ordering::Release);
    }
    pub(super) fn panic(&self) {
        self.0.store(3, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn wait_for(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !condition() {
            assert!(Instant::now() < deadline, "sampler condition timed out");
            thread::sleep(Duration::from_millis(1));
        }
    }
    fn first(sampler: &mut Sampler) -> Batch {
        let mut batch = None;
        wait_for(|| {
            batch = sampler.latest();
            batch.is_some()
        });
        batch.unwrap()
    }
    fn one(
        origin: Instant,
        read: impl FnMut() -> Result<Vec<f64>, ()> + Send + 'static,
    ) -> Sampler {
        Sampler::spawn(
            origin,
            vec![(FAKE_ID.into(), true)],
            Duration::from_secs(60),
            read,
        )
    }

    #[test]
    fn fake_has_exact_host_local_source_and_same_epoch() {
        let origin = Instant::now() - Duration::from_millis(50);
        let mut sampler = Sampler::fake(origin);
        assert!(sampler.supports(FAKE_ID));
        for id in [
            "temperature",
            "test:Temperature",
            "test:temperature ",
            "test:temperature:other",
        ] {
            assert!(!sampler.supports(id));
        }
        let batch = first(&mut sampler);
        assert_eq!(batch.readings.len(), 1);
        assert_eq!(batch.readings[0].sensor.0, FAKE_ID);
        assert_eq!(batch.readings[0].sample.celsius, 60.0);
        assert_eq!(batch.readings[0].sample.quality, Quality::Verified);
        assert!(batch.sampled_at_ms >= 50);
        assert!(batch.sampled_at_ms <= origin.elapsed().as_millis() as u64);
        assert_eq!(batch.sampled_at_ms, batch.readings[0].sample.sampled_at_ms);
    }

    #[test]
    fn production_unavailable_has_no_admitted_source() {
        let mut sampler = Sampler::unavailable();
        assert!(!sampler.supports(FAKE_ID));
        assert!(!sampler.is_alive());
        assert!(sampler.latest().is_none());
    }

    #[test]
    fn latest_consumes_once_without_restamping() {
        let mut sampler = one(Instant::now(), || Ok(vec![60.0]));
        let batch = first(&mut sampler);
        for _ in 0..1000 {
            assert!(sampler.latest().is_none());
        }
        assert_eq!(batch.sequence, 1);
    }

    #[test]
    fn late_read_keeps_read_start_timestamp_and_poll_never_waits() {
        let origin = Instant::now();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut sampler = one(origin, move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(vec![60.0])
        });
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let entered_at = origin.elapsed().as_millis() as u64;
        let poll_started = Instant::now();
        for _ in 0..1000 {
            assert!(sampler.latest().is_none());
        }
        assert!(poll_started.elapsed() < Duration::from_millis(100));
        thread::sleep(Duration::from_millis(35));
        release_tx.send(()).unwrap();
        let batch = first(&mut sampler);
        assert!(batch.sampled_at_ms <= entered_at);
        assert!(origin.elapsed().as_millis() as u64 - batch.sampled_at_ms >= 35);
    }

    #[test]
    fn dropping_hung_reader_does_not_join() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let sampler = one(Instant::now(), move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(vec![60.0])
        });
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let mailbox = sampler.mailbox.clone();
        let before = Instant::now();
        drop(sampler);
        assert!(before.elapsed() < Duration::from_millis(100));
        assert!(mailbox.alive.load(Ordering::Acquire));
        release_tx.send(()).unwrap();
        wait_for(|| !mailbox.alive.load(Ordering::Acquire));
        assert!(
            mailbox.take().is_none(),
            "cancelled late read must not publish"
        );
    }

    #[test]
    fn read_failure_is_complete_terminal_and_cannot_be_overwritten() {
        let mut sampler = one(Instant::now(), || Err(()));
        wait_for(|| !sampler.is_alive());
        let batch = first(&mut sampler);
        assert_eq!(batch.readings.len(), 1);
        assert_eq!(batch.readings[0].sample.quality, Quality::Failed);
        assert_eq!(batch.sequence, 1);
        assert!(sampler.latest().is_none());
    }

    #[test]
    fn invalid_temperature_is_terminal_even_if_reader_would_recover() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -20.01, 127.01] {
            let mut calls = 0;
            let mut sampler = one(Instant::now(), move || {
                calls += 1;
                Ok(vec![if calls == 1 { value } else { 60.0 }])
            });
            wait_for(|| !sampler.is_alive());
            let batch = first(&mut sampler);
            assert_eq!(batch.sequence, 1);
            assert_eq!(batch.readings[0].sample.quality, Quality::Failed);
        }
    }

    #[test]
    fn panic_discards_previously_published_success() {
        let mut reads = 0;
        let mut sampler = Sampler::spawn(
            Instant::now(),
            vec![(FAKE_ID.into(), true)],
            Duration::from_millis(1),
            move || {
                reads += 1;
                assert_eq!(reads, 1, "injected panic after successful publication");
                Ok(vec![60.0])
            },
        );
        wait_for(|| !sampler.is_alive());
        assert!(sampler.latest().is_none());
    }

    #[test]
    fn source_deduplication_is_bounded_and_never_upgrades_quality() {
        let mut sampler = Sampler::spawn(
            Instant::now(),
            vec![("raw".into(), true), ("raw".into(), false)],
            PERIOD,
            || Ok(vec![60.0]),
        );
        let batch = first(&mut sampler);
        assert_eq!(batch.readings.len(), 1);
        assert_eq!(batch.readings[0].sample.quality, Quality::Unverified);
        assert!(!sampler.supports("raw"));
        let excessive = (0..17).map(|i| (format!("source:{i}"), true)).collect();
        let sampler = Sampler::spawn(Instant::now(), excessive, PERIOD, || {
            panic!("must not spawn")
        });
        assert!(!sampler.is_alive());
        let mut incomplete = one(Instant::now(), || Ok(vec![]));
        assert_eq!(
            first(&mut incomplete).readings[0].sample.quality,
            Quality::Failed
        );
    }

    // Rc deliberately makes this reader !Send. Only its factory is moved.
    struct LocalCandidate {
        local: std::rc::Rc<std::cell::Cell<usize>>,
        value: Option<f64>,
        mismatch: u8,
    }
    impl LocalCandidate {
        fn new(value: Option<f64>, mismatch: u8) -> Self {
            Self {
                local: std::rc::Rc::new(std::cell::Cell::new(0)),
                value,
                mismatch,
            }
        }
    }
    impl CandidateReader for LocalCandidate {
        fn read(&mut self, key: &str) -> Result<TemperatureReading, ()> {
            self.local.set(self.local.get() + 1);
            let mut reading = TestCandidateReader.read(key)?;
            if let Some(value) = self.value {
                reading.candidate_celsius = value;
            }
            match self.mismatch {
                1 => reading.identity.model = "Mac99,1".into(),
                2 => reading.identity.architecture = "x86_64".into(),
                3 => reading.key = "T???".into(),
                4 => reading.suspect = true,
                _ => {}
            }
            Ok(reading)
        }
    }
    fn candidate(value: Option<f64>, mismatch: u8) -> Sampler {
        Sampler::candidate_with(
            Instant::now(),
            || Ok(test_identity()),
            move || Ok(LocalCandidate::new(value, mismatch)),
        )
    }

    #[test]
    fn candidates_are_exactly_mapped_unverified_and_never_admitted() {
        let origin = Instant::now() - Duration::from_millis(50);
        let caller = thread::current().id();
        let (thread_tx, thread_rx) = mpsc::channel();
        let mut sampler = Sampler::candidate_with(
            origin,
            || Ok(test_identity()),
            move || {
                thread_tx.send(thread::current().id()).unwrap();
                Ok(LocalCandidate::new(None, 0))
            },
        );
        assert_ne!(
            caller,
            thread_rx.recv_timeout(Duration::from_secs(3)).unwrap()
        );
        let batch = first(&mut sampler);
        assert_eq!(batch.readings.len(), 4);
        assert!(batch.sampled_at_ms >= 50);
        assert!(batch.sampled_at_ms <= origin.elapsed().as_millis() as u64);
        for (index, ((id, _), reading)) in CANDIDATES.iter().zip(&batch.readings).enumerate() {
            assert_eq!(&reading.sensor.0, id);
            assert_eq!(reading.sample.celsius, 30.0 + index as f64);
            assert_eq!(reading.sample.quality, Quality::Unverified);
            assert_eq!(reading.sample.sampled_at_ms, batch.sampled_at_ms);
            assert!(!sampler.supports(id));
        }
    }

    #[test]
    fn unknown_model_architecture_or_identity_failure_never_opens_reader() {
        for identity in [
            Err(()),
            Ok(Identity {
                model: "Mac16,9".into(),
                ..test_identity()
            }),
            Ok(Identity {
                model: "Mac16,8 ".into(),
                ..test_identity()
            }),
            Ok(Identity {
                architecture: "x86_64".into(),
                ..test_identity()
            }),
        ] {
            let opened = Arc::new(AtomicBool::new(false));
            let worker_opened = opened.clone();
            let mut sampler = Sampler::candidate_with(
                Instant::now(),
                move || identity,
                move || {
                    worker_opened.store(true, Ordering::Release);
                    Ok(TestCandidateReader)
                },
            );
            wait_for(|| !sampler.is_alive());
            assert!(!opened.load(Ordering::Acquire));
            assert!(sampler.latest().is_none());
            assert!(!sampler.supports("smc:Tp01"));
        }
    }

    #[test]
    fn candidate_identity_returning_after_stop_never_opens_reader() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let opened = Arc::new(AtomicBool::new(false));
        let worker_opened = opened.clone();
        let mut sampler = Sampler::candidate_with(
            Instant::now(),
            move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(test_identity())
            },
            move || {
                worker_opened.store(true, Ordering::Release);
                Ok(TestCandidateReader)
            },
        );
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        sampler.stop();
        release_tx.send(()).unwrap();
        wait_for(|| !sampler.mailbox.alive.load(Ordering::Acquire));
        assert!(!opened.load(Ordering::Acquire));
        assert!(sampler.latest().is_none());
        assert!(sampler.mailbox.take().is_none());
    }

    #[test]
    fn candidate_identity_returning_over_budget_never_opens_reader() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let opened = Arc::new(AtomicBool::new(false));
        let worker_opened = opened.clone();
        let mut sampler = Sampler::candidate_with(
            Instant::now(),
            move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(test_identity())
            },
            move || {
                worker_opened.store(true, Ordering::Release);
                Ok(TestCandidateReader)
            },
        );
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        thread::sleep(MAX_READ_TIME + Duration::from_millis(20));
        release_tx.send(()).unwrap();
        wait_for(|| !sampler.is_alive());
        assert!(!opened.load(Ordering::Acquire));
        assert!(sampler.latest().is_none());
        assert!(sampler.mailbox.take().is_none());
    }

    #[test]
    fn candidate_initialization_failure_has_no_sample_or_admission() {
        let mut sampler = Sampler::candidate_with(
            Instant::now(),
            || Ok(test_identity()),
            || Err::<TestCandidateReader, _>(()),
        );
        wait_for(|| !sampler.is_alive());
        assert!(sampler.latest().is_none());
        assert!(!sampler.supports("smc:Tp01"));
    }

    #[test]
    fn changed_identity_key_or_suspect_read_is_terminal_failed_batch() {
        for mismatch in 1..=4 {
            let mut sampler = candidate(None, mismatch);
            wait_for(|| !sampler.is_alive());
            let batch = first(&mut sampler);
            assert_eq!(batch.readings.len(), 4);
            assert!(batch
                .readings
                .iter()
                .all(|r| r.sample.quality == Quality::Failed && r.sample.celsius.is_nan()));
            assert!(sampler.latest().is_none());
        }
    }

    #[test]
    fn candidate_range_preserves_real_boundaries_but_rejects_zero_and_nonfinite() {
        for value in [-40.0, -20.01, 127.01, 150.0] {
            let mut sampler = candidate(Some(value), 0);
            let batch = first(&mut sampler);
            assert!(batch
                .readings
                .iter()
                .all(|r| r.sample.celsius == value && r.sample.quality == Quality::Unverified));
            assert!(!sampler.supports("smc:Tp01"));
        }
        for value in [
            0.0,
            -0.0,
            -40.01,
            150.01,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ] {
            let mut sampler = candidate(Some(value), 0);
            wait_for(|| !sampler.is_alive());
            assert!(first(&mut sampler)
                .readings
                .iter()
                .all(|r| r.sample.quality == Quality::Failed));
        }
    }

    #[test]
    fn overdue_return_is_not_published_or_retried() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut sampler = one(Instant::now(), move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(vec![60.0])
        });
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        thread::sleep(MAX_READ_TIME + Duration::from_millis(20));
        release_tx.send(()).unwrap();
        wait_for(|| !sampler.is_alive());
        assert!(sampler.latest().is_none());
        assert!(!sampler.supports(FAKE_ID));
    }

    #[test]
    fn overdue_initialization_never_starts_a_read() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let read_called = Arc::new(AtomicBool::new(false));
        let worker_read_called = read_called.clone();
        let mut sampler = Sampler::spawn_initialized(
            Instant::now(),
            vec![(FAKE_ID.into(), true)],
            PERIOD,
            move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(move || {
                    worker_read_called.store(true, Ordering::Release);
                    Ok(vec![60.0])
                })
            },
        );
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        thread::sleep(MAX_READ_TIME + Duration::from_millis(20));
        release_tx.send(()).unwrap();
        wait_for(|| !sampler.is_alive());
        assert!(!read_called.load(Ordering::Acquire));
        assert!(sampler.latest().is_none());
        assert!(!sampler.supports(FAKE_ID));
    }

    #[test]
    fn drop_during_initialization_is_nonblocking_and_cannot_publish() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let sampler = Sampler::test_candidate_initializing(Instant::now(), entered_tx, release_rx);
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let mailbox = sampler.mailbox.clone();
        let start = Instant::now();
        drop(sampler);
        assert!(start.elapsed() < Duration::from_millis(100));
        release_tx.send(()).unwrap();
        wait_for(|| !mailbox.alive.load(Ordering::Acquire));
        assert!(mailbox.take().is_none());
    }

    #[test]
    fn stop_clears_pending_batch_and_never_restarts() {
        let mut sampler = Sampler::test_candidates(Instant::now());
        wait_for(|| !sampler.mailbox.pending.load(Ordering::Acquire).is_null());
        sampler.stop();
        assert!(sampler.latest().is_none());
        assert!(!sampler.is_alive());
        assert!(!sampler.supports("smc:Tp01"));
    }

    #[test]
    fn one_slot_overwrites_without_queuing_work_or_batches() {
        let mailbox = Mailbox {
            pending: AtomicPtr::new(ptr::null_mut()),
            alive: AtomicBool::new(true),
            stop: AtomicBool::new(false),
        };
        for sequence in 1..=10_000 {
            mailbox.publish(Batch {
                readings: vec![],
                sampled_at_ms: 0,
                sequence,
            });
        }
        assert_eq!(mailbox.take().unwrap().sequence, 10_000);
        assert!(mailbox.take().is_none());
    }

    #[test]
    fn simultaneous_publish_and_take_transfer_each_box_only_once() {
        let mailbox = Arc::new(Mailbox {
            pending: AtomicPtr::new(ptr::null_mut()),
            alive: AtomicBool::new(true),
            stop: AtomicBool::new(false),
        });
        let publisher = mailbox.clone();
        let worker = thread::spawn(move || {
            for sequence in 1..=20_000 {
                publisher.publish(Batch {
                    readings: vec![],
                    sampled_at_ms: sequence,
                    sequence,
                });
            }
            publisher.alive.store(false, Ordering::Release);
        });
        let mut last = 0;
        while mailbox.alive.load(Ordering::Acquire) {
            if let Some(batch) = mailbox.take() {
                assert!(batch.sequence > last);
                assert_eq!(batch.sequence, batch.sampled_at_ms);
                last = batch.sequence;
            }
            thread::yield_now();
        }
        worker.join().unwrap(); // Test publisher cannot perform blocking I/O.
        if let Some(batch) = mailbox.take() {
            assert!(batch.sequence > last);
            last = batch.sequence;
        }
        assert_eq!(last, 20_000);
    }

    #[test]
    fn rapid_sampling_waits_for_real_milliseconds_never_future_last_plus_one() {
        let origin = Instant::now();
        let mut sampler =
            Sampler::spawn(origin, vec![(FAKE_ID.into(), true)], Duration::ZERO, || {
                Ok(vec![60.0])
            });
        let mut last = None;
        for _ in 0..20 {
            let batch = first(&mut sampler);
            assert!(batch.sampled_at_ms <= origin.elapsed().as_millis() as u64);
            if let Some((sequence, at)) = last {
                assert!(batch.sequence > sequence);
                assert!(batch.sampled_at_ms > at);
            }
            last = Some((batch.sequence, batch.sampled_at_ms));
        }
    }
}
