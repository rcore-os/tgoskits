//! Kernel random number generator.
//!
//! Follows the readiness contract of Linux `drivers/char/random.c` (v7.2-rc3):
//! an input pool counts credited entropy bits, and the ChaCha20 CRNG it keys is
//! ready only after `POOL_READY_BITS` have been credited. Firmware seeds and
//! CPU random instructions are credited; timestamps and user writes are mixed
//! in without credit. `getrandom()` and `/dev/random` wait for readiness, while
//! `/dev/urandom` and in-kernel users such as `AT_RANDOM` never wait.
//!
//! SHA-256 stands in for Linux's BLAKE2s. Without firmware or CPU entropy, a
//! waiting `getrandom()` caller or an unseeded `/dev/urandom` reader credits
//! scheduling jitter as `try_to_generate_entropy()` does, sampling the clock
//! at tick boundaries in place of Linux's timer callback.

mod arch;

use core::{
    sync::atomic::{AtomicBool, AtomicU32, Ordering},
    time::Duration,
};

use ax_lazyinit::OnceLock;
use axpoll::{IoEvents, Pollable, SharedRegistrationSink};
use axpoll_set::PollSet;
use linux_raw_sys::general::{GRND_INSECURE, GRND_NONBLOCK, GRND_RANDOM};
use rand::{Rng, SeedableRng, rngs::ChaCha20Rng};
use sha2::{Digest, Sha256};

use crate::{
    StarryError, StarryResult,
    sync::Mutex,
    task::{
        UserTaskRef,
        future::{UserWaitOutcome, block_on_user_timeout, poll_io},
        try_current_user_task, yield_now,
    },
};

const NANOS_PER_SEC: u64 = 1_000_000_000;

/// `POOL_BITS`: the width of the pool hash.
const POOL_BITS: u32 = 256;
/// `POOL_READY_BITS`.
const POOL_READY_BITS: u32 = POOL_BITS;
/// `POOL_EARLY_BITS`.
const POOL_EARLY_BITS: u32 = POOL_READY_BITS / 2;
/// `CRNG_RESEED_START_INTERVAL`.
const CRNG_RESEED_START_INTERVAL: u64 = NANOS_PER_SEC;
/// `CRNG_RESEED_INTERVAL`.
const CRNG_RESEED_INTERVAL: u64 = 60 * NANOS_PER_SEC;
/// `random_init_early()` gathers one hash block of CPU words.
const EARLY_CPU_WORDS: usize = 64 / size_of::<u64>();
/// `maxwarn` in `urandom_read_iter()`.
const URANDOM_MAX_WARN: u32 = 10;
/// `HZ`, matching the `AT_CLKTCK` StarryOS reports.
const HZ: u64 = 100;
const TICK_NANOS: u64 = NANOS_PER_SEC / HZ;
/// `NUM_TRIAL_SAMPLES` in `try_to_generate_entropy()`.
const NUM_TRIAL_SAMPLES: u64 = 8192;
/// `MAX_SAMPLES_PER_BIT`: a coarser cycle counter carries too little jitter.
const MAX_SAMPLES_PER_BIT: u64 = HZ / 15;

/// Entropy inputs outside the pool.
trait Sources {
    /// `arch_get_random_seed_longs()` falling back to `arch_get_random_longs()`.
    fn cpu_word(&self) -> Option<u64>;
    /// `random_get_entropy()`.
    fn cycles(&self) -> u64;
}

struct Platform;

impl Sources for Platform {
    fn cpu_word(&self) -> Option<u64> {
        arch::random_long()
    }

