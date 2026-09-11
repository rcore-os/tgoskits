use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
};

#[cfg(feature = "arceos")]
use ax_std as _;

const DEFAULT_GROUP_SWITCHES: usize = 1_000_000;
const DEFAULT_GROUPS: usize = 10;
const WARMUP_SWITCHES: usize = 1_000;
const REALTIME_PRIORITY: u8 = 80;
const DEFAULT_SCHED_POLICY: &str = "fifo";

const MAIN_TURN: usize = 0;
const CHILD_TURN: usize = 1;

fn main() {
    let group_switches = option_usize("AXVISOR_TASK_SWITCH_GROUP_SWITCHES", DEFAULT_GROUP_SWITCHES);
    let groups = option_usize("AXVISOR_TASK_SWITCH_GROUPS", DEFAULT_GROUPS);

    println!(
        "AXVISOR_TASK_SWITCH_BENCH_BEGIN group_switches={} groups={} warmup_switches={} \
         scheduler_policy={}",
        group_switches,
        groups,
        WARMUP_SWITCHES,
        selected_policy_name(),
    );

    if let Err(error) = init_cycle_counter() {
        println!("AXVISOR_TASK_SWITCH_BENCH_FAILED reason={error}");
        return;
    }
    if let Err(error) = configure_current_thread_for_bench() {
        println!("AXVISOR_TASK_SWITCH_BENCH_FAILED reason={error}");
        return;
    }

    let gpio = if gpio_enabled() {
        match Gpio3C6::init() {
            Ok(gpio) => {
                println!("AXVISOR_TASK_SWITCH_GPIO enabled=1 pin=GPIO3_C6");
                Some(Arc::new(gpio))
            }
            Err(error) => {
                println!("AXVISOR_TASK_SWITCH_GPIO enabled=0 reason={error}");
                None
            }
        }
    } else {
        println!("AXVISOR_TASK_SWITCH_GPIO enabled=0 reason=disabled-by-config");
        None
    };

    for group in 0..groups {
        if let Err(error) = run_ping_pong(WARMUP_SWITCHES, gpio.clone()) {
            println!("AXVISOR_TASK_SWITCH_BENCH_FAILED reason={error}");
            return;
        }

        match run_ping_pong(group_switches, gpio.clone()) {
            Ok(result) => result.print_avg(group),
            Err(error) => {
                println!("AXVISOR_TASK_SWITCH_BENCH_FAILED reason={error}");
                return;
            }
        }
    }

    if let Some(gpio) = gpio {
        gpio.write(false);
    }

    println!("AXVISOR_TASK_SWITCH_BENCH_DONE");
}

fn option_usize(name: &str, default: usize) -> usize {
    match name {
        "AXVISOR_TASK_SWITCH_GROUP_SWITCHES" => option_env!("AXVISOR_TASK_SWITCH_GROUP_SWITCHES"),
        "AXVISOR_TASK_SWITCH_GROUPS" => option_env!("AXVISOR_TASK_SWITCH_GROUPS"),
        _ => None,
    }
    .and_then(|value| value.parse().ok())
    .filter(|value| *value > 0)
    .unwrap_or(default)
}

fn gpio_enabled() -> bool {
    option_env!("AXVISOR_TASK_SWITCH_GPIO")
        .map(|value| !matches!(value, "0" | "false" | "False" | "FALSE" | "off" | "OFF"))
        .unwrap_or(false)
}

fn selected_policy_name() -> &'static str {
    option_env!("AXVISOR_TASK_SWITCH_SCHED_POLICY").unwrap_or(DEFAULT_SCHED_POLICY)
}

#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
fn init_cycle_counter() -> Result<(), &'static str> {
    if ax_cpu::pmu::probe().is_none() {
        return Err("aarch64-pmuv3-unavailable");
    }
    ax_cpu::pmu::init_cpu();
    if !ax_cpu::pmu::self_check() {
        return Err("pmccntr-not-counting");
    }
    ax_cpu::pmu::cycles::configure(false, false);
    ax_cpu::pmu::cycles::enable();
    Ok(())
}

#[cfg(not(all(feature = "arceos", target_arch = "aarch64")))]
fn init_cycle_counter() -> Result<(), &'static str> {
    Err("pmccntr-requires-arceos-aarch64")
}

#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
#[inline(always)]
fn read_cycles() -> u64 {
    ax_cpu::pmu::cycles::read()
}

#[cfg(not(all(feature = "arceos", target_arch = "aarch64")))]
#[inline(always)]
fn read_cycles() -> u64 {
    0
}

struct PingPong {
    ready: AtomicBool,
    turn: AtomicUsize,
    start: AtomicU64,
}

