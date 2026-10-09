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

//! Interactive `vm` shell command.
//!
//! Every handler reads immutable [`axvm::VmSnapshot`] observations and drives
//! lifecycle requests through the application-owned manager. Commands block in
//! the shell task (`VmOperation::wait`) so the prompt returns only after the
//! documented postcondition holds; no handler reaches a backend or a mutable
//! architecture object.

use std::{
    collections::btree_map::BTreeMap,
    string::{String, ToString},
    vec::Vec,
};

use anyhow::Context;
use axvm::{VmStatus, VmVcpuState};
use std::fs::read_to_string;

use crate::shell::command::{CommandNode, FlagDef, OptionDef, ParsedCommand};

/// Check if a VM can transition to Running state.
/// Returns Ok(()) if the transition is valid, Err with a message otherwise.
fn can_start_vm(status: VmStatus) -> Result<(), &'static str> {
    match status {
        VmStatus::Ready | VmStatus::Stopped => Ok(()),
        VmStatus::Running => Err("VM is already running"),
        VmStatus::Paused => Err("VM is suspended, use 'vm resume' instead"),
        VmStatus::Stopping => Err("VM is stopping, wait for it to fully stop"),
        VmStatus::Pausing => Err("VM is pausing"),
        VmStatus::Destroying | VmStatus::Destroyed => Err("VM is being destroyed"),
        VmStatus::Failed => Err("VM is failed"),
    }
}

/// Check if a VM can transition to Stopping state.
/// Returns Ok(()) if the transition is valid, Err with a message otherwise.
fn can_stop_vm(status: VmStatus, force: bool) -> Result<(), &'static str> {
    match status {
        VmStatus::Running | VmStatus::Paused => Ok(()),
        VmStatus::Stopping => {
            if force {
                Ok(())
            } else {
                Err("VM is already stopping")
            }
        }
        VmStatus::Stopped => Err("VM is already stopped"),
        VmStatus::Ready => Ok(()), // Allow stopping VMs before their first start.
        VmStatus::Pausing => Err("VM is pausing"),
        VmStatus::Destroying | VmStatus::Destroyed => Err("VM is being destroyed"),
        VmStatus::Failed => Err("VM is failed"),
    }
}

/// Check if a VM can be suspended.
fn can_suspend_vm(status: VmStatus) -> Result<(), &'static str> {
    match status {
        VmStatus::Running => Ok(()),
        VmStatus::Paused => Err("VM is already suspended"),
        VmStatus::Stopped => Err("VM is stopped, cannot suspend"),
        VmStatus::Stopping => Err("VM is stopping, cannot suspend"),
        VmStatus::Ready => Err("VM is not running, cannot suspend"),
        VmStatus::Pausing => Err("VM is already pausing"),
        VmStatus::Destroying | VmStatus::Destroyed => Err("VM is being destroyed"),
        VmStatus::Failed => Err("VM is failed"),
    }
}

/// Check if a VM can be resumed.
fn can_resume_vm(status: VmStatus) -> Result<(), &'static str> {
    match status {
        VmStatus::Paused => Ok(()),
        VmStatus::Running => Err("VM is already running"),
        VmStatus::Stopped => Err("VM is stopped, use 'vm start' instead"),
        VmStatus::Stopping => Err("VM is stopping, cannot resume"),
        VmStatus::Ready => Err("VM is not started yet, use 'vm start' instead"),
        VmStatus::Pausing => Err("VM is pausing, wait before resuming"),
        VmStatus::Destroying | VmStatus::Destroyed => Err("VM is being destroyed"),
        VmStatus::Failed => Err("VM is failed"),
    }
}

/// Format memory size in a human-readable way.
fn format_memory_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{}KB", bytes / 1024)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{}MB", bytes / (1024 * 1024))
    } else {
        format!("{}GB", bytes / (1024 * 1024 * 1024))
    }
}

/// Short source label used by the `vm list` vCPU state summary.
fn vcpu_state_short(state: VmVcpuState) -> &'static str {
    match state {
        VmVcpuState::Free => "Free",
        VmVcpuState::Running => "Run",
        VmVcpuState::Blocked => "Blk",
        VmVcpuState::Invalid => "Inv",
        VmVcpuState::Created => "Cre",
        VmVcpuState::Ready => "Rdy",
        VmVcpuState::Starting => "Sta",
    }
}

/// Full source label used by the detailed vCPU view.
fn vcpu_state_long(state: VmVcpuState) -> &'static str {
    match state {
        VmVcpuState::Free => "Free",
        VmVcpuState::Running => "Running",
        VmVcpuState::Blocked => "Blocked",
        VmVcpuState::Invalid => "Invalid",
        VmVcpuState::Created => "Created",
        VmVcpuState::Ready => "Ready",
        VmVcpuState::Starting => "Starting",
    }
}