    fn cycles(&self) -> u64 {
        uptime()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CrngInit {
    Empty,
    Early,
    Ready,
}

/// Linux `input_pool` and `base_crng`, kept under one lock.
struct Crng {
    pool: Sha256,
    init_bits: u32,
    init: CrngInit,
    key: [u8; 32],
    reseeded_at: u64,
}

impl Crng {
    fn new() -> Self {
        Self {
            pool: Sha256::new(),
            init_bits: 0,
            init: CrngInit::Empty,
            key: [0; 32],
            reseeded_at: 0,
        }
    }

    fn is_ready(&self) -> bool {
        self.init == CrngInit::Ready
    }

    /// `mix_pool_bytes()`: adds input without crediting it.
    fn mix(&mut self, buf: &[u8]) {
        self.pool.update(buf);
    }

    /// `extract_entropy()`: the pool hash keys the output and the next pool.
    fn extract(&mut self, src: &impl Sources) -> [u8; 32] {
        let mut block = [0; 40];
        for word in block[..32].as_chunks_mut::<{ size_of::<u64>() }>().0 {
            *word = src.cpu_word().unwrap_or_else(|| src.cycles()).to_ne_bytes();
        }
        let seed: [u8; 32] = self.pool.finalize_reset().into();
        self.pool.update(prf(&seed, &block));
        block[32..].copy_from_slice(&1u64.to_ne_bytes());
        prf(&seed, &block)
    }

    /// `crng_reseed()`.
    fn reseed(&mut self, src: &impl Sources, now: u64) {
        self.key = self.extract(src);
        self.init = CrngInit::Ready;
        self.reseeded_at = now;
    }

    /// `_credit_init_bits()`; returns whether this call made the CRNG ready.
    fn credit_init_bits(&mut self, bits: usize, src: &impl Sources, now: u64) -> bool {
        if bits == 0 || self.is_ready() {
            return false;
        }
        let orig = self.init_bits;
        let new = (orig + bits.min(POOL_BITS as usize) as u32).min(POOL_BITS);
        self.init_bits = new;
        if orig < POOL_READY_BITS && new >= POOL_READY_BITS {
            self.reseed(src, now);
            return true;
        }
        if orig < POOL_EARLY_BITS && new >= POOL_EARLY_BITS && self.init == CrngInit::Empty {
            self.key = self.extract(src);
            self.init = CrngInit::Early;
        }
        false
    }

    /// `add_bootloader_randomness()` with `random.trust_bootloader=on`.
    fn add_bootloader_randomness(&mut self, seed: &[u8], src: &impl Sources, now: u64) -> bool {
        self.mix(seed);
        self.credit_init_bits(seed.len().saturating_mul(8), src, now)
    }

    /// `random_init_early()` with `random.trust_cpu=on`.
    fn init_early(&mut self, src: &impl Sources, now: u64) -> bool {
        let mut cpu_bits = EARLY_CPU_WORDS * u64::BITS as usize;
        for _ in 0..EARLY_CPU_WORDS {
            match src.cpu_word() {
                Some(word) => self.mix(&word.to_ne_bytes()),
                None => cpu_bits -= u64::BITS as usize,
            }
        }
        if self.is_ready() {
            self.reseed(src, now);
            false
        } else {
            self.credit_init_bits(cpu_bits, src, now)
        }
    }

    /// `random_init()`: timestamps are mixed in but never credited.
    fn init_late(&mut self, src: &impl Sources, now: u64, wall: u64) {
        self.mix(&wall.to_ne_bytes());
        self.mix(&src.cycles().to_ne_bytes());
        if self.is_ready() {
            self.reseed(src, now);
        }
    }

    /// `crng_make_state()`: the key is replaced before the stream is handed out.
    fn make_state(&mut self, src: &impl Sources, now: u64) -> ChaCha20Rng {
        match self.init {
            CrngInit::Ready if now.saturating_sub(self.reseeded_at) >= reseed_interval(now) => {
                self.reseed(src, now);
            }
            CrngInit::Empty => self.key = self.extract(src),
            _ => {}
        }
        let mut stream = ChaCha20Rng::from_seed(self.key);
        stream.fill_bytes(&mut self.key);
        stream
    }
}

/// `entropy_timer_state`.
struct Jitter {
    samples_per_bit: u64,
    samples: u64,
    next_tick: u64,
}

impl Jitter {
    /// The trial run of `try_to_generate_entropy()`; `None` when the cycle
    /// counter changes too rarely to carry jitter.
    fn new(src: &impl Sources) -> Option<Self> {
        let mut last = src.cycles();
        let mut different = 0;
        for _ in 1..NUM_TRIAL_SAMPLES {
            let entropy = src.cycles();
            different += u64::from(entropy != last);
            last = entropy;
        }
        let samples_per_bit = NUM_TRIAL_SAMPLES.div_ceil(different + 1);
        (samples_per_bit <= MAX_SAMPLES_PER_BIT).then_some(Self {
            samples_per_bit,
            samples: 0,
            next_tick: last.saturating_add(TICK_NANOS),
        })
    }

    /// One pass of the collection loop: every sample is mixed, and the first
    /// one past each tick counts as an `entropy_timer()` sample. Returns
    /// whether its credit made the CRNG ready.
    fn sample(&mut self, crng: &mut Crng, src: &impl Sources) -> bool {
        let entropy = src.cycles();
        crng.mix(&entropy.to_ne_bytes());
        if entropy < self.next_tick {
            return false;
        }
        self.next_tick = entropy.saturating_add(TICK_NANOS);
        self.samples += 1;
        self.samples.is_multiple_of(self.samples_per_bit) && crng.credit_init_bits(1, src, entropy)
    }
}

/// Keyed hash standing in for Linux's keyed BLAKE2s.
fn prf(key: &[u8; 32], data: &[u8]) -> [u8; 32] {
    Sha256::new_with_prefix(key)
        .chain_update(data)
        .finalize()
        .into()
}

/// `crng_reseed_interval()`: shorter intervals during the first two minutes.
fn reseed_interval(uptime: u64) -> u64 {
    if uptime >= CRNG_RESEED_INTERVAL * 2 {
        CRNG_RESEED_INTERVAL
    } else {
        CRNG_RESEED_START_INTERVAL.max(uptime / NANOS_PER_SEC / 2 * NANOS_PER_SEC)
    }
}

fn uptime() -> u64 {
    ax_runtime::hal::time::monotonic_time_nanos()
}

struct Random {
    crng: Mutex<Crng>,
    /// `crng_is_ready`, published before `waiters` are woken.
    ready: AtomicBool,
    /// `crng_init_wait`.
    waiters: PollSet,
    urandom_warnings_left: AtomicU32,
    urandom_warnings_missed: AtomicU32,
}

static RANDOM: OnceLock<Random> = OnceLock::new();

fn random() -> &'static Random {
    RANDOM.call_once(Random::new)
}

impl Random {
    fn new() -> Self {
        Self {
            crng: Mutex::new(Crng::new()),
            ready: AtomicBool::new(false),
            waiters: PollSet::new(),
            urandom_warnings_left: AtomicU32::new(URANDOM_MAX_WARN),
            urandom_warnings_missed: AtomicU32::new(0),
        }
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// The notification half of `_credit_init_bits()`, run without the lock.
    fn publish_ready(&self) {
        self.ready.store(true, Ordering::Release);
        // SAFETY: readiness is permanent and published above; no lock is held.
        unsafe { self.waiters.wake_all(IoEvents::IN) };
        info!("random: crng init done");
        let missed = self.urandom_warnings_missed.load(Ordering::Relaxed);
        if missed != 0 {
            info!("random: {missed} urandom warning(s) missed due to ratelimiting");
        }
    }

    /// Returns whether an unseeded `/dev/urandom` read may still be reported.
    fn note_unseeded_urandom(&self) -> bool {
        let report = self
            .urandom_warnings_left
            .try_update(Ordering::Release, Ordering::Relaxed, |left| {
                left.checked_sub(1)
            })
            .is_ok();
        if !report {
            self.urandom_warnings_missed.fetch_add(1, Ordering::Relaxed);
        }
        report
    }
}

/// `try_to_generate_entropy()`: collects until the CRNG is ready or `task` has
/// a signal pending.
fn try_to_generate_entropy(task: &UserTaskRef) {
    let rnd = random();
    let Some(mut jitter) = Jitter::new(&Platform) else {
        return;
    };
    while !rnd.is_ready() && !task.interrupted() {
        if jitter.sample(&mut rnd.crng.lock(), &Platform) {
            rnd.publish_ready();
        }
        yield_now();
    }
    rnd.crng.lock().mix(&Platform.cycles().to_ne_bytes());
}

/// Seeds the CRNG at boot, in the order of Linux `add_bootloader_randomness()`,
/// `random_init_early()` and `random_init()`.
pub(crate) fn init() {
    let rnd = random();
    let now = uptime();
    let mut crng = rnd.crng.lock();
    let mut ready = false;
    if let Some(seed) = ax_runtime::hal::boot::boot_entropy() {
        ready |= crng.add_bootloader_randomness(&seed, &Platform, now);
    }
    ready |= crng.init_early(&Platform, now);
    let wall = ax_runtime::hal::time::wall_time().as_nanos() as u64;
    crng.init_late(&Platform, now, wall);
    drop(crng);
    if ready {
        rnd.publish_ready();
    }
}

/// `rng_is_initialized()`.
pub(crate) fn rng_is_initialized() -> bool {
    random().is_ready()
}

/// A ChaCha20 stream whose CRNG key has already been replaced.
pub(crate) struct RandomStream(ChaCha20Rng);

impl RandomStream {
    pub(crate) fn fill(&mut self, buf: &mut [u8]) {
        self.0.fill_bytes(buf);
    }
}

/// `crng_make_state()`.
pub(crate) fn random_stream() -> RandomStream {
    RandomStream(random().crng.lock().make_state(&Platform, uptime()))
}

/// `get_random_bytes()`: never waits, even before [`rng_is_initialized`].
pub(crate) fn get_random_bytes(buf: &mut [u8]) {
    if !buf.is_empty() {
        random_stream().fill(buf);
    }
}

/// `write_pool_user()`: written bytes are mixed in without credit.
pub(crate) fn write_pool(buf: &[u8]) {
    random().crng.lock().mix(buf);
}

/// `urandom_read_iter()`.
pub(crate) fn urandom_read(buf: &mut [u8]) {
    let rnd = random();
    if !rnd.is_ready()
        && let Ok(Some(task)) = try_current_user_task()
    {
        try_to_generate_entropy(&task);
    }
    if !rnd.is_ready() && rnd.note_unseeded_urandom() {
        info!(
            "random: uninitialized urandom read ({} bytes read)",
            buf.len()
        );
    }
    get_random_bytes(buf);
}

/// Validates `getrandom()` flags and returns whether the call must wait.
pub(crate) fn getrandom_must_wait(flags: u32, ready: bool) -> StarryResult<bool> {
    if flags & !(GRND_NONBLOCK | GRND_RANDOM | GRND_INSECURE) != 0 {
        return Err(StarryError::InvalidInput);
    }
    if flags & (GRND_INSECURE | GRND_RANDOM) == GRND_INSECURE | GRND_RANDOM {
        return Err(StarryError::InvalidInput);
    }
    if ready || flags & GRND_INSECURE != 0 {
        return Ok(false);
    }
    if flags & GRND_NONBLOCK != 0 {
        return Err(StarryError::WouldBlock);
    }
    Ok(true)
}

/// `wait_for_random_bytes()`: collects jitter, then waits up to a second for
/// readiness, until the CRNG is ready.
pub(crate) fn wait_for_random_bytes(task: &UserTaskRef) -> StarryResult {
    while !rng_is_initialized() {
        try_to_generate_entropy(task);
        let ready = poll_io(&RandomReady, IoEvents::IN, false, || {
            if rng_is_initialized() {
                Ok(())
            } else {
                Err(StarryError::WouldBlock)
            }
        });
        let outcome = block_on_user_timeout(task, Some(Duration::from_secs(1)), ready);
        if !matches!(outcome, UserWaitOutcome::TimedOut) {
            return outcome.into_result()?;
        }
    }
    Ok(())
}

/// `random_poll()` for `/dev/random` and blocked `getrandom()` callers:
/// readable once the CRNG is ready, writable before that.
pub(crate) struct RandomReady;

impl Pollable for RandomReady {
    fn poll(&self) -> IoEvents {
        if rng_is_initialized() {
            IoEvents::IN
        } else {
            IoEvents::OUT
        }
    }