impl PingPong {
    fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            turn: AtomicUsize::new(MAIN_TURN),
            start: AtomicU64::new(0),
        }
    }
}

struct SwitchResult {
    main_to_child: SwitchStats,
    child_to_main: SwitchStats,
}

impl SwitchResult {
    fn print_avg(&self, group: usize) {
        println!(
            "AXVISOR_TASK_SWITCH_GROUP_SUMMARY index={} samples_per_direction={} avg_cycles={} \
             min_cycles={} max_cycles={}",
            group,
            self.main_to_child.samples.min(self.child_to_main.samples),
            self.avg_cycles(),
            self.min_cycles(),
            self.max_cycles(),
        );
    }

    fn avg_cycles(&self) -> u64 {
        let samples = self.main_to_child.samples + self.child_to_main.samples;
        if samples == 0 {
            return 0;
        }
        ((self.main_to_child.total_cycles + self.child_to_main.total_cycles) / u128::from(samples))
            as u64
    }

    fn min_cycles(&self) -> u64 {
        if self.main_to_child.samples + self.child_to_main.samples == 0 {
            return 0;
        }
        self.main_to_child
            .min_cycles
            .min(self.child_to_main.min_cycles)
    }

    fn max_cycles(&self) -> u64 {
        self.main_to_child
            .max_cycles
            .max(self.child_to_main.max_cycles)
    }
}

#[derive(Clone)]
struct SwitchStats {
    samples: u64,
    total_cycles: u128,
    min_cycles: u64,
    max_cycles: u64,
}

impl SwitchStats {
    fn new() -> Self {
        Self {
            samples: 0,
            total_cycles: 0,
            min_cycles: u64::MAX,
            max_cycles: 0,
        }
    }

    fn record(&mut self, cycles: u64) {
        self.samples += 1;
        self.total_cycles += u128::from(cycles);
        self.min_cycles = self.min_cycles.min(cycles);
        self.max_cycles = self.max_cycles.max(cycles);
    }
}

struct ChildResult {
    done: AtomicBool,
    samples: AtomicU64,
    total_low: AtomicU64,
    total_high: AtomicU64,
    min_cycles: AtomicU64,
    max_cycles: AtomicU64,
}

impl ChildResult {
    fn new() -> Self {
        Self {
            done: AtomicBool::new(false),
            samples: AtomicU64::new(0),
            total_low: AtomicU64::new(0),
            total_high: AtomicU64::new(0),
            min_cycles: AtomicU64::new(u64::MAX),
            max_cycles: AtomicU64::new(0),
        }
    }

    fn publish(&self, stats: SwitchStats) {
        let total = stats.total_cycles;
        self.samples.store(stats.samples, Ordering::Relaxed);
        self.total_low.store(total as u64, Ordering::Relaxed);
        self.total_high
            .store((total >> 64) as u64, Ordering::Relaxed);
        self.min_cycles.store(stats.min_cycles, Ordering::Relaxed);
        self.max_cycles.store(stats.max_cycles, Ordering::Relaxed);
        self.done.store(true, Ordering::Release);
    }

    fn take(&self) -> Option<SwitchStats> {
        if !self.done.load(Ordering::Acquire) {
            return None;
        }
        let low = self.total_low.load(Ordering::Relaxed) as u128;
        let high = self.total_high.load(Ordering::Relaxed) as u128;
        Some(SwitchStats {
            samples: self.samples.load(Ordering::Relaxed),
            total_cycles: (high << 64) | low,
            min_cycles: self.min_cycles.load(Ordering::Relaxed),
            max_cycles: self.max_cycles.load(Ordering::Relaxed),
        })
    }
}

fn run_ping_pong(
    switches: usize,
    gpio: Option<Arc<Gpio3C6>>,
) -> Result<SwitchResult, &'static str> {
    let shared = Arc::new(PingPong::new());
    let child_result = Arc::new(ChildResult::new());
    let child_shared = Arc::clone(&shared);
    let child_gpio = gpio.clone();
    let child_result_writer = Arc::clone(&child_result);
    let child = spawn_bench_thread("task-switch-peer", move || {
        let mut stats = SwitchStats::new();
        child_shared.ready.store(true, Ordering::Release);

        for _ in 0..switches {
            wait_for_turn(&child_shared, CHILD_TURN);
            let finished = read_cycles();
            stats.record(elapsed_since_start(&child_shared, finished));

            if let Some(gpio) = &child_gpio {
                gpio.write(false);
            }

            publish_switch_start(&child_shared, MAIN_TURN);
            thread::yield_now();
        }

        child_result_writer.publish(stats);
    })?;

    while !shared.ready.load(Ordering::Acquire) {
        thread::yield_now();
    }

    let mut child_to_main = SwitchStats::new();
    let mut child_started = false;
    for _ in 0..switches {
        wait_for_turn(&shared, MAIN_TURN);
        if child_started {
            child_to_main.record(elapsed_since_start(&shared, read_cycles()));
        }

        if let Some(gpio) = &gpio {
            gpio.write(true);
        }

        publish_switch_start(&shared, CHILD_TURN);
        child_started = true;
        thread::yield_now();
    }

    wait_for_turn(&shared, MAIN_TURN);
    child_to_main.record(elapsed_since_start(&shared, read_cycles()));

    join_bench_thread(child)?;
    let main_to_child = child_result
        .take()
        .ok_or("child-thread-did-not-publish-result")?;
    Ok(SwitchResult {
        main_to_child,
        child_to_main,
    })
}