// ============================================================================
// Command Handlers
// ============================================================================

fn vm_help(_cmd: &ParsedCommand) {
    println!("VM - virtual machine management");
    println!();
    println!("Most commonly used vm commands:");
    println!("  create    Create a new virtual machine");
    println!("  start     Start a virtual machine");
    println!("  console   Attach a running virtual machine console");
    println!("  stop      Stop a virtual machine");
    println!("  suspend   Suspend (pause) a running virtual machine");
    println!("  resume    Resume a suspended virtual machine");
    println!("  reset     Reset and restart a virtual machine");
    println!("  delete    Delete a virtual machine");
    println!();
    println!("Information commands:");
    println!("  list      Show table of all VMs");
    println!("  show      Show VM details (requires VM_ID)");
    println!("            - Default: basic information");
    println!("            - --full: complete detailed information");
    println!("            - --config: show configuration");
    println!("            - --stats: show statistics");
    println!();
    println!("Use 'vm <command> --help' for more information on a specific command.");
}

fn vm_create(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;

    println!("Positional args: {:?}", args);

    if args.is_empty() {
        println!("Error: No VM configuration file specified");
        println!("Usage: vm create [CONFIG_FILE]");
        return;
    }

    let initial_vm_count = crate::manager::manager().list().len();

    for config_path in args.iter() {
        println!("Creating VM from config: {}", config_path);

        match read_to_string(config_path) {
            Ok(raw_cfg) => match crate::manager::manager().create_vm_from_toml(&raw_cfg) {
                Ok(operation) => match operation.wait() {
                    Ok(vm) => println!(
                        "✓ Successfully created VM[{}] from config: {}",
                        vm.vm_id(),
                        config_path
                    ),
                    Err(error) => {
                        println!("✗ Failed to create VM from {config_path}: {error:#}");
                    }
                },
                Err(error) => {
                    println!("✗ Failed to create VM from {config_path}: {error:#}");
                }
            },
            Err(e) => {
                println!("✗ Failed to read config file {}: {:?}", config_path, e);
            }
        }
    }

    // Check the actual number of VMs created
    let final_vm_count = crate::manager::manager().list().len();
    let created_count = final_vm_count - initial_vm_count;

    if created_count > 0 {
        println!("Successfully created {} VM(s)", created_count);
        println!("Use 'vm start <VM_ID>' to start the created VMs.");
    } else {
        println!("No VMs were created.");
    }
}

fn vm_start(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let detach = cmd.flags.contains("detach");
    let attach_console = cmd.flags.contains("console");

    if detach && attach_console {
        println!("Error: --detach and --console cannot be used together");
        return;
    }
    if attach_console && args.len() != 1 {
        println!("Error: --console requires exactly one VM_ID");
        println!("Usage: vm start --console <VM_ID>");
        return;
    }

    if args.is_empty() {
        // start all VMs
        info!("VMM starting, booting all VMs...");
        let mut started_count = 0;

        for vm in crate::manager::manager().list() {
            let vm_id = vm.vm_id();
            // Check current status before starting
            let status: VmStatus = vm.snapshot().state;
            if status == VmStatus::Running {
                println!("⚠ VM[{}] is already running, skipping", vm_id);
                continue;
            }

            if status != VmStatus::Ready && status != VmStatus::Stopped {
                println!("⚠ VM[{}] is in {:?} state, cannot start", vm_id, status);
                continue;
            }

            if let Err(e) = start_single_vm(vm_id) {
                println!("✗ VM[{}] failed to start: {e:#}", vm_id);
            } else {
                println!("✓ VM[{}] started successfully", vm_id);
                started_count += 1;
            }
        }
        println!("Started {} VM(s)", started_count);
    } else {
        // Start specified VMs
        for vm_name in args {
            // Try to parse as VM ID or lookup VM name
            if let Ok(vm_id) = vm_name.parse::<usize>() {
                start_vm_by_id(vm_id, attach_console);
            } else {
                println!("Error: VM name lookup not implemented. Use VM ID instead.");
                println!("Available VMs:");
                vm_list_simple();
            }
        }
    }

    if detach {
        println!("VMs started in background mode");
    }
}

/// Start a single VM and block until its start postcondition holds.
fn start_single_vm(vm_id: usize) -> anyhow::Result<()> {
    let Some(vm) = crate::manager::manager().get(vm_id) else {
        anyhow::bail!("VM[{vm_id}] not found");
    };

    // Validate state transition using helper function
    can_start_vm(vm.snapshot().state).map_err(anyhow::Error::msg)?;
    crate::manager::manager()
        .start_vm(vm_id)
        .with_context(|| format!("boot VM[{vm_id}]"))?;
    crate::guest_console::mark_running(vm_id);
    Ok(())
}

