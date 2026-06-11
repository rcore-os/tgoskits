//! cgroup v2 cpu controller — kernel-side bandwidth tick.
//!
//! The core CpuState / BandwidthState live in `ax-cgroup`.
//! This module provides the tick hook that accesses current task / cgroup / time.
//!
//! NOTE: The actual bandwidth_tick function is deferred because
//! `ax_task::set_tick_hook` and `set_throttled` APIs are not yet
//! available on the dev branch. When these APIs are added, uncomment
//! the tick hook registration in `mod.rs::init()`.

// use crate::task::AsThread;
//
// pub fn bandwidth_tick() {
//     let curr = match ax_task::current_may_uninit() {
//         Some(task) => task,
//         None => return,
//     };
//
//     if curr.name() == "idle" {
//         return;
//     }
//
//     let proc_data = match curr.try_as_thread() {
//         Some(thr) => thr.proc_data.clone(),
//         None => return,
//     };
//
//     let cgroup = proc_data.cgroup.read().clone();
//
//     if !cgroup.cpu.bandwidth.has_quota() {
//         return;
//     }
//
//     let now = ax_hal::time::monotonic_time_nanos() / 1000;
//     let period_start = cgroup.cpu.bandwidth.period_start.load(core::sync::atomic::Ordering::Acquire);
//     let period = cgroup.cpu.bandwidth.period.load(core::sync::atomic::Ordering::Acquire);
//
//     if now - period_start >= period as u64 {
//         cgroup.cpu.bandwidth.reset_period();
//         cgroup.cpu.bandwidth.period_start.store(now, core::sync::atomic::Ordering::Release);
//         curr.set_throttled(false);
//     }
//
//     let tick_usec = 1000;
//     let throttled = cgroup.cpu.bandwidth.consume(tick_usec);
//
//     if throttled {
//         curr.set_throttled(true);
//     }
// }