fn publish_switch_start(shared: &PingPong, next_turn: usize) {
    let started = read_cycles();
    // `turn` carries the release/acquire synchronization; `start` only carries
    // the timestamp published before that release edge.
    shared.start.store(started, Ordering::Relaxed);
    shared.turn.store(next_turn, Ordering::Release);
}

fn elapsed_since_start(shared: &PingPong, finished: u64) -> u64 {
    let started = shared.start.load(Ordering::Relaxed);
    finished.wrapping_sub(started)
}

fn wait_for_turn(shared: &PingPong, turn: usize) {
    while shared.turn.load(Ordering::Acquire) != turn {
        thread::yield_now();
    }
}

#[cfg(feature = "arceos")]
type BenchThreadHandle = ax_runtime::task::thread::ThreadHandle;

#[cfg(not(feature = "arceos"))]
type BenchThreadHandle = thread::JoinHandle<()>;

#[cfg(feature = "arceos")]
fn configure_current_thread_for_bench() -> Result<(), &'static str> {
    let policy = bench_policy()?;
    ax_runtime::task::thread::current::current_thread_handle()
        .map_err(|_| "read-current-thread-failed")?
        .set_policy(policy)
        .map_err(|_| "set-current-thread-policy-failed")
}

#[cfg(not(feature = "arceos"))]
fn configure_current_thread_for_bench() -> Result<(), &'static str> {
    Ok(())
}

#[cfg(feature = "arceos")]
fn spawn_bench_thread(
    name: &str,
    entry: impl FnOnce() + Send + 'static,
) -> Result<BenchThreadHandle, &'static str> {
    let policy = bench_policy()?;
    ax_runtime::thread::spawn_raw_with_policy_and_affinity(
        entry,
        name.into(),
        ax_runtime::thread::default_task_stack_size(),
        policy,
        single_cpu_affinity()?,
    )
    .map_err(|_| "spawn-policy-thread-failed")
}

#[cfg(not(feature = "arceos"))]
fn spawn_bench_thread(
    _name: &str,
    entry: impl FnOnce() + Send + 'static,
) -> Result<BenchThreadHandle, &'static str> {
    thread::Builder::new()
        .spawn(entry)
        .map_err(|_| "spawn-thread-failed")
}

#[cfg(feature = "arceos")]
fn join_bench_thread(thread: BenchThreadHandle) -> Result<(), &'static str> {
    ax_runtime::thread::join_thread(thread)
        .map(|_| ())
        .map_err(|_| "child-thread-join-failed")
}

#[cfg(not(feature = "arceos"))]
fn join_bench_thread(thread: BenchThreadHandle) -> Result<(), &'static str> {
    thread.join().map_err(|_| "child-thread-panicked")
}

#[cfg(feature = "arceos")]
fn bench_policy() -> Result<ax_runtime::task::sched::SchedulePolicy, &'static str> {
    use ax_runtime::task::sched::{FairMode, Nice, RtPriority, SchedulePolicy};

    let rt_priority =
        RtPriority::new(REALTIME_PRIORITY).map_err(|_| "invalid-realtime-priority")?;
    match selected_policy_name() {
        "fair_normal" => Ok(SchedulePolicy::fair(Nice::ZERO, FairMode::Normal)),
        "fair_batch" => Ok(SchedulePolicy::fair(Nice::ZERO, FairMode::Batch)),
        "fair_idle" => Ok(SchedulePolicy::fair(Nice::ZERO, FairMode::Idle)),
        "fifo" => Ok(SchedulePolicy::fifo(rt_priority)),
        "round_robin" => Ok(SchedulePolicy::round_robin(rt_priority)),
        "deadline" => Err("deadline-policy-not-supported-for-this-benchmark"),
        _ => Err("unsupported-scheduler-policy"),
    }
}