fn start_vm_by_id(vm_id: usize, attach_console: bool) {
    match start_single_vm(vm_id) {
        Ok(()) => {
            println!("✓ VM[{}] started successfully", vm_id);
            if attach_console {
                match crate::guest_console::attach(vm_id) {
                    Ok(crate::guest_console::ConsoleAttachment::Interactive) => {
                        println!(
                            "✓ Attached VM[{vm_id}] console; use Ctrl+X, then h to return to the \
                             shell"
                        );
                        crate::guest_console::activate(vm_id);
                    }
                    Ok(crate::guest_console::ConsoleAttachment::Replayed) => {
                        println!("✓ Replayed buffered VM[{vm_id}] console output");
                    }
                    Err(error) => println!("✗ Failed to attach VM[{vm_id}] console: {error:#}"),
                }
            }
        }
        Err(err) => {
            println!("✗ VM[{vm_id}] failed to start: {err:#}");
        }
    }
}

fn vm_stop(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let force = cmd.flags.contains("force");

    if args.is_empty() {
        println!("Error: No VM specified");
        println!("Usage: vm stop [OPTIONS] <VM_ID>");
        return;
    }

    for vm_name in args {
        if let Ok(vm_id) = vm_name.parse::<usize>() {
            stop_vm_by_id(vm_id, force);
        } else {
            println!("Error: Invalid VM ID: {}", vm_name);
        }
    }
}

fn stop_vm_by_id(vm_id: usize, force: bool) {
    let Some(vm) = crate::manager::manager().get(vm_id) else {
        println!("✗ VM[{}] not found", vm_id);
        return;
    };

    let status = vm.snapshot().state;
    if let Err(message) = can_stop_vm(status, force) {
        println!("✗ Failed to stop VM[{vm_id}]: {message}");
        return;
    }

    // Print appropriate message based on status
    match status {
        VmStatus::Stopping if force => {
            println!("Force stopping VM[{}]...", vm_id);
        }
        VmStatus::Running => {
            if force {
                println!("Force stopping VM[{}]...", vm_id);
            } else {
                println!("Gracefully stopping VM[{}]...", vm_id);
            }
        }
        VmStatus::Ready => {
            println!(
                "⚠ VM[{}] is in {:?} state, stopping anyway...",
                vm_id, status
            );
        }
        _ => {}
    }

    match crate::manager::manager().stop_vm(vm_id) {
        Ok(()) => {
            crate::guest_console::mark_stopped(vm_id);
            println!("✓ VM[{}] stopped successfully", vm_id);
        }
        Err(err) => {
            println!("✗ Failed to stop VM[{vm_id}]: {err:#}");
        }
    }
}

/// Reset a VM through the AxVM lifecycle state machine.
fn vm_reset(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;

    if args.is_empty() {
        println!("Error: No VM specified");
        println!("Usage: vm reset <VM_ID>");
        return;
    }

    for vm_name in args {
        if let Ok(vm_id) = vm_name.parse::<usize>() {
            reset_vm_by_id(vm_id);
        } else {
            println!("Error: Invalid VM ID: {}", vm_name);
        }
    }
}

fn reset_vm_by_id(vm_id: usize) {
    println!("Resetting VM[{}]...", vm_id);
    match crate::manager::manager().reset_vm(vm_id) {
        Ok(()) => {
            crate::guest_console::mark_running(vm_id);
            println!("✓ VM[{}] reset and started successfully", vm_id);
        }
        Err(err) => println!("✗ VM[{vm_id}] reset failed: {err:#}"),
    }
}

/// Suspend a running VM.
fn vm_suspend(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;

    if args.is_empty() {
        println!("Error: No VM specified");
        println!("Usage: vm suspend <VM_ID>...");
        return;
    }

    for vm_name in args {
        if let Ok(vm_id) = vm_name.parse::<usize>() {
            suspend_vm_by_id(vm_id);
        } else {
            println!("Error: Invalid VM ID: {}", vm_name);
        }
    }
}

