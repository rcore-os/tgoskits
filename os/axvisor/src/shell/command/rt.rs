// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Real-time diagnostics and bounded software-timer stress measurements.

use std::{collections::BTreeMap, string::ToString, time::Duration};

use crate::shell::command::{CommandNode, OptionDef, ParsedCommand};

fn rt_stat(_cmd: &ParsedCommand) {
    let runtime = axvm::rt_runtime_stats_snapshot();
    if let Some(counts) = ax_std::os::arceos::modules::ax_task::priority_rr_scheduler_stats() {
        println!("FP-RR scheduler counters:");
        println!(
            "  quantum_ticks={} quantum_expiries={} same_priority_rotations={} \
             slice_preserving_preemptions={} voluntary_requeues={} idle_quantum_skips={} \
             lower_priority_services={}",
            ax_std::os::arceos::modules::ax_task::priority_rr_scheduler_quantum_ticks()
                .unwrap_or(0),
            counts.quantum_expiries,
            counts.same_priority_rotations,
            counts.slice_preserving_preemptions,
            counts.voluntary_requeues,
            counts.idle_quantum_skips,
            counts.lower_priority_services
        );
    }
    println!("RT vCPU wait counters:");
    for counts in runtime.vcpus {
        if counts.parks != 0
            || counts.wakes != 0
            || counts.notify_woke != 0
            || counts.vtimer_direct_acks != 0
        {
            println!(
                "  vcpu={} post_vmexit_yields={} parks={} wakes={} notify_woke={} vtimer_arms={} \
                 vtimer_immediate={} vtimer_no_deadline={} vtimer_registered={} \
                 vtimer_callbacks={} vtimer_stale_callbacks={} \
                 vtimer_notifications={} vtimer_invalidations={} \
                 callback_to_wake_samples={} callback_to_wake_overflow={} \
                 callback_to_wake_p50_ns={} callback_to_wake_p99_ns={} \
                 callback_to_wake_p99_9_ns={} callback_to_wake_max_ns={} \
                 callback_to_run_dispatch_samples={} callback_to_run_dispatch_overflow={} \
                 callback_to_run_dispatch_p50_ns={} callback_to_run_dispatch_p99_ns={} \
                 callback_to_run_dispatch_p99_9_ns={} callback_to_run_dispatch_max_ns={} \
                 callback_to_guest_entry_samples={} callback_to_guest_entry_overflow={} \
                 callback_to_guest_entry_p50_ns={} callback_to_guest_entry_p99_ns={} \
                 callback_to_guest_entry_p99_9_ns={} callback_to_guest_entry_max_ns={} \
                 direct_acks={} direct_overlaps={} \
                 direct_to_run_dispatch_samples={} direct_to_run_dispatch_overflow={} \
                 direct_to_run_dispatch_p50_ns={} direct_to_run_dispatch_p99_ns={} \
                 direct_to_run_dispatch_p99_9_ns={} direct_to_run_dispatch_max_ns={} \
                 direct_to_guest_entry_samples={} direct_to_guest_entry_overflow={} \
                 direct_to_guest_entry_p50_ns={} direct_to_guest_entry_p99_ns={} \
                 direct_to_guest_entry_p99_9_ns={} direct_to_guest_entry_max_ns={} \
                 activation_hold_samples={} activation_hold_overflow={} \
                 activation_hold_p50_ns={} activation_hold_p99_ns={} \
                 activation_hold_p99_9_ns={} activation_hold_max_ns={}",
                counts.vcpu_id,
                counts.post_vmexit_yields,
                counts.parks,
                counts.wakes,
                counts.notify_woke,
                counts.vtimer_arms,
                counts.vtimer_immediate,
                counts.vtimer_no_deadline,
                counts.vtimer_registered,
                counts.vtimer_callbacks,
                counts.vtimer_stale_callbacks,
                counts.vtimer_notifications,
                counts.vtimer_invalidations,
                counts.vtimer_callback_to_wake_samples,
                counts.vtimer_callback_to_wake_overflow,
                counts.vtimer_callback_to_wake_p50_ns,
                counts.vtimer_callback_to_wake_p99_ns,
                counts.vtimer_callback_to_wake_p99_9_ns,
                counts.vtimer_callback_to_wake_max_ns,
                counts.vtimer_callback_to_entry_samples,
                counts.vtimer_callback_to_entry_overflow,
                counts.vtimer_callback_to_entry_p50_ns,
                counts.vtimer_callback_to_entry_p99_ns,
                counts.vtimer_callback_to_entry_p99_9_ns,
                counts.vtimer_callback_to_entry_max_ns,
                counts.vtimer_callback_to_guest_entry_samples,
                counts.vtimer_callback_to_guest_entry_overflow,
                counts.vtimer_callback_to_guest_entry_p50_ns,
                counts.vtimer_callback_to_guest_entry_p99_ns,
                counts.vtimer_callback_to_guest_entry_p99_9_ns,
                counts.vtimer_callback_to_guest_entry_max_ns,
                counts.vtimer_direct_acks,
                counts.vtimer_direct_overlaps,
                counts.vtimer_direct_to_entry_samples,
                counts.vtimer_direct_to_entry_overflow,
                counts.vtimer_direct_to_entry_p50_ns,
                counts.vtimer_direct_to_entry_p99_ns,
                counts.vtimer_direct_to_entry_p99_9_ns,
                counts.vtimer_direct_to_entry_max_ns,
                counts.vtimer_direct_to_guest_entry_samples,
                counts.vtimer_direct_to_guest_entry_overflow,
                counts.vtimer_direct_to_guest_entry_p50_ns,
                counts.vtimer_direct_to_guest_entry_p99_ns,
                counts.vtimer_direct_to_guest_entry_p99_9_ns,
                counts.vtimer_direct_to_guest_entry_max_ns,
                counts.vtimer_activation_hold_samples,
                counts.vtimer_activation_hold_overflow,
                counts.vtimer_activation_hold_p50_ns,
                counts.vtimer_activation_hold_p99_ns,
                counts.vtimer_activation_hold_p99_9_ns,
                counts.vtimer_activation_hold_max_ns
            );
        }
    }
    println!("  lr_skips={}", runtime.lr_skips);

    println!("RT device-poll counters:");
    for vm in crate::manager::AxvmManager::vm_list() {
        if let Ok(counts) = vm.device_poll_runtime_counts() {
            println!(
                "  vm={} published={} kicked={} consumed={} pending={}",
                vm.id(),
                counts.published,
                counts.kicked,
                counts.consumed,
                counts.pending
            );
        }
    }

    println!("RT AxVM timer counters:");
    for counts in runtime.timers {
        if counts.registered != 0
            || counts.cancelled != 0
            || counts.expired != 0
            || counts.worker_wakes != 0
        {
            println!(
                "  cpu={} now_ns={} wheel_next_ns={} published_ns={} \
                 registered={} cancelled={} expired={} worker_wakes={} \
                 expiry_batches={} \
                 expiry_late_samples={} expiry_late_overflow={} \
                 expiry_late_p50_ns={} expiry_late_p99_ns={} \
                 expiry_late_p99_9_ns={} expiry_late_max_ns={} \
                 lock_acquisitions={} lock_wait_total_ns={} lock_wait_max_ns={} \
                 lock_hold_total_ns={} lock_hold_max_ns={}",
                counts.cpu_id,
                counts.snapshot_now_ns,
                counts.wheel_next_deadline_ns,
                counts.published_deadline_ns,
                counts.registered,
                counts.cancelled,
                counts.expired,
                counts.worker_wakes,
                counts.expiry_batches,
                counts.expiry_late_samples,
                counts.expiry_late_overflow,
                counts.expiry_late_p50_ns,
                counts.expiry_late_p99_ns,
                counts.expiry_late_p99_9_ns,
                counts.expiry_late_max_ns,
                counts.lock_acquisitions,
                counts.lock_wait_total_ns,
                counts.lock_wait_max_ns,
                counts.lock_hold_total_ns,
                counts.lock_hold_max_ns
            );
        }
    }

    println!("RT host hardware timer IRQ counters:");
    for cpu_id in 0..ax_std::os::arceos::modules::ax_runtime::hal::cpu_num() {
        let count = ax_std::os::arceos::modules::ax_task::hardware_timer_irq_count(cpu_id);
        println!("  cpu={} irqs={}", cpu_id, count);
    }

    let console = crate::guest_console::stats_snapshot();
    println!(
        "RT console counters: attached={:?} flush_calls={} host_write_bytes={}",
        console.attached, console.flush_calls, console.host_write_bytes
    );
    for counts in console.guests {
        println!(
            "  vm={} active={} input_enqueued={} input_drained={} input_dropped={} \
             input_pending={} output_enqueued={} output_drained={} output_dropped={} \
             output_pending={}",
            counts.vm_id,
            counts.active,
            counts.input_enqueued,
            counts.input_drained,
            counts.input_dropped,
            counts.input_pending,
            counts.output_enqueued,
            counts.output_drained,
            counts.output_dropped,
            counts.output_pending
        );
    }
}