#[cfg(feature = "arceos")]
fn single_cpu_affinity() -> Result<ax_runtime::task::sched::CpuSet, &'static str> {
    let topology_len =
        ax_runtime::task::sched::cpu_topology_len().map_err(|_| "read-cpu-topology-failed")?;
    let mut affinity = ax_runtime::task::sched::CpuSet::empty(topology_len);
    if affinity.insert(ax_runtime::task::sched::CpuId::new(0)) {
        Ok(affinity)
    } else {
        Err("insert-cpu0-affinity-failed")
    }
}

#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
struct Gpio3C6 {
    base: usize,
}

#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
impl Gpio3C6 {
    const IOC_BASE: usize = 0xfd5f_0000;
    const GPIO_BASES: [usize; 5] = [
        0xfd8a_0000,
        0xfec2_0000,
        0xfec3_0000,
        0xfec4_0000,
        0xfec5_0000,
    ];
    const GPIO_BANK_SIZE: usize = 0x100;
    const IOC_SIZE: usize = 0x1_0000;
    const GPIO3_C6_IN_BANK: u32 = 22;
    const SWPORT_DR_L: usize = 0x00;
    const SWPORT_DR_H: usize = 0x04;

    fn init() -> Result<Self, &'static str> {
        use core::ptr::NonNull;

        use rockchip_soc::{
            GPIO3_C6, GpioDirection, Iomux, PinConfig, PinCtrl, PinCtrlOp, Pull, SocType,
        };

        let ioc = map_mmio(Self::IOC_BASE, Self::IOC_SIZE)?;
        let mut gpio = [NonNull::dangling(); 5];
        for (mapped, base) in gpio.iter_mut().zip(Self::GPIO_BASES) {
            *mapped = map_mmio(base, Self::GPIO_BANK_SIZE)?;
        }

        let mut pinctrl = PinCtrl::new(SocType::Rk3588, ioc, &gpio);
        pinctrl
            .set_config(PinConfig {
                id: GPIO3_C6,
                mux: Iomux::empty(),
                pull: Pull::Disabled,
                drive: None,
            })
            .map_err(|_| "gpio3-c6-pinmux-config-failed")?;
        pinctrl
            .set_gpio_direction(GPIO3_C6, GpioDirection::Output(false))
            .map_err(|_| "gpio3-c6-output-config-failed")?;

        let base = gpio[3].as_ptr() as usize;
        Ok(Self { base })
    }

    #[inline(always)]
    fn write(&self, value: bool) {
        set_gpio_bit(
            self.base,
            Self::SWPORT_DR_L,
            Self::SWPORT_DR_H,
            Self::GPIO3_C6_IN_BANK,
            value,
        );
    }
}

#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
fn map_mmio(base: usize, size: usize) -> Result<core::ptr::NonNull<u8>, &'static str> {
    use ax_memory_addr::PhysAddr;

    let virt = ax_mm::iomap(PhysAddr::from_usize(base), size).map_err(|_| "iomap-failed")?;
    core::ptr::NonNull::new(virt.as_mut_ptr()).ok_or("iomap-returned-null")
}

#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
#[inline(always)]
fn set_gpio_bit(base: usize, low: usize, high: usize, pin: u32, value: bool) {
    let mut current = read_gpio_pair(base, low, high);
    if value {
        current |= 1 << pin;
    } else {
        current &= !(1 << pin);
    }
    write_gpio_pair(base, low, high, current);
}

#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
#[inline(always)]
fn read_gpio_pair(base: usize, low: usize, high: usize) -> u32 {
    // SAFETY: `base` comes from `ax_mm::iomap` for the live GPIO3 register
    // window. The offsets are 32-bit RK3588 GPIO data/direction registers.
    unsafe {
        let low = ((base + low) as *const u32).read_volatile() & 0xffff;
        let high = ((base + high) as *const u32).read_volatile() & 0xffff;
        low | (high << 16)
    }
}

#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
#[inline(always)]
fn write_gpio_pair(base: usize, low: usize, high: usize, value: u32) {
    // SAFETY: `base` comes from `ax_mm::iomap` for the live GPIO3 register
    // window. RK3588 GPIO split registers use the upper 16 bits as write mask.
    unsafe {
        ((base + low) as *mut u32).write_volatile((value & 0xffff) | 0xffff_0000);
        ((base + high) as *mut u32).write_volatile((value >> 16) | 0xffff_0000);
    }
}

#[cfg(not(all(feature = "arceos", target_arch = "aarch64")))]
struct Gpio3C6;

#[cfg(not(all(feature = "arceos", target_arch = "aarch64")))]
impl Gpio3C6 {
    fn init() -> Result<Self, &'static str> {
        Err("gpio3-c6-requires-arceos-aarch64")
    }

    fn write(&self, _value: bool) {}
}