fn suspend_vm_by_id(vm_id: usize) {
    println!("Suspending VM[{}]...", vm_id);

    let Some(vm) = crate::manager::manager().get(vm_id) else {
        println!("✗ VM[{}] not found", vm_id);
        return;
    };

    if let Err(message) = can_suspend_vm(vm.snapshot().state) {
        println!("✗ Failed to suspend VM[{vm_id}]: {message}");
        return;
    }

    // `pause` returns only after every participating vCPU has unloaded and
    // parked, so the shell reports the observed park count directly.
    match crate::manager::manager().pause_vm(vm_id) {
        Ok(()) => {
            let snapshot = vm.snapshot();
            let parked = snapshot
                .vcpu
                .iter()
                .filter(|vcpu| matches!(vcpu.state, VmVcpuState::Blocked))
                .count();
            log::info!("VM[{vm_id}] paused");
            println!("✓ VM[{}] suspended", vm_id);
            println!(
                "  {}/{} VCpu task(s) parked at the VMExit boundary",
                parked, snapshot.cpu.vcpu_num
            );
            println!("  Use 'vm resume {}' to resume the VM", vm_id);
        }
        Err(err) => {
            println!("✗ Failed to suspend VM[{vm_id}]: {err:#}");
        }
    }
}

/// Resume a suspended VM.
fn vm_resume(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;

    if args.is_empty() {
        println!("Error: No VM specified");
        println!("Usage: vm resume <VM_ID>...");
        return;
    }

    for vm_name in args {
        if let Ok(vm_id) = vm_name.parse::<usize>() {
            resume_vm_by_id(vm_id);
        } else {
            println!("Error: Invalid VM ID: {}", vm_name);
        }
    }
}

fn resume_vm_by_id(vm_id: usize) {
    println!("Resuming VM[{}]...", vm_id);

    let Some(vm) = crate::manager::manager().get(vm_id) else {
        println!("✗ VM[{}] not found", vm_id);
        return;
    };

    if let Err(message) = can_resume_vm(vm.snapshot().state) {
        println!("✗ Failed to resume VM[{vm_id}]: {message}");
        return;
    }

    match crate::manager::manager().resume_vm(vm_id) {
        Ok(()) => {
            crate::guest_console::mark_running(vm_id);
            log::info!("VM[{vm_id}] resumed");
            println!("✓ VM[{}] resumed successfully", vm_id);
        }
        Err(err) => {
            println!("✗ Failed to resume VM[{vm_id}]: {err:#}");
        }
    }
}

fn vm_delete(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let force = cmd.flags.contains("force");
    let keep_data = cmd.flags.contains("keep-data");

    if args.is_empty() {
        println!("Error: No VM specified");
        println!("Usage: vm delete [OPTIONS] <VM_ID>");
        return;
    }

    let vm_name = &args[0];

    if let Ok(vm_id) = vm_name.parse::<usize>() {
        // Check if VM exists and get its status first
        let Some(status) = crate::manager::manager()
            .get(vm_id)
            .map(|vm| vm.snapshot().state)
        else {
            println!("✗ VM[{}] not found", vm_id);
            return;
        };

        // Check if VM is running
        match status {
            VmStatus::Running => {
                if !force {
                    println!("✗ VM[{}] is currently running", vm_id);
                    println!(
                        "  Use 'vm stop {}' first, or use '--force' to force delete",
                        vm_id
                    );
                    return;
                }
                println!("⚠ Force deleting running VM[{}]...", vm_id);
            }
            VmStatus::Stopping => {
                if !force {
                    println!("⚠ VM[{}] is currently stopping", vm_id);
                    println!("  Wait for it to stop completely, or use '--force' to force delete");
                    return;
                }
                println!("⚠ Force deleting stopping VM[{}]...", vm_id);
            }
            VmStatus::Stopped => {
                println!("Deleting stopped VM[{}]...", vm_id);
            }
            _ => {
                println!("⚠ VM[{}] is in {:?} state", vm_id, status);
                if !force {
                    println!("Use --force to force delete");
                    return;
                }
            }
        }

        delete_vm_by_id(vm_id, keep_data);
    } else {
        println!("Error: Invalid VM ID: {}", vm_name);
    }
}

fn delete_vm_by_id(vm_id: usize, keep_data: bool) {
    // Capture the console backend identity before destroying so a later VM that
    // reuses the identifier cannot inherit this instance's console state.
    let console_backend = crate::guest_console::backend_identity(vm_id);
    match crate::manager::manager().destroy_vm(vm_id) {
        Ok(()) => {
            if let Some(identity) = console_backend {
                crate::guest_console::remove_if_backend(identity);
            }
            crate::guest_console::mark_stopped(vm_id);
            println!("✓ VM[{}] removed from VM list", vm_id);

            if keep_data {
                println!("✓ VM[{}] deleted (configuration and data preserved)", vm_id);
            } else {
                println!("✓ VM[{}] deleted completely", vm_id);
            }
        }
        Err(err) => {
            println!("✗ Failed to remove VM[{vm_id}] from list: {err:#}");
        }
    }

    println!("✓ VM[{}] deletion completed", vm_id);
}