fn parse_usize_option(cmd: &ParsedCommand, name: &str, default: usize) -> Result<usize, ()> {
    let Some(raw) = cmd.options.get(name) else {
        return Ok(default);
    };
    raw.parse().map_err(|_| {
        println!("RT_TIMER_STORM_ERROR invalid --{name} value: {raw}");
    })
}

fn parse_cpu_mask(cmd: &ParsedCommand) -> Result<usize, ()> {
    let Some(raw) = cmd.options.get("cpus") else {
        return Ok(0xf);
    };
    let parsed = raw
        .strip_prefix("0x")
        .or_else(|| raw.strip_prefix("0X"))
        .map_or_else(|| raw.parse(), |hex| usize::from_str_radix(hex, 16));
    parsed.map_err(|_| {
        println!("RT_TIMER_STORM_ERROR invalid --cpus mask: {raw}");
    })
}

fn rt_timer_storm(cmd: &ParsedCommand) {
    let Ok(cpu_mask) = parse_cpu_mask(cmd) else {
        return;
    };
    let Ok(iterations) = parse_usize_option(cmd, "iterations", 2_000) else {
        return;
    };
    let Ok(expiry_samples) = parse_usize_option(cmd, "expiry-samples", 64) else {
        return;
    };
    let Ok(expiry_delay_us) = parse_usize_option(cmd, "expiry-delay-us", 100_000) else {
        return;
    };
    println!(
        "RT_TIMER_STORM_START cpu_mask={cpu_mask:#x} iterations_per_worker={iterations} \
         expiry_samples_per_worker={expiry_samples} expiry_delay_us={expiry_delay_us}"
    );
    match axvm::run_timer_storm(
        cpu_mask,
        iterations,
        expiry_samples,
        Duration::from_micros(expiry_delay_us as u64),
    ) {
        Ok(result) => {
            println!(
                "RT_TIMER_STORM_RESULT implementation={} cpu_mask={:#x} workers={} \
                 iterations_per_worker={} register_cancel_pairs={} elapsed_ns={} \
                 pairs_per_second={}",
                result.implementation,
                result.cpu_mask,
                result.workers,
                result.iterations_per_worker,
                result.register_cancel_pairs,
                result.elapsed_ns,
                result.pairs_per_second
            );
            println!(
                "RT_TIMER_STORM_LOCK acquisitions={} wait_total_ns={} wait_max_ns={} \
                 hold_total_ns={} hold_max_ns={}",
                result.lock_acquisitions,
                result.lock_wait_total_ns,
                result.lock_wait_max_ns,
                result.lock_hold_total_ns,
                result.lock_hold_max_ns
            );
            println!(
                "RT_TIMER_STORM_EXPIRY samples={} completed={} p50_late_ns={} \
                 p99_late_ns={} max_late_ns={}",
                result.expiry_samples,
                result.expiry_completed,
                result.expiry_p50_late_ns,
                result.expiry_p99_late_ns,
                result.expiry_max_late_ns
            );
            println!(
                "RT_TIMER_STORM_COMPLETE implementation={}",
                result.implementation
            );
        }
        Err(error) => println!("RT_TIMER_STORM_ERROR {error}"),
    }
}

