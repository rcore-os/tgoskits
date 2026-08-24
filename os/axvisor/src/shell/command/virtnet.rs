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

//! Control commands for the in-hypervisor virtio-net switch: blackout fault
//! injection, per-frame capture, and port introspection.

use std::println;
use std::string::{String, ToString};

use crate::shell::command::{CommandNode, ParsedCommand};

fn virtnet_drop(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let Some(state) = args.first() else {
        println!("virtnet drop: usage: virtnet drop <on|off>");
        return;
    };
    match state.as_str() {
        "on" => {
            crate::virtio_net::set_blackout(true);
            println!("virtnet: blackout ON (all switch traffic dropped)");
        }
        "off" => {
            crate::virtio_net::set_blackout(false);
            println!("virtnet: blackout OFF");
        }
        other => println!("virtnet drop: invalid state `{other}`; expected on or off"),
    }
}

fn virtnet_capture_on(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let Some(state) = args.first() else {
        println!("virtnet capture: usage: virtnet capture <on|off|dump PATH>");
        return;
    };
    match state.as_str() {
        "on" => {
            crate::virtio_net::capture_set_enabled(true);
            println!("virtnet: capture ON");
        }
        "off" => {
            crate::virtio_net::capture_set_enabled(false);
            println!(
                "virtnet: capture OFF ({} frames buffered)",
                crate::virtio_net::capture_frame_count()
            );
        }
        #[cfg(feature = "fs")]
        "dump" => {
            if let Some(path) = args.get(1) {
                match crate::virtio_net::dump_capture(path) {
                    Ok((vm1, vm2)) => {
                        println!(
                            "virtnet: dumped {vm1} frames to {path}.vm1.pcap and {vm2} frames to {path}.vm2.pcap"
                        );
                    }
                    Err(error) => println!("virtnet capture dump: {error}"),
                }
            } else {
                let (vm1, vm2) = crate::virtio_net::dump_capture_to_console();
                println!("virtnet: streamed {vm1} vm1 and {vm2} vm2 frames to the console");
            }
        }
        #[cfg(not(feature = "fs"))]
        "dump" => {
            let (vm1, vm2) = crate::virtio_net::dump_capture_to_console();
            println!("virtnet: streamed {vm1} vm1 and {vm2} vm2 frames to the console");
        }
        other => println!("virtnet capture: invalid subcommand `{other}`"),
    }
}

fn virtnet_show(_cmd: &ParsedCommand) {
    println!(
        "virtnet switch: blackout={} capture={} frames={}",
        if crate::virtio_net::blackout_is_active() {
            "ON"
        } else {
            "off"
        },
        if crate::virtio_net::capture_is_enabled() {
            "ON"
        } else {
            "off"
        },
        crate::virtio_net::capture_frame_count(),
    );
    for (vm_id, mac, active) in crate::virtio_net::switch_ports() {
        println!(
            "  port vm{vm_id} mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} {}",
            mac[0],
            mac[1],
            mac[2],
            mac[3],
            mac[4],
            mac[5],
            if active { "active" } else { "inactive" },
        );
    }
}

pub fn build_virtnet_cmd(tree: &mut std::collections::BTreeMap<String, CommandNode>) {
    let drop_cmd = CommandNode::new("Drop all virtio-net switch traffic (blackout)")
        .with_handler(virtnet_drop)
        .with_usage("virtnet drop <on|off>");

    let capture_cmd = CommandNode::new("Capture or dump virtio-net switch frames")
        .with_handler(virtnet_capture_on)
        .with_usage("virtnet capture <on|off|dump [PATH]>");

    let show_cmd = CommandNode::new("Show virtio-net switch state and ports")
        .with_handler(virtnet_show)
        .with_usage("virtnet show");

    let virtnet_cmd = CommandNode::new("Control the in-hypervisor virtio-net switch")
        .with_usage("virtnet <drop|capture|show> ...")
        .add_subcommand("drop", drop_cmd)
        .add_subcommand("capture", capture_cmd)
        .add_subcommand("show", show_cmd);

    tree.insert("virtnet".to_string(), virtnet_cmd);
}