fn vm_console(cmd: &ParsedCommand) {
    let [vm_id] = cmd.positional_args.as_slice() else {
        println!("Error: exactly one VM_ID is required");
        println!("Usage: vm console <VM_ID>");
        return;
    };
    let Ok(vm_id) = vm_id.parse::<usize>() else {
        println!("Error: invalid VM ID: {vm_id}");
        return;
    };

    match crate::guest_console::attach(vm_id) {
        Ok(crate::guest_console::ConsoleAttachment::Interactive) => {
            println!("✓ Attached VM[{vm_id}] console; use Ctrl+X, then h to return to the shell");
            crate::guest_console::activate(vm_id);
        }
        Ok(crate::guest_console::ConsoleAttachment::Replayed) => {
            println!("✓ Replayed buffered VM[{vm_id}] console output");
        }
        Err(error) => println!("✗ Failed to attach VM[{vm_id}] console: {error:#}"),
    }
}

fn vm_list_simple() {
    let vms = crate::manager::manager().list();
    println!("ID    NAME           STATE      VCPU   MEMORY");
    println!("----  -----------    -------    ----   ------");
    for vm in vms {
        let snapshot = vm.snapshot();
        println!(
            "{:<4}  {:<11}    {:<7}    {:<4}   {}",
            snapshot.vm_id,
            snapshot.name,
            snapshot.state.as_str(),
            snapshot.cpu.vcpu_num,
            format_memory_size(snapshot.memory.total_bytes)
        );
    }
}

fn vm_list(cmd: &ParsedCommand) {
    let binding = "table".to_string();
    let format = cmd.options.get("format").unwrap_or(&binding);

    let display_vms = crate::manager::manager().list();

    if display_vms.is_empty() {
        println!("No virtual machines found.");
        return;
    }

    if format == "json" {
        // JSON output
        println!("{{");
        println!("  \"vms\": [");
        for (i, vm) in display_vms.iter().enumerate() {
            let snapshot = vm.snapshot();
            println!("    {{");
            println!("      \"id\": {},", snapshot.vm_id);
            println!("      \"name\": \"{}\",", snapshot.name);
            println!("      \"state\": \"{}\",", snapshot.state.as_str());
            println!("      \"vcpu\": {},", snapshot.cpu.vcpu_num);
            println!(
                "      \"memory\": \"{}\"",
                format_memory_size(snapshot.memory.total_bytes)
            );

            if i < display_vms.len() - 1 {
                println!("    }},");
            } else {
                println!("    }}");
            }
        }
        println!("  ]");
        println!("}}");
    } else {
        // Table output (default)
        println!(
            "{:<6} {:<15} {:<12} {:<15} {:<10} {:<20}",
            "VM ID", "NAME", "STATUS", "VCPU", "MEMORY", "VCPU STATE"
        );
        println!(
            "{:-<6} {:-<15} {:-<12} {:-<15} {:-<10} {:-<20}",
            "", "", "", "", "", ""
        );

        for vm in display_vms {
            let snapshot = vm.snapshot();

            // Get VCpu ID list
            let vcpu_ids: Vec<String> = snapshot
                .vcpu
                .iter()
                .map(|vcpu| vcpu.id.to_string())
                .collect();
            let vcpu_id_list = vcpu_ids.join(",");

            // Get VCpu state summary
            let mut state_counts = std::collections::BTreeMap::new();
            for vcpu in &snapshot.vcpu {
                *state_counts
                    .entry(vcpu_state_short(vcpu.state))
                    .or_insert(0) += 1;
            }

            // Format: Run:2,Blk:1
            let summary: Vec<String> = state_counts
                .iter()
                .map(|(state, count)| format!("{}:{}", state, count))
                .collect();
            let vcpu_state_summary = summary.join(",");

            println!(
                "{:<6} {:<15} {:<12} {:<15} {:<10} {:<20}",
                snapshot.vm_id,
                snapshot.name,
                snapshot.state.as_str(),
                vcpu_id_list,
                format_memory_size(snapshot.memory.total_bytes),
                vcpu_state_summary
            );
        }
    }
}

fn vm_show(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let show_config = cmd.flags.contains("config");
    let show_stats = cmd.flags.contains("stats");
    let show_full = cmd.flags.contains("full");

    if args.is_empty() {
        println!("Error: No VM specified");
        println!("Usage: vm show [OPTIONS] <VM_ID>");
        println!();
        println!("Options:");
        println!("  -f, --full     Show full detailed information");
        println!("  -c, --config   Show configuration details");
        println!("  -s, --stats    Show statistics");
        println!();
        println!("Use 'vm list' to see all VMs");
        return;
    }

    // Show specific VM details
    let vm_name = &args[0];
    if let Ok(vm_id) = vm_name.parse::<usize>() {
        if show_full {
            show_vm_full_details(vm_id);
        } else {
            show_vm_basic_details(vm_id, show_config, show_stats);
        }
    } else {
        println!("Error: Invalid VM ID: {}", vm_name);
    }
}

