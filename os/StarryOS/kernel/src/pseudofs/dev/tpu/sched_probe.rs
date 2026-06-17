//! TPU 流水线可观测性：调度切换探针。
//!
//! 挂在 `ax_task` 的 `sched_switch` tracepoint 上（已有的
//! [`crate::tracepoint::sched`] implementor 会调用本模块），按任务 id 累计
//! 「占用 CPU 的纳秒数」。TPU worker 每跑完一帧从这里取快照差值，就能回答
//! 「这一帧 worker 睡眠让出期间，CPU 跑去干了什么」：
//!
//! - 落到 **idle** 任务的时间 = 纯空转（无活可干）；
//! - 落到 **其他任务** 的时间 = 真正被有效利用的重叠窗口。
//!
//! 设计为零依赖侵入：只在 `sched_switch` 触发点累加，不改调度器。所有计时用
//! `monotonic_time_nanos`（RISC-V `time` CSR，跨架构安全），与项目其余计时
//! 口径一致。

use alloc::{string::String, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use ax_kspin::SpinNoIrq;
use ax_runtime::hal::time::monotonic_time_nanos;

/// 单核假设下的全局调度账本。SG2002 `max-cpu-num = 1`，无需 per-CPU。
struct SchedLedger {
    /// 上一次切换发生的时刻（ns）。
    last_switch_ns: AtomicU64,
    /// 上一次切入的任务 id（即当前正在占用 CPU 的任务）。
    last_tid: AtomicU64,
    /// idle 任务累计占用 CPU 的纳秒数。
    idle_ns: AtomicU64,
    /// 非 idle 任务累计占用 CPU 的纳秒数。
    other_ns: AtomicU64,
    /// TPU worker 自身累计占用 CPU 的纳秒数（真正在跑 run_one 的时间）。
    worker_ns: AtomicU64,
    /// 上下文切换总次数（全系统，任意任务间）。
    switch_count: AtomicU64,
    /// 涉及 worker 的切换次数（worker 被切出或切入，即 worker 为等硬件被
    /// 换出/换回的次数）。
    worker_switch_count: AtomicU64,
    /// idle 任务 id（启动后注册；0 表示未知）。
    idle_tid: AtomicU64,
    /// TPU worker 任务 id（worker 启动时注册；0 表示未知）。
    worker_tid: AtomicU64,
}

static LEDGER: SchedLedger = SchedLedger {
    last_switch_ns: AtomicU64::new(0),
    last_tid: AtomicU64::new(0),
    idle_ns: AtomicU64::new(0),
    other_ns: AtomicU64::new(0),
    worker_ns: AtomicU64::new(0),
    switch_count: AtomicU64::new(0),
    worker_switch_count: AtomicU64::new(0),
    idle_tid: AtomicU64::new(0),
    worker_tid: AtomicU64::new(0),
};

/// worker 切出目标表槽位数。worker 让出 CPU 时切到的不同任务一般很少
/// （idle + 几个用户/内核线程），16 槽足够；满了归入溢出计数。
const TARGET_SLOTS: usize = 16;
/// 每个目标任务名缓存的最大字节数（截断保存，够区分即可）。
const NAME_CAP: usize = 24;

/// 单个切出目标槽。`tid == 0` 表示空槽。
#[derive(Clone, Copy)]
struct TargetSlot {
    tid: u64,
    count: u64,
    /// 任务名截断字节缓存 + 有效长度。
    name: [u8; NAME_CAP],
    name_len: u8,
}

impl TargetSlot {
    const EMPTY: Self = Self {
        tid: 0,
        count: 0,
        name: [0; NAME_CAP],
        name_len: 0,
    };
}

/// 「worker 切出去给了谁」目标表。`on_switch` 在 IRQ off 下触发，单核串行，
/// 用 `SpinNoIrq` 守护一张定长表即可——零分配（中断上下文不能 alloc），
/// 临界区只是几次定长写入。
struct TargetTable {
    slots: [TargetSlot; TARGET_SLOTS],
    /// 槽满后无法记录的切换次数。
    overflow: u64,
}

static TARGETS: SpinNoIrq<TargetTable> = SpinNoIrq::new(TargetTable {
    slots: [TargetSlot::EMPTY; TARGET_SLOTS],
    overflow: 0,
});

/// 记录一次「worker 切到 (tid, name)」。在 tid 对应槽计数 +1；无则占一个空槽
/// 并缓存名字；满则计入 overflow。
fn record_target(tid: u64, name: &str) {
    let mut table = TARGETS.lock();
    for slot in table.slots.iter_mut() {
        if slot.tid == tid {
            slot.count += 1;
            return;
        }
        if slot.tid == 0 {
            let bytes = name.as_bytes();
            let len = bytes.len().min(NAME_CAP);
            slot.tid = tid;
            slot.count = 1;
            slot.name[..len].copy_from_slice(&bytes[..len]);
            slot.name_len = len as u8;
            return;
        }
    }
    table.overflow += 1;
}

/// 一次性快照，供 worker 计算每帧差值。
#[derive(Clone, Copy)]
pub struct SchedSnapshot {
    pub idle_ns: u64,
    pub other_ns: u64,
    pub worker_ns: u64,
    pub switch_count: u64,
    pub worker_switch_count: u64,
}

/// 注册 idle 任务 id，用于把「让给 idle」单列为空转。
pub fn set_idle_tid(tid: u64) {
    LEDGER.idle_tid.store(tid, Ordering::Relaxed);
}

/// 注册 TPU worker 任务 id（worker 线程启动时调用）。
pub fn set_worker_tid(tid: u64) {
    LEDGER.worker_tid.store(tid, Ordering::Relaxed);
}

/// 由 `sched_switch` tracepoint 调用：把「上一段」时间记到切出任务名下，并在
/// worker 切出时按 `next_tid` 记录目标任务（含名字 `next_name`）。
///
/// 在 `axtask::switch_to` 内触发，IRQ 已关、抢占已禁，故只做原子加 / 定长字节
/// 拷贝，不分配、不加睡眠锁。
pub fn on_switch(prev_tid: u64, next_tid: u64, next_name: &str) {
    let now = monotonic_time_nanos();
    let last = LEDGER.last_switch_ns.swap(now, Ordering::Relaxed);
    let who = LEDGER.last_tid.swap(next_tid, Ordering::Relaxed);

    LEDGER.switch_count.fetch_add(1, Ordering::Relaxed);

    let idle_tid = LEDGER.idle_tid.load(Ordering::Relaxed);
    let worker_tid = LEDGER.worker_tid.load(Ordering::Relaxed);

    // worker 被切出或切入：即 worker 为等硬件被换出/换回的次数。
    if worker_tid != 0 && (prev_tid == worker_tid || next_tid == worker_tid) {
        LEDGER.worker_switch_count.fetch_add(1, Ordering::Relaxed);
    }
    // worker 切「出」去给了谁：按 next_tid 计数并缓存名字，回答「让出的 CPU
    // 跑了谁」。
    if worker_tid != 0 && prev_tid == worker_tid && next_tid != worker_tid {
        record_target(next_tid, next_name);
    }

    // 首次调用 last==0，跳过这段无意义的区间。
    if last == 0 || now <= last {
        return;
    }
    let delta = now - last;

    // 把刚结束的这段 CPU 时间记到「切出去的那个任务」(who，即上一次切入者)。
    // prev_tid 与 who 正常情况下一致；以 who 为准更稳健。
    let _ = prev_tid;

    if who == idle_tid && idle_tid != 0 {
        LEDGER.idle_ns.fetch_add(delta, Ordering::Relaxed);
    } else if who == worker_tid && worker_tid != 0 {
        LEDGER.worker_ns.fetch_add(delta, Ordering::Relaxed);
    } else {
        LEDGER.other_ns.fetch_add(delta, Ordering::Relaxed);
    }
}

/// 取当前累计快照。
pub fn snapshot() -> SchedSnapshot {
    SchedSnapshot {
        idle_ns: LEDGER.idle_ns.load(Ordering::Relaxed),
        other_ns: LEDGER.other_ns.load(Ordering::Relaxed),
        worker_ns: LEDGER.worker_ns.load(Ordering::Relaxed),
        switch_count: LEDGER.switch_count.load(Ordering::Relaxed),
        worker_switch_count: LEDGER.worker_switch_count.load(Ordering::Relaxed),
    }
}

impl SchedSnapshot {
    /// 与较早的快照求差，得到「这一帧期间」各项增量。
    pub fn delta_since(&self, base: &SchedSnapshot) -> SchedSnapshot {
        SchedSnapshot {
            idle_ns: self.idle_ns.saturating_sub(base.idle_ns),
            other_ns: self.other_ns.saturating_sub(base.other_ns),
            worker_ns: self.worker_ns.saturating_sub(base.worker_ns),
            switch_count: self.switch_count.saturating_sub(base.switch_count),
            worker_switch_count: self
                .worker_switch_count
                .saturating_sub(base.worker_switch_count),
        }
    }
}

/// 清空「worker 切出目标表」。在每帧开跑前由 worker 上下文调用，使表只统计
/// 当前这一帧的切出目标。
pub fn reset_targets() {
    let mut table = TARGETS.lock();
    table.slots = [TargetSlot::EMPTY; TARGET_SLOTS];
    table.overflow = 0;
}

/// 一个「worker 切出目标」条目。
pub struct YieldTarget {
    pub tid: u64,
    pub count: u64,
    /// 目标任务名；空表示未知（如溢出哨兵或名字未缓存）。
    pub name: String,
}

/// 读出当前「worker 切出目标」列表，按次数降序。末项若 `tid == 0` 表示槽溢出
/// 的次数（name 为空）。由 worker 上下文调用（可分配）。
pub fn targets_summary() -> Vec<YieldTarget> {
    let mut out: Vec<YieldTarget> = Vec::new();
    let table = TARGETS.lock();
    for slot in table.slots.iter() {
        if slot.tid != 0 && slot.count != 0 {
            let len = slot.name_len as usize;
            let name = core::str::from_utf8(&slot.name[..len]).unwrap_or("").into();
            out.push(YieldTarget {
                tid: slot.tid,
                count: slot.count,
                name,
            });
        }
    }
    let overflow = table.overflow;
    drop(table);

    out.sort_unstable_by_key(|entry| core::cmp::Reverse(entry.count));
    if overflow != 0 {
        // tid==0 作为「溢出」哨兵附在末尾。
        out.push(YieldTarget {
            tid: 0,
            count: overflow,
            name: String::new(),
        });
    }
    out
}
