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

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::{print, println};

use crate::realtime::{
    RtState, RtTaskState, heartbeats, last_heartbeat_nanos, last_watchdog_nanos, rt_read_output,
    status,
};
use crate::shell::command::{CommandNode, ParsedCommand};

fn do_rt_status(_cmd: &ParsedCommand) {
    let status = status();
    let cpu = status
        .cpu_id
        .map(|cpu_id| cpu_id.to_string())
        .unwrap_or_else(|| "none".to_string());
    let state = match status.state {
        RtState::Offline => "offline",
        RtState::Running => "running",
    };

    println!("RT CPU: {cpu}");
    println!("State: {state}");
    println!("Scheduler: cooperative-context-switch");
    println!("Task contexts: {}", status.task_count);
    println!("Heartbeats: {}", heartbeats());
    println!("Executor iterations: {}", status.executor_iterations);
    println!("Entry ns: {}", status.entry_nanos);
    println!("Last heartbeat ns: {}", last_heartbeat_nanos());
    println!("Last watchdog ns: {}", last_watchdog_nanos());
    println!("Tasks:");
    println!(
        "  {name:<10} {state:<8} {period:>14} {deadline:>14} {runs:>8} {start:>14} {finish:>14}",
        name = "name",
        state = "state",
        period = "period(ns)",
        deadline = "deadline(ns)",
        runs = "runs",
        start = "last_start(ns)",
        finish = "last_finish(ns)",
    );
    for task in status.tasks.into_iter().take(status.task_count) {
        let task_state = match task.state {
            RtTaskState::Ready => "ready",
            RtTaskState::Running => "running",
            RtTaskState::Delayed => "delayed",
            RtTaskState::Blocked => "blocked",
            RtTaskState::Exited => "exited",
        };
        println!(
            "  {name:<10} {state:<8} {period:>14} {deadline:>14} {runs:>8} {start:>14} {finish:>14}",
            name = task.name,
            state = task_state,
            period = task.period_nanos,
            deadline = task.deadline_nanos,
            runs = task.runs,
            start = task.last_start_nanos,
            finish = task.last_finish_nanos,
        );
    }
}

fn do_rt_help(_cmd: &ParsedCommand) {
    println!("RT commands:");
    println!("  rt status     Show realtime CPU status");
    println!("  rt console    Drain realtime console output");
    println!("  rt shell      Alias of rt console");
}

fn do_rt_console(_cmd: &ParsedCommand) {
    println!("[RT] console output:");
    let mut idle_rounds = 0;
    let mut output = [0u8; 128];
    loop {
        let copied = rt_read_output(&mut output);
        if copied == 0 {
            idle_rounds += 1;
            if idle_rounds >= 200 {
                break;
            }
            ax_std::thread::sleep(core::time::Duration::from_millis(25));
            continue;
        }

        idle_rounds = 0;
        let text = core::str::from_utf8(&output[..copied]).unwrap_or("<non-utf8 RT output>");
        print!("{text}");
        io::stdout().flush().ok();
    }
    println!("[RT] console detached");
}

pub fn build_rt_cmd(tree: &mut BTreeMap<String, CommandNode>) {
    let rt_node = CommandNode::new("Realtime CPU management")
        .add_subcommand(
            "status",
            CommandNode::new("Show realtime CPU status")
                .with_handler(do_rt_status)
                .with_usage("rt status"),
        )
        .add_subcommand(
            "help",
            CommandNode::new("Show RT help")
                .with_handler(do_rt_help)
                .with_usage("rt help"),
        )
        .add_subcommand(
            "console",
            CommandNode::new("Drain realtime console output")
                .with_handler(do_rt_console)
                .with_usage("rt console"),
        )
        .add_subcommand(
            "shell",
            CommandNode::new("Drain realtime console output")
                .with_handler(do_rt_console)
                .with_usage("rt shell"),
        );

    tree.insert("rt".to_string(), rt_node);
}