/// Show basic VM information (default view)
fn show_vm_basic_details(vm_id: usize, show_config: bool, show_stats: bool) {
    let Some(vm) = crate::manager::manager().get(vm_id) else {
        println!("✗ VM[{}] not found", vm_id);
        return;
    };

    let snapshot = vm.snapshot();
    let status = snapshot.state;
    let total_memory = snapshot.memory.total_bytes;

    println!("VM Details: {}", vm_id);
    println!();

    // Basic Information
    println!("  VM ID:     {}", snapshot.vm_id);
    println!("  Name:      {}", snapshot.name);
    println!("  Status:    {}", status.as_str_with_icon());
    println!("  VCPUs:     {}", snapshot.cpu.vcpu_num);
    println!("  Memory:    {}", format_memory_size(total_memory));
    if let Some(error) = &snapshot.last_failure {
        println!("  Last failure: {error}");
    }

    // Add state-specific information
    match status {
        VmStatus::Paused => {
            println!();
            println!("  ℹ VM is paused. Use 'vm resume {}' to continue.", vm_id);
        }
        VmStatus::Stopped => {
            println!();
            println!("  ℹ VM is stopped. Use 'vm delete {}' to clean up.", vm_id);
        }
        VmStatus::Ready => {
            println!();
            println!("  ℹ VM is ready. Use 'vm start {}' to boot.", vm_id);
        }
        _ => {}
    }

    // VCPU Summary
    println!();
    println!("VCPU Summary:");
    let mut state_counts = std::collections::BTreeMap::new();
    for vcpu in &snapshot.vcpu {
        *state_counts.entry(vcpu_state_long(vcpu.state)).or_insert(0) += 1;
    }

    for (state, count) in state_counts {
        println!("  {}: {}", state, count);
    }

    // Memory Summary
    println!();
    println!("Memory Summary:");
    println!("  Total Regions: {}", snapshot.memory.regions.len());
    println!("  Total Size:    {}", format_memory_size(total_memory));

    // Configuration Summary
    if show_config {
        println!();
        println!("Configuration:");
        println!(
            "  BSP Entry:      {:#x}",
            snapshot.description.bsp_entry.as_usize()
        );
        println!(
            "  AP Entry:       {:#x}",
            snapshot.description.ap_entry.as_usize()
        );
        println!(
            "  Address Space:  {:?}",
            snapshot.description.address_space_policy
        );
        if let Some(dtb_addr) = snapshot.description.image.dtb_load_gpa {
            println!("  DTB Address:    {:#x}", dtb_addr.as_usize());
        }
    }

    // Device Summary
    if show_stats {
        println!();
        println!("Device Summary:");
        println!("  Registered Devices: {}", snapshot.device.device_count);
    }

    println!();
    println!("Use 'vm show {} --full' for detailed information", vm_id);
}

