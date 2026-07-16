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

use std::{
    collections::btree_map::BTreeMap,
    println,
    string::{String, ToString},
    thread,
    vec::Vec,
};

use anyhow::Context;
use axvm::{StopReason, VmStatus, VmVcpuState};
#[cfg(feature = "fs")]
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

// ============================================================================
// Command Handlers
// ============================================================================

fn vm_help(_cmd: &ParsedCommand) {
    println!("VM - virtual machine management");
    println!();
    println!("Most commonly used vm commands:");
    println!("  create    Create a new virtual machine");
    println!("  start     Start a virtual machine");
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

#[cfg(feature = "fs")]
fn vm_create(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;

    println!("Positional args: {:?}", args);

    if args.is_empty() {
        println!("Error: No VM configuration file specified");
        println!("Usage: vm create [CONFIG_FILE]");
        return;
    }

    let initial_vm_count = crate::manager::AxvmManager::vm_list().len();

    for config_path in args.iter() {
        println!("Creating VM from config: {}", config_path);

        match read_to_string(config_path) {
            Ok(raw_cfg) => match crate::manager::AxvmManager::create_vm_from_toml(&raw_cfg) {
                Ok(vm_id) => {
                    println!(
                        "✓ Successfully created VM[{}] from config: {}",
                        vm_id, config_path
                    );
                }
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
    let final_vm_count = crate::manager::AxvmManager::vm_list().len();
    let created_count = final_vm_count - initial_vm_count;

    if created_count > 0 {
        println!("Successfully created {} VM(s)", created_count);
        println!("Use 'vm start <VM_ID>' to start the created VMs.");
    } else {
        println!("No VMs were created.");
    }
}

#[cfg(feature = "fs")]
fn vm_start(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let detach = cmd.flags.contains("detach");

    if args.is_empty() {
        // start all VMs
        info!("VMM starting, booting all VMs...");
        let mut started_count = 0;

        for vm in crate::manager::AxvmManager::vm_list() {
            // Check current status before starting
            let status: VmStatus = vm.status();
            if status == VmStatus::Running {
                println!("⚠ VM[{}] is already running, skipping", vm.id());
                continue;
            }

            if status != VmStatus::Ready && status != VmStatus::Stopped {
                println!("⚠ VM[{}] is in {:?} state, cannot start", vm.id(), status);
                continue;
            }

            if let Err(e) = start_single_vm(vm.clone()) {
                println!("✗ VM[{}] failed to start: {e:#}", vm.id());
            } else {
                println!("✓ VM[{}] started successfully", vm.id());
                started_count += 1;
            }
        }
        println!("Started {} VM(s)", started_count);
    } else {
        // Start specified VMs
        for vm_name in args {
            // Try to parse as VM ID or lookup VM name
            if let Ok(vm_id) = vm_name.parse::<usize>() {
                start_vm_by_id(vm_id);
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

/// Start a single VM by setting up vCPUs and calling boot.
/// Returns Ok(()) if successful, Err otherwise.
fn start_single_vm(vm: axvm::AxVMRef) -> anyhow::Result<()> {
    let vm_id = vm.id();
    let status = vm.status();

    // Validate state transition using helper function
    can_start_vm(status).map_err(anyhow::Error::msg)?;
    crate::manager::AxvmManager::start_vm(vm_id).with_context(|| format!("boot VM[{vm_id}]"))
}

fn start_vm_by_id(vm_id: usize) {
    match crate::manager::AxvmManager::with_vm(vm_id, |vm| start_single_vm(vm.clone())) {
        Some(Ok(_)) => {
            println!("✓ VM[{}] started successfully", vm_id);
        }
        Some(Err(err)) => {
            println!("✗ VM[{vm_id}] failed to start: {err:#}");
        }
        None => {
            println!("✗ VM[{}] not found", vm_id);
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
    match crate::manager::AxvmManager::with_vm(vm_id, |vm| {
        let status = vm.status();

        // Validate state transition using helper function
        can_stop_vm(status, force).map_err(anyhow::Error::msg)?;

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

        // Call shutdown
        crate::manager::AxvmManager::stop_vm(vm_id)
            .with_context(|| format!("send shutdown request to VM[{vm_id}]"))
    }) {
        Some(Ok(_)) => {
            println!("✓ VM[{}] stop signal sent successfully", vm_id);
            println!(
                "  Note: vCPU threads will exit gracefully, VM status will transition to Stopped"
            );
        }
        Some(Err(err)) => {
            println!("✗ Failed to stop VM[{vm_id}]: {err:#}");
        }
        None => {
            println!("✗ VM[{}] not found", vm_id);
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
    match crate::manager::AxvmManager::reset_vm(vm_id) {
        Ok(()) => println!("✓ VM[{}] reset and started successfully", vm_id),
        Err(err) => println!("✗ VM[{vm_id}] reset failed: {err:#}"),
    }
}

/// Compatibility alias for the old shell command name.
fn vm_restart(cmd: &ParsedCommand) {
    if cmd.flags.contains("force") {
        println!("⚠ --force is ignored; reset always rebuilds runtime state");
    }
    vm_reset(cmd);
}

/// Suspend a running VM (functionality incomplete)
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

    let result: Option<anyhow::Result<()>> = crate::manager::AxvmManager::with_vm(vm_id, |vm| {
        let status = vm.status();

        // Check if VM can be suspended
        can_suspend_vm(status).map_err(anyhow::Error::msg)?;

        vm.pause().with_context(|| format!("suspend VM[{vm_id}]"))?;
        info!("VM[{}] status set to Paused", vm_id);

        Ok(())
    });

    match result {
        Some(Ok(_)) => {
            println!("✓ VM[{}] suspend signal sent", vm_id);

            // Get VM to check VCpu count
            let vcpu_count =
                crate::manager::AxvmManager::with_vm(vm_id, |vm| vm.vcpu_num()).unwrap_or(0);
            println!(
                "  Note: {} VCpu task(s) will enter wait queue at next VMExit",
                vcpu_count
            );

            // Wait a brief moment for VCpus to enter suspended state
            println!("  Waiting for VCpus to suspend...");
            let max_wait_iterations = 10; // 1 second timeout (10 * 100ms)
            let mut iterations = 0;
            let mut all_suspended = false;

            while iterations < max_wait_iterations {
                // Check if all VCpus are in blocked state
                if let Some(vm) = crate::manager::AxvmManager::vm_by_id(vm_id) {
                    let vcpu_states: Vec<_> =
                        vm.vcpu_snapshots().iter().map(|vcpu| vcpu.state).collect();

                    let blocked_count = vcpu_states
                        .iter()
                        .filter(|s| matches!(s, VmVcpuState::Blocked))
                        .count();

                    if blocked_count == vcpu_states.len() {
                        all_suspended = true;
                        break;
                    }

                    // Show progress for the first few iterations
                    if iterations < 3 {
                        debug!("  VCpus blocked: {}/{}", blocked_count, vcpu_states.len());
                    }
                }

                iterations += 1;
                thread::sleep(core::time::Duration::from_millis(100));
            }

            if all_suspended {
                println!("✓ All VCpu tasks are now suspended");
            } else {
                println!("⚠ Some VCpu tasks may still be transitioning to suspended state");
                println!("  VCpus will suspend at next VMExit (timer interrupt, I/O, etc.)");
                println!("  This is normal for VMs with low interrupt rates");
            }

            println!("  Use 'vm resume {}' to resume the VM", vm_id);
        }
        Some(Err(err)) => {
            println!("✗ Failed to suspend VM[{vm_id}]: {err:#}");
        }
        None => {
            println!("✗ VM[{}] not found", vm_id);
        }
    }
}

// Resume a suspended VM (functionality incomplete)
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

    let result: Option<anyhow::Result<()>> = crate::manager::AxvmManager::with_vm(vm_id, |vm| {
        let status = vm.status();

        // Check if VM can be resumed
        can_resume_vm(status).map_err(anyhow::Error::msg)?;

        crate::manager::AxvmManager::resume_vm(vm_id)
            .with_context(|| format!("resume suspended VM[{vm_id}]"))?;

        info!("VM[{}] resumed", vm_id);
        Ok(())
    });

    match result {
        Some(Ok(_)) => {
            println!("✓ VM[{}] resumed successfully", vm_id);
        }
        Some(Err(err)) => {
            println!("✗ Failed to resume VM[{vm_id}]: {err:#}");
        }
        None => {
            println!("✗ VM[{}] not found", vm_id);
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
        let Some(status) = crate::manager::AxvmManager::with_vm(vm_id, |vm| vm.status()) else {
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
    // First check VM status and try to stop it if running
    let vm_status = crate::manager::AxvmManager::with_vm(vm_id, |vm| {
        let status = vm.status();

        // If VM is running, suspended, or stopping, send shutdown signal
        match status {
            VmStatus::Running | VmStatus::Paused | VmStatus::Stopping => {
                println!(
                    "  VM[{}] is {:?}, sending shutdown signal...",
                    vm_id, status
                );
                let _ = crate::manager::AxvmManager::stop_vm(vm_id);
            }
            VmStatus::Ready => {
                let _ = vm.stop(StopReason::Forced);
            }
            _ => {}
        }

        status
    });

    if vm_status.is_none() {
        println!("✗ VM[{}] not found or already removed", vm_id);
        return;
    }

    // Remove VM from global list
    // Note: This drops the reference from the global list, but the VM object
    // will only be fully destroyed when all vCPU threads exit and drop their references
    match crate::manager::AxvmManager::remove_vm(vm_id) {
        Some(vm) => {
            if let Err(err) = vm.destroy() {
                println!("⚠ VM[{vm_id}] destroy failed: {err}");
            }
            println!("✓ VM[{}] removed from VM list", vm_id);

            if keep_data {
                println!("✓ VM[{}] deleted (configuration and data preserved)", vm_id);
            } else {
                println!("✓ VM[{}] deleted completely", vm_id);

                // TODO: Clean up VM-related data files
                // - Remove disk images
                // - Remove configuration files
                // - Remove log files
            }
        }
        None => {
            println!(
                "✗ Failed to remove VM[{}] from list (may have been removed already)",
                vm_id
            );
        }
    }

    println!("✓ VM[{}] deletion completed", vm_id);
}

#[cfg(feature = "fs")]
fn vm_list_simple() {
    let vms = crate::manager::AxvmManager::vm_list();
    println!("ID    NAME           STATE      VCPU   MEMORY");
    println!("----  -----------    -------    ----   ------");
    for vm in vms {
        let status = vm.status();

        // Calculate total memory size
        let total_memory: usize = vm.memory_regions().iter().map(|region| region.size()).sum();

        println!(
            "{:<4}  {:<11}    {:<7}    {:<4}   {}",
            vm.id(),
            vm.with_config(|cfg| cfg.name()),
            status.as_str(),
            vm.vcpu_num(),
            format_memory_size(total_memory)
        );
    }
}

fn vm_list(cmd: &ParsedCommand) {
    let binding = "table".to_string();
    let format = cmd.options.get("format").unwrap_or(&binding);

    let display_vms = crate::manager::AxvmManager::vm_list();

    if display_vms.is_empty() {
        println!("No virtual machines found.");
        return;
    }

    if format == "json" {
        // JSON output
        println!("{{");
        println!("  \"vms\": [");
        for (i, vm) in display_vms.iter().enumerate() {
            let status = vm.status();
            let total_memory: usize = vm.memory_regions().iter().map(|region| region.size()).sum();

            println!("    {{");
            println!("      \"id\": {},", vm.id());
            println!("      \"name\": \"{}\",", vm.with_config(|cfg| cfg.name()));
            println!("      \"state\": \"{}\",", status.as_str());
            println!("      \"vcpu\": {},", vm.vcpu_num());
            println!("      \"memory\": \"{}\"", format_memory_size(total_memory));

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
            let status = vm.status();
            let total_memory: usize = vm.memory_regions().iter().map(|region| region.size()).sum();

            // Get VCpu ID list
            let vcpu_ids: Vec<String> = vm
                .vcpu_snapshots()
                .iter()
                .map(|vcpu| vcpu.id.to_string())
                .collect();
            let vcpu_id_list = vcpu_ids.join(",");

            // Get VCpu state summary
            let mut state_counts = std::collections::BTreeMap::new();
            for vcpu in vm.vcpu_snapshots() {
                let state = match vcpu.state {
                    VmVcpuState::Free => "Free",
                    VmVcpuState::Running => "Run",
                    VmVcpuState::Blocked => "Blk",
                    VmVcpuState::Invalid => "Inv",
                    VmVcpuState::Created => "Cre",
                    VmVcpuState::Ready => "Rdy",
                };
                *state_counts.entry(state).or_insert(0) += 1;
            }

            // Format: Run:2,Blk:1
            let summary: Vec<String> = state_counts
                .iter()
                .map(|(state, count)| format!("{}:{}", state, count))
                .collect();
            let vcpu_state_summary = summary.join(",");

            println!(
                "{:<6} {:<15} {:<12} {:<15} {:<10} {:<20}",
                vm.id(),
                vm.with_config(|cfg| cfg.name()),
                status.as_str(),
                vcpu_id_list,
                format_memory_size(total_memory),
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
    match crate::manager::AxvmManager::with_vm(vm_id, |vm| {
        let status = vm.status();

        println!("VM Details: {}", vm_id);
        println!();

        // Basic Information
        println!("  VM ID:     {}", vm.id());
        println!("  Name:      {}", vm.with_config(|cfg| cfg.name()));
        println!("  Status:    {}", status.as_str_with_icon());
        println!("  VCPUs:     {}", vm.vcpu_num());

        // Calculate total memory
        let total_memory: usize = vm.memory_regions().iter().map(|region| region.size()).sum();
        println!("  Memory:    {}", format_memory_size(total_memory));

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
        for vcpu in vm.vcpu_snapshots() {
            let state = match vcpu.state {
                VmVcpuState::Free => "Free",
                VmVcpuState::Running => "Running",
                VmVcpuState::Blocked => "Blocked",
                VmVcpuState::Invalid => "Invalid",
                VmVcpuState::Created => "Created",
                VmVcpuState::Ready => "Ready",
            };
            *state_counts.entry(state).or_insert(0) += 1;
        }

        for (state, count) in state_counts {
            println!("  {}: {}", state, count);
        }

        // Memory Summary
        println!();
        println!("Memory Summary:");
        println!("  Total Regions: {}", vm.memory_regions().len());
        println!("  Total Size:    {}", format_memory_size(total_memory));

        // Configuration Summary
        if show_config {
            println!();
            println!("Configuration:");
            vm.with_config(|cfg| {
                println!("  BSP Entry:      {:#x}", cfg.bsp_entry().as_usize());
                println!("  AP Entry:       {:#x}", cfg.ap_entry().as_usize());
                println!("  Interrupt Delivery: {:?}", cfg.interrupt_delivery());
                if let Some(dtb_addr) = cfg.image_config().dtb_load_gpa {
                    println!("  DTB Address:    {:#x}", dtb_addr.as_usize());
                }
            });
        }

        // Device Summary
        if show_stats {
            println!();
            println!("Device Summary:");
            println!("  Registered Devices: {}", vm.device_count());
        }

        println!();
        println!("Use 'vm show {} --full' for detailed information", vm_id);
    }) {
        Some(_) => {}
        None => {
            println!("✗ VM[{}] not found", vm_id);
        }
    }
}

/// Show full detailed information about a specific VM (--full flag)
fn show_vm_full_details(vm_id: usize) {
    match crate::manager::AxvmManager::with_vm(vm_id, |vm| {
        let status = vm.status();

        println!("=== VM Details: {} ===", vm_id);
        println!();

        // Basic Information
        println!("Basic Information:");
        println!("  VM ID:     {}", vm.id());
        println!("  Name:      {}", vm.with_config(|cfg| cfg.name()));
        println!("  Status:    {}", status.as_str_with_icon());
        println!("  VCPUs:     {}", vm.vcpu_num());

        // Calculate total memory
        let total_memory: usize = vm.memory_regions().iter().map(|region| region.size()).sum();
        println!("  Memory:    {}", format_memory_size(total_memory));
        match vm.nested_page_table_root() {
            Ok(root) => println!("  NPT Root:  {:#x}", root.as_usize()),
            Err(err) => println!("  NPT Root:  unavailable ({:?})", err),
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
        for vcpu in vm.vcpu_snapshots() {
            let state = match vcpu.state {
                VmVcpuState::Free => "Free",
                VmVcpuState::Running => "Running",
                VmVcpuState::Blocked => "Blocked",
                VmVcpuState::Invalid => "Invalid",
                VmVcpuState::Created => "Created",
                VmVcpuState::Ready => "Ready",
            };
            *state_counts.entry(state).or_insert(0) += 1;
        }

        // Show summary first
        let summary: Vec<String> = state_counts
            .iter()
            .map(|(state, count)| format!("{}: {}", state, count))
            .collect();
        println!("  Summary: {}", summary.join(", "));
        println!();

        for vcpu in vm.vcpu_snapshots() {
            let vcpu_state = match vcpu.state {
                VmVcpuState::Free => "Free",
                VmVcpuState::Running => "Running",
                VmVcpuState::Blocked => "Blocked",
                VmVcpuState::Invalid => "Invalid",
                VmVcpuState::Created => "Created",
                VmVcpuState::Ready => "Ready",
            };

            if let Some(phys_cpu_set) = vcpu.phys_cpu_set {
                println!(
                    "  VCPU {}: {} (Affinity: {:#x})",
                    vcpu.id, vcpu_state, phys_cpu_set
                );
            } else {
                println!("  VCPU {}: {} (No affinity)", vcpu.id, vcpu_state);
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
            vm.memory_regions().len(),
            format_memory_size(total_memory)
        );
        for (i, region) in vm.memory_regions().iter().enumerate() {
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
        vm.with_config(|cfg| {
            println!("  BSP Entry:      {:#x}", cfg.bsp_entry().as_usize());
            println!("  AP Entry:       {:#x}", cfg.ap_entry().as_usize());
            println!("  Interrupt Delivery: {:?}", cfg.interrupt_delivery());

            if let Some(dtb_addr) = cfg.image_config().dtb_load_gpa {
                println!("  DTB Address:    {:#x}", dtb_addr.as_usize());
            }

            // Show kernel info
            println!(
                "  Kernel GPA:     {:#x}",
                cfg.image_config().kernel_load_gpa.as_usize()
            );

            let plan = cfg.machine_plan();
            let passthrough_devices = plan
                .host_devices()
                .iter()
                .filter(|device| {
                    device.disposition() == axvm::machine::DeviceDisposition::Passthrough
                })
                .collect::<Vec<_>>();
            if !passthrough_devices.is_empty() {
                println!();
                println!(
                    "  Passthrough Devices: ({} device(s))",
                    passthrough_devices.len()
                );
                for device in passthrough_devices {
                    println!("    - {}", device.id());
                    for mmio in device.mmio() {
                        println!(
                            "      MMIO: [{:#x}~{:#x}] ({})",
                            mmio.base(),
                            mmio.end(),
                            format_memory_size(mmio.size() as usize)
                        );
                    }
                    if !device.interrupts().is_empty() {
                        println!("      IRQs: {:?}", device.interrupts());
                    }
                }
            }

            if !plan.identity_mappings().is_empty() {
                println!();
                println!(
                    "  Identity-mapped I/O: ({} region(s))",
                    plan.identity_mappings().len()
                );
                for range in plan.identity_mappings() {
                    println!(
                        "    - GPA[{:#x}~{:#x}] ({})",
                        range.base(),
                        range.end(),
                        format_memory_size(range.size() as usize)
                    );
                }
            }

            let host_interrupts = plan.assigned_host_interrupts().collect::<Vec<_>>();
            if !host_interrupts.is_empty() {
                println!();
                println!("  Assigned Host IRQs: {:?}", host_interrupts);
            }

            if !plan.virtual_devices().is_empty() {
                println!();
                println!(
                    "  Virtual Devices: ({} device(s))",
                    plan.virtual_devices().len()
                );
                for device in plan.virtual_devices() {
                    println!(
                        "    - {}: {} {:?}",
                        device.instance_id(),
                        device.model_id(),
                        device.resources()
                    );
                }
            }
        });

        // Devices
        println!();
        let device_count = vm.device_count();
        println!("Devices:");
        println!("  Devices:        {}", device_count);

        // Additional Statistics
        println!();
        println!("Additional Statistics:");
        println!("  Total Memory Regions: {}", vm.memory_regions().len());

        // Show VCpu affinity details
        println!();
        println!("  VCpu Affinity Details:");
        for (vcpu_id, affinity, pcpu_id) in vm.get_vcpu_affinities_pcpu_ids() {
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
    }) {
        Some(_) => {}
        None => {
            println!("✗ VM[{}] not found", vm_id);
        }
    }
}

/// Build the VM command tree and register it.
pub fn build_vm_cmd(tree: &mut BTreeMap<String, CommandNode>) {
    #[cfg(feature = "fs")]
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

    #[cfg(feature = "fs")]
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

    let reset_cmd = CommandNode::new("Reset and restart a virtual machine")
        .with_handler(vm_reset)
        .with_usage("vm reset <VM_ID>...");

    let restart_cmd = CommandNode::new("Restart a virtual machine (alias of reset)")
        .with_handler(vm_restart)
        .with_usage("vm restart [OPTIONS] <VM_ID>...")
        .with_flag(
            FlagDef::new("force", "Force restart")
                .with_short('f')
                .with_long("force"),
        );

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

    #[cfg(feature = "fs")]
    {
        vm_node = vm_node
            .add_subcommand("create", create_cmd)
            .add_subcommand("start", start_cmd);
    }

    vm_node = vm_node
        .add_subcommand("stop", stop_cmd)
        .add_subcommand("suspend", suspend_cmd)
        .add_subcommand("resume", resume_cmd)
        .add_subcommand("reset", reset_cmd)
        .add_subcommand("restart", restart_cmd)
        .add_subcommand("delete", delete_cmd)
        .add_subcommand("list", list_cmd)
        .add_subcommand("show", show_cmd);

    tree.insert("vm".to_string(), vm_node);
}