pub fn build_rt_cmd(tree: &mut BTreeMap<String, CommandNode>) {
    let stat_cmd = CommandNode::new("Show real-time runtime and console counters")
        .with_handler(rt_stat)
        .with_usage("rt stat");
    let timer_storm_cmd = CommandNode::new("Run a bounded multi-CPU AxVM software-timer storm")
        .with_handler(rt_timer_storm)
        .with_usage(
            "rt timer-storm [--cpus MASK] [--iterations N] [--expiry-samples N] \
             [--expiry-delay-us N]",
        )
        .with_option(OptionDef::new("cpus", "Physical CPU mask (default: 0xf)").with_long("cpus"))
        .with_option(
            OptionDef::new(
                "iterations",
                "Register/cancel pairs per worker (default: 2000)",
            )
            .with_long("iterations"),
        )
        .with_option(
            OptionDef::new("expiry-samples", "Expiry samples per worker (default: 64)")
                .with_long("expiry-samples"),
        )
        .with_option(
            OptionDef::new(
                "expiry-delay-us",
                "Timer expiry delay in us (default: 100000)",
            )
            .with_long("expiry-delay-us"),
        );
    let rt_cmd = CommandNode::new("Inspect real-time runtime diagnostics")
        .with_usage("rt <stat|timer-storm> ...")
        .add_subcommand("stat", stat_cmd)
        .add_subcommand("timer-storm", timer_storm_cmd);
    tree.insert("rt".to_string(), rt_cmd);
}
