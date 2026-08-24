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

//! `vmexit stat`: per-CPU VM-exit reason counters with rates.

use std::{
    println,
    string::{String, ToString},
    sync::Mutex,
    time::Instant,
};

use axvm::{CpuExitCounts, ExitReason, vmexit_stats_snapshot};

use crate::shell::command::{CommandNode, ParsedCommand};

static LAST_STAT: Mutex<Option<(Instant, Vec<CpuExitCounts>, Vec<u64>)>> = Mutex::new(None);

fn vmexit_stat(_cmd: &ParsedCommand) {
    let now = Instant::now();
    let snapshot = vmexit_stats_snapshot();
    let host_periodic_ticks = (0..ax_std::os::arceos::modules::ax_runtime::hal::cpu_num())
        .map(|cpu_id| {
            ax_std::os::arceos::modules::ax_runtime::periodic_scheduler_tick_count(cpu_id)
                .unwrap_or(0)
        })
        .collect::<Vec<_>>();

    let mut last = LAST_STAT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let previous = last.take();
    let elapsed_secs = previous
        .as_ref()
        .map(|(instant, _, _)| now.duration_since(*instant).as_secs_f64())
        .unwrap_or(0.0);

    println!(
        "VM-exit counters per physical CPU ({} reasons):",
        ExitReason::COUNT
    );
    print!("  cpu  ");
    for reason in ExitReason::ALL {
        print!("{:>10}", reason.name());
    }
    println!();
    for entry in &snapshot {
        let previous_counts = previous
            .as_ref()
            .and_then(|(_, entries, _)| entries.iter().find(|old| old.cpu_id == entry.cpu_id))
            .map(|old| &old.counts);
        let any_counted = entry.counts.iter().any(|count| *count != 0);
        if !any_counted {
            continue;
        }
        print!("  {:>3}  ", entry.cpu_id);
        for (index, count) in entry.counts.iter().enumerate() {
            if *count == 0 {
                print!("{:>10}", "-");
                continue;
            }
            let delta = count.saturating_sub(previous_counts.map_or(0, |old| old[index]));
            if elapsed_secs > 0.0 && delta > 0 {
                let rate = delta as f64 / elapsed_secs;
                print!("{:>6}@{:>3.1}/s", count, rate);
            } else {
                print!("{:>10}", count);
            }
        }
        println!();
    }

    println!("Host periodic scheduler ticks (event-driven timer IRQs excluded):");
    for (cpu_id, count) in host_periodic_ticks.iter().copied().enumerate() {
        let previous_count = previous
            .as_ref()
            .and_then(|(_, _, counts)| counts.get(cpu_id))
            .copied()
            .unwrap_or(0);
        let delta = count.saturating_sub(previous_count);
        if elapsed_secs > 0.0 && delta > 0 {
            println!(
                "  cpu {:>3}: {:>10} ({:.3}/s)",
                cpu_id,
                count,
                delta as f64 / elapsed_secs
            );
        } else {
            println!("  cpu {:>3}: {:>10}", cpu_id, count);
        }
    }

    *last = Some((now, snapshot, host_periodic_ticks));
}

pub fn build_vmexit_cmd(tree: &mut std::collections::BTreeMap<String, CommandNode>) {
    let stat_cmd = CommandNode::new("Show per-CPU VM-exit counters with rates since the last call")
        .with_handler(vmexit_stat)
        .with_usage("vmexit stat");

    let vmexit_cmd = CommandNode::new("Inspect VM-exit statistics")
        .with_usage("vmexit <stat> ...")
        .add_subcommand("stat", stat_cmd);

    tree.insert("vmexit".to_string(), vmexit_cmd);
}