    unsafe fn register_shared(&self, sink: &mut dyn SharedRegistrationSink, events: IoEvents) {
        if !events.contains(IoEvents::IN) {
            return;
        }
        let rnd = random();
        unsafe { sink.register_shared(&rnd.waiters, IoEvents::IN) };
        if rnd.is_ready() {
            // SAFETY: readiness is permanent and published; no lock is held.
            unsafe { rnd.waiters.wake_all(IoEvents::IN) };
        }
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use core::cell::Cell;

    use super::*;

    /// A platform with a fixed number of CPU words and a ticking clock.
    struct Script {
        cpu_words: Cell<usize>,
        clock: Cell<u64>,
    }

    impl Script {
        fn new(cpu_words: usize) -> Self {
            Self {
                cpu_words: Cell::new(cpu_words),
                clock: Cell::new(0),
            }
        }
    }

    impl Sources for Script {
        fn cpu_word(&self) -> Option<u64> {
            let left = self.cpu_words.get().checked_sub(1)?;
            self.cpu_words.set(left);
            Some(0x5eed_0000_0000_0000 | left as u64)
        }

        fn cycles(&self) -> u64 {
            self.clock.set(self.clock.get() + 1);
            self.clock.get()
        }
    }

    /// A cycle counter advancing a fixed step per read.
    struct Clock {
        now: Cell<u64>,
        step: u64,
    }

    impl Clock {
        fn new(step: u64) -> Self {
            Self {
                now: Cell::new(0),
                step,
            }
        }
    }

    impl Sources for Clock {
        fn cpu_word(&self) -> Option<u64> {
            None
        }

        fn cycles(&self) -> u64 {
            self.now.set(self.now.get() + self.step);
            self.now.get()
        }
    }

    fn output(crng: &mut Crng, src: &Script) -> [u8; 32] {
        let mut out = [0; 32];
        crng.make_state(src, 0).fill_bytes(&mut out);
        out
    }

    #[test]
    fn uncredited_input_leaves_the_crng_unready() {
        let src = Script::new(0);
        let mut crng = Crng::new();
        for tick in 0..4096u64 {
            crng.mix(&tick.to_ne_bytes());
        }
        assert!(!crng.init_early(&src, 0));
        crng.init_late(&src, 0, 0);
        let _ = output(&mut crng, &src);
        assert_eq!(crng.init_bits, 0);
        assert_eq!(crng.init, CrngInit::Empty);
    }

    #[test]
    fn a_full_bootloader_seed_makes_the_crng_ready() {
        let src = Script::new(0);
        let mut short = Crng::new();
        assert!(!short.add_bootloader_randomness(&[7; 31], &src, 0));
        assert_eq!(short.init, CrngInit::Early);

        let mut full = Crng::new();
        assert!(full.add_bootloader_randomness(&[7; 32], &src, 0));
        assert!(full.is_ready());
        assert!(!full.add_bootloader_randomness(&[7; 32], &src, 0));
    }

    #[test]
    fn cpu_words_are_credited_per_word() {
        let mut three = Crng::new();
        assert!(!three.init_early(&Script::new(3), 0));
        assert_eq!(three.init_bits, 192);

        let mut four = Crng::new();
        assert!(four.init_early(&Script::new(4), 0));
        assert!(four.is_ready());
    }

    #[test]
    fn getrandom_flags_follow_linux() {
        for ready in [false, true] {
            assert!(matches!(
                getrandom_must_wait(0x8, ready),
                Err(StarryError::InvalidInput)
            ));
            assert!(matches!(
                getrandom_must_wait(GRND_INSECURE | GRND_RANDOM, ready),
                Err(StarryError::InvalidInput)
            ));
            assert!(matches!(getrandom_must_wait(GRND_INSECURE, ready), Ok(false)));
        }
        assert!(matches!(getrandom_must_wait(0, false), Ok(true)));
        assert!(matches!(getrandom_must_wait(GRND_RANDOM, false), Ok(true)));
        assert!(matches!(
            getrandom_must_wait(GRND_NONBLOCK, false),
            Err(StarryError::WouldBlock)
        ));
        assert!(matches!(
            getrandom_must_wait(GRND_NONBLOCK | GRND_RANDOM, true),
            Ok(false)
        ));
    }

    #[test]
    fn output_is_keyed_by_the_credited_seed() {
        let seeded = |seed| {
            let src = Script::new(0);
            let mut crng = Crng::new();
            crng.add_bootloader_randomness(&[seed; 32], &src, 0);
            (crng, src)
        };
        let (mut a, a_src) = seeded(1);
        let (mut b, b_src) = seeded(1);
        let (mut c, c_src) = seeded(2);
        let first = output(&mut a, &a_src);
        assert_eq!(first, output(&mut b, &b_src));
        assert_ne!(first, output(&mut c, &c_src));
        assert_ne!(first, output(&mut a, &a_src));
    }

    #[test]
    fn ready_crng_reseeds_on_the_linux_interval() {
        assert_eq!(reseed_interval(0), NANOS_PER_SEC);
        assert_eq!(reseed_interval(10 * NANOS_PER_SEC), 5 * NANOS_PER_SEC);
        assert_eq!(reseed_interval(119 * NANOS_PER_SEC), 59 * NANOS_PER_SEC);
        assert_eq!(reseed_interval(120 * NANOS_PER_SEC), CRNG_RESEED_INTERVAL);

        let src = Script::new(0);
        let mut crng = Crng::new();
        crng.add_bootloader_randomness(&[3; 32], &src, 0);
        let _ = crng.make_state(&src, NANOS_PER_SEC / 2);
        assert_eq!(crng.reseeded_at, 0);
        let _ = crng.make_state(&src, 200 * NANOS_PER_SEC);
        assert_eq!(crng.reseeded_at, 200 * NANOS_PER_SEC);
    }

    #[test]
    fn jitter_needs_a_changing_cycle_counter() {
        assert!(Jitter::new(&Clock::new(0)).is_none());
        assert_eq!(
            Jitter::new(&Clock::new(1)).map(|jitter| jitter.samples_per_bit),
            Some(1)
        );
    }

    #[test]
    fn jitter_credits_one_bit_per_tick_sample() {
        let src = Clock::new(TICK_NANOS);
        let mut jitter = Jitter::new(&src).unwrap();
        let mut crng = Crng::new();
        let mut samples = 1;
        while !jitter.sample(&mut crng, &src) {
            samples += 1;
        }
        assert_eq!(samples, POOL_READY_BITS);
        assert!(crng.is_ready());

        let fast = Clock::new(1);
        let mut idle = Jitter::new(&fast).unwrap();
        let mut unready = Crng::new();
        for _ in 0..4096 {
            assert!(!idle.sample(&mut unready, &fast));
        }
        assert_eq!(unready.init_bits, 0);
    }

    #[test]
    fn unseeded_urandom_notices_are_rate_limited() {
        let rnd = Random::new();
        let reported = (0..URANDOM_MAX_WARN + 2)
            .filter(|_| rnd.note_unseeded_urandom())
            .count();
        assert_eq!(reported, URANDOM_MAX_WARN as usize);
        assert_eq!(rnd.urandom_warnings_missed.load(Ordering::Relaxed), 2);
    }
}