/// Show full detailed information about a specific VM (--full flag)
fn show_vm_full_details(vm_id: usize) {
    let Some(vm) = crate::manager::manager().get(vm_id) else {
        println!("✗ VM[{}] not found", vm_id);
        return;
    };

    let snapshot = vm.snapshot();
    let status = snapshot.state;
    let total_memory = snapshot.memory.total_bytes;
    let description = &snapshot.description;

    println!("=== VM Details: {} ===", vm_id);
    println!();

    // Basic Information
    println!("Basic Information:");
    println!("  VM ID:     {}", snapshot.vm_id);
    println!("  Name:      {}", snapshot.name);
    println!("  Status:    {}", status.as_str_with_icon());
    println!("  VCPUs:     {}", snapshot.cpu.vcpu_num);
    println!("  Memory:    {}", format_memory_size(total_memory));
    if let Some(error) = &snapshot.last_failure {
        println!("  Last failure: {error}");
    }
    match snapshot.memory.nested_page_table_root {
        Some(root) => println!("  NPT Root:  {:#x}", root.as_usize()),
        None => println!("  NPT Root:  unavailable (no backing address space)"),
    }

    // Add state-specific information
    match status {
        VmStatus::Paused => {
            println!(
                "    ℹ VM is paused, VCpu tasks are waiting. Use 'vm resume {}' to continue.",
                vm_id
            );
        }
        VmStatus::Stopping => {
            println!("    ℹ VM is shutting down, VCpu tasks are exiting.");
        }
        VmStatus::Stopped => {
            println!(
                "    ℹ VM is stopped, all VCpu tasks have exited. Use 'vm delete {}' to clean up.",
                vm_id
            );
        }
        VmStatus::Ready => {
            println!(
                "    ℹ VM is ready to start. Use 'vm start {}' to boot.",
                vm_id
            );
        }
        _ => {}
    }

    // VCPU Details
    println!();
    println!("VCPU Details:");

    // Count VCpu states for summary
    let mut state_counts = std::collections::BTreeMap::new();
    for vcpu in &snapshot.vcpu {
        *state_counts.entry(vcpu_state_long(vcpu.state)).or_insert(0) += 1;
    }

    // Show summary first
    let summary: Vec<String> = state_counts
        .iter()
        .map(|(state, count)| format!("{}: {}", state, count))
        .collect();
    println!("  Summary: {}", summary.join(", "));
    println!();

    for vcpu in &snapshot.vcpu {
        if let Some(phys_cpu_set) = vcpu.phys_cpu_set {
            println!(
                "  VCPU {}: {} (Affinity: {:#x})",
                vcpu.id,
                vcpu_state_long(vcpu.state),
                phys_cpu_set
            );
        } else {
            println!(
                "  VCPU {}: {} (No affinity)",
                vcpu.id,
                vcpu_state_long(vcpu.state)
            );
        }
    }

    // Add note for Suspended VMs
    if status == VmStatus::Paused {
        println!();
        println!(
            "  Note: VCpu tasks are blocked in wait queue and will resume when VM is unpaused."
        );
    }

    // Memory Regions
    println!();
    println!(
        "Memory Regions: ({} region(s), {} total)",
        snapshot.memory.regions.len(),
        format_memory_size(total_memory)
    );
    for (i, region) in snapshot.memory.regions.iter().enumerate() {
        let region_type = if region.needs_dealloc {
            "Allocated"
        } else {
            "Reserved"
        };
        let identical = if region.is_identical() {
            " [identical]"
        } else {
            ""
        };
        println!(
            "  Region {}: GPA={:#x} HVA={:#x} Size={} Type={}{}",
            i,
            region.gpa,
            region.hva,
            format_memory_size(region.size()),
            region_type,
            identical
        );
    }

    // Configuration
    println!();
    println!("Configuration:");
    println!("  BSP Entry:      {:#x}", description.bsp_entry.as_usize());
    println!("  AP Entry:       {:#x}", description.ap_entry.as_usize());
    println!("  Address Space:  {:?}", description.address_space_policy);

    if let Some(dtb_addr) = description.image.dtb_load_gpa {
        println!("  DTB Address:    {:#x}", dtb_addr.as_usize());
    }

    println!(
        "  Kernel GPA:     {:#x}",
        description.image.kernel_load_gpa.as_usize()
    );

    if !description.passthrough_devices.is_empty() {
        println!();
        println!(
            "  Passthrough Devices: ({} device(s))",
            description.passthrough_devices.len()
        );
        for device in &description.passthrough_devices {
            println!(
                "    - {}: GPA[{:#x}~{:#x}] -> HPA[{:#x}~{:#x}] ({})",
                device.name,
                device.base_gpa,
                device.base_gpa + device.length,
                device.base_hpa,
                device.base_hpa + device.length,
                format_memory_size(device.length)
            );
        }
    }

    if !description.passthrough_addresses.is_empty() {
        println!();
        println!(
            "  Passthrough Memory Regions: ({} region(s))",
            description.passthrough_addresses.len()
        );
        for address in &description.passthrough_addresses {
            println!(
                "    - GPA[{:#x}~{:#x}] ({})",
                address.base_gpa,
                address.base_gpa + address.length,
                format_memory_size(address.length)
            );
        }
    }

    #[cfg(target_arch = "aarch64")]
    if !description.passthrough_irqs.is_empty() {
        println!();
        println!("  Physical IRQ Routes: {:?}", description.passthrough_irqs);
    }

    // Devices
    println!();
    println!("Devices:");
    println!("  Devices:        {}", snapshot.device.device_count);

    // Additional Statistics
    println!();
    println!("Additional Statistics:");
    println!("  Total Memory Regions: {}", snapshot.memory.regions.len());

    // Show VCpu affinity details
    println!();
    println!("  VCpu Affinity Details:");
    for (vcpu_id, affinity, pcpu_id) in &description.vcpu_affinities {
        if let Some(aff) = affinity {
            println!(
                "    VCpu {}: Physical CPU mask {:#x}, PCpu ID {}",
                vcpu_id, aff, pcpu_id
            );
        } else {
            println!(
                "    VCpu {}: No specific affinity, PCpu ID {}",
                vcpu_id, pcpu_id
            );
        }
    }
}

