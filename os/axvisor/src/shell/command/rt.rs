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
use std::println;

use crate::realtime::{RtState, status};
use crate::shell::command::{CommandNode, ParsedCommand};

fn do_rt_status(_cmd: &ParsedCommand) {
    let status = status();
    let cpu = status
        .cpu_id
        .map(|cpu_id| cpu_id.to_string())
        .unwrap_or_else(|| "none".to_string());
    let state = match status.state {
        RtState::Offline => "offline",
        RtState::Heartbeat => "heartbeat",
    };

    println!("RT CPU: {cpu}");
    println!("State: {state}");
    println!("Heartbeats: {}", status.heartbeats);
    println!("Entry ns: {}", status.entry_nanos);
    println!("Last heartbeat ns: {}", status.last_heartbeat_nanos);
}

fn do_rt_help(_cmd: &ParsedCommand) {
    println!("RT commands:");
    println!("  rt status    Show realtime CPU status");
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
        );

    tree.insert("rt".to_string(), rt_node);
}