/// Build the VM command tree and register it.
pub fn build_vm_cmd(tree: &mut BTreeMap<String, CommandNode>) {
    let create_cmd = CommandNode::new("Create a new virtual machine")
        .with_handler(vm_create)
        .with_usage("vm create [OPTIONS] <CONFIG_FILE>...")
        .with_option(
            OptionDef::new("name", "Virtual machine name")
                .with_short('n')
                .with_long("name"),
        )
        .with_option(
            OptionDef::new("cpu", "Number of CPU cores")
                .with_short('c')
                .with_long("cpu"),
        )
        .with_option(
            OptionDef::new("memory", "Amount of memory")
                .with_short('m')
                .with_long("memory"),
        )
        .with_flag(
            FlagDef::new("force", "Force creation without confirmation")
                .with_short('f')
                .with_long("force"),
        );

    let start_cmd = CommandNode::new("Start a virtual machine")
        .with_handler(vm_start)
        .with_usage("vm start [OPTIONS] [VM_ID...]")
        .with_flag(
            FlagDef::new("detach", "Start in background")
                .with_short('d')
                .with_long("detach"),
        )
        .with_flag(
            FlagDef::new("console", "Attach to console")
                .with_short('c')
                .with_long("console"),
        );

    let stop_cmd = CommandNode::new("Stop a virtual machine")
        .with_handler(vm_stop)
        .with_usage("vm stop [OPTIONS] <VM_ID>...")
        .with_flag(
            FlagDef::new("force", "Force stop")
                .with_short('f')
                .with_long("force"),
        )
        .with_flag(
            FlagDef::new("graceful", "Graceful shutdown")
                .with_short('g')
                .with_long("graceful"),
        );

    let console_cmd = CommandNode::new("Attach a running virtual machine console")
        .with_handler(vm_console)
        .with_usage("vm console <VM_ID>");

    let reset_cmd = CommandNode::new("Reset and restart a virtual machine")
        .with_handler(vm_reset)
        .with_usage("vm reset <VM_ID>...");

    let suspend_cmd = CommandNode::new("Suspend (pause) a running virtual machine")
        .with_handler(vm_suspend)
        .with_usage("vm suspend <VM_ID>...");

    let resume_cmd = CommandNode::new("Resume a suspended virtual machine")
        .with_handler(vm_resume)
        .with_usage("vm resume <VM_ID>...");

    let delete_cmd = CommandNode::new("Delete a virtual machine")
        .with_handler(vm_delete)
        .with_usage("vm delete [OPTIONS] <VM_ID>")
        .with_flag(
            FlagDef::new("force", "Skip confirmation")
                .with_short('f')
                .with_long("force"),
        )
        .with_flag(FlagDef::new("keep-data", "Keep VM data").with_long("keep-data"));

    let list_cmd = CommandNode::new("Show virtual machine lists")
        .with_handler(vm_list)
        .with_usage("vm list [OPTIONS]")
        .with_flag(
            FlagDef::new("all", "Show all VMs including stopped ones")
                .with_short('a')
                .with_long("all"),
        )
        .with_option(OptionDef::new("format", "Output format (table, json)").with_long("format"));

    let show_cmd = CommandNode::new("Show detailed VM information")
        .with_handler(vm_show)
        .with_usage("vm show [OPTIONS] <VM_ID>")
        .with_flag(
            FlagDef::new("full", "Show full detailed information")
                .with_short('f')
                .with_long("full"),
        )
        .with_flag(
            FlagDef::new("config", "Show configuration details")
                .with_short('c')
                .with_long("config"),
        )
        .with_flag(
            FlagDef::new("stats", "Show device statistics")
                .with_short('s')
                .with_long("stats"),
        );

    // main VM command
    let mut vm_node = CommandNode::new("Virtual machine management")
        .with_handler(vm_help)
        .with_usage("vm <command> [options] [args...]")
        .add_subcommand(
            "help",
            CommandNode::new("Show VM help").with_handler(vm_help),
        );

    {
        vm_node = vm_node
            .add_subcommand("create", create_cmd)
            .add_subcommand("start", start_cmd);
    }

    vm_node = vm_node
        .add_subcommand("console", console_cmd)
        .add_subcommand("stop", stop_cmd)
        .add_subcommand("suspend", suspend_cmd)
        .add_subcommand("resume", resume_cmd)
        .add_subcommand("reset", reset_cmd)
        .add_subcommand("delete", delete_cmd)
        .add_subcommand("list", list_cmd)
        .add_subcommand("show", show_cmd);

    tree.insert("vm".to_string(), vm_node);
}
