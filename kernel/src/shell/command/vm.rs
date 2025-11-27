use std::{
    collections::btree_map::BTreeMap,
    println,
    string::{String, ToString},
    vec::Vec,
};

use axvm::VMStatus;
#[cfg(feature = "fs")]
use std::fs::read_to_string;

use crate::{
    shell::command::{CommandNode, FlagDef, OptionDef, ParsedCommand},
    vmm::{add_running_vm_count, config::init_guest_vm, vcpus, vm_list, with_vm},
};

/// Check if a VM can transition to Running state.
/// Returns Ok(()) if the transition is valid, Err with a message otherwise.
fn can_start_vm(status: VMStatus) -> Result<(), &'static str> {
    match status {
        VMStatus::Loaded | VMStatus::Stopped => Ok(()),
        VMStatus::Running => Err("VM is already running"),
        VMStatus::Suspended => Err("VM is suspended, use 'vm resume' instead"),
        VMStatus::Stopping => Err("VM is stopping, wait for it to fully stop"),
        VMStatus::Loading => Err("VM is still loading"),
    }
}

/// Check if a VM can transition to Stopping state.
/// Returns Ok(()) if the transition is valid, Err with a message otherwise.
fn can_stop_vm(status: VMStatus, force: bool) -> Result<(), &'static str> {
    match status {
        VMStatus::Running | VMStatus::Suspended => Ok(()),
        VMStatus::Stopping => {
            if force {
                Ok(())
            } else {
                Err("VM is already stopping")
            }
        }
        VMStatus::Stopped => Err("VM is already stopped"),
        VMStatus::Loading | VMStatus::Loaded => Ok(()), // Allow stopping VMs in these states
    }
}

/// Check if a VM can be suspended.
fn can_suspend_vm(status: VMStatus) -> Result<(), &'static str> {
    match status {
        VMStatus::Running => Ok(()),
        VMStatus::Suspended => Err("VM is already suspended"),
        VMStatus::Stopped => Err("VM is stopped, cannot suspend"),
        VMStatus::Stopping => Err("VM is stopping, cannot suspend"),
        VMStatus::Loading => Err("VM is loading, cannot suspend"),
        VMStatus::Loaded => Err("VM is not running, cannot suspend"),
    }
}

/// Check if a VM can be resumed.
fn can_resume_vm(status: VMStatus) -> Result<(), &'static str> {
    match status {
        VMStatus::Suspended => Ok(()),
        VMStatus::Running => Err("VM is already running"),
        VMStatus::Stopped => Err("VM is stopped, use 'vm start' instead"),
        VMStatus::Stopping => Err("VM is stopping, cannot resume"),
        VMStatus::Loading => Err("VM is loading, cannot resume"),
        VMStatus::Loaded => Err("VM is not started yet, use 'vm start' instead"),
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
    println!("  restart   Restart a virtual machine");
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

    let initial_vm_count = vm_list::get_vm_list().len();

    for config_path in args.iter() {
        println!("Creating VM from config: {}", config_path);

        match read_to_string(config_path) {
            Ok(raw_cfg) => match init_guest_vm(&raw_cfg) {
                Ok(vm_id) => {
                    println!(
                        "✓ Successfully created VM[{}] from config: {}",
                        vm_id, config_path
                    );
                }
                Err(_) => {
                    println!(
                        "✗ Failed to create VM from {}: Configuration error or panic occurred",
                        config_path
                    );
                }
            },
            Err(e) => {
                println!("✗ Failed to read config file {}: {:?}", config_path, e);
            }
        }
    }

    // Check the actual number of VMs created
    let final_vm_count = vm_list::get_vm_list().len();
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
    let detach = cmd.flags.get("detach").unwrap_or(&false);

    if args.is_empty() {
        // start all VMs
        info!("VMM starting, booting all VMs...");
        let mut started_count = 0;

        for vm in vm_list::get_vm_list() {
            // Check current status before starting
            let status: VMStatus = vm.vm_status();
            if status == VMStatus::Running {
                println!("⚠ VM[{}] is already running, skipping", vm.id());
                continue;
            }

            if status != VMStatus::Loaded && status != VMStatus::Stopped {
                println!("⚠ VM[{}] is in {:?} state, cannot start", vm.id(), status);
                continue;
            }

            if let Err(e) = start_single_vm(vm.clone()) {
                println!("✗ VM[{}] failed to start: {:?}", vm.id(), e);
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

    if *detach {
        println!("VMs started in background mode");
    }
}

/// Start a single VM by setting up vCPUs and calling boot.
/// Returns Ok(()) if successful, Err otherwise.
fn start_single_vm(vm: crate::vmm::VMRef) -> Result<(), &'static str> {
    let vm_id = vm.id();
    let status = vm.vm_status();

    // Validate state transition using helper function
    can_start_vm(status)?;

    // Set up primary virtual CPU before starting
    vcpus::setup_vm_primary_vcpu(vm.clone());

    // Boot the VM
    match vm.boot() {
        Ok(_) => {
            // Transition to Running state and notify the primary VCpu
            // Note: Since the VCpu task is created directly in the wait queue (blocked state),
            // we can immediately notify it without waiting for it to be scheduled first.
            vcpus::notify_primary_vcpu(vm_id);
            add_running_vm_count(1);
            Ok(())
        }
        Err(err) => {
            // Revert status on failure
            error!("Failed to boot VM[{}]: {:?}", vm_id, err);
            Err("Failed to boot VM")
        }
    }
}

fn start_vm_by_id(vm_id: usize) {
    match with_vm(vm_id, |vm| start_single_vm(vm.clone())) {
        Some(Ok(_)) => {
            println!("✓ VM[{}] started successfully", vm_id);
        }
        Some(Err(err)) => {
            println!("✗ VM[{}] failed to start: {}", vm_id, err);
        }
        None => {
            println!("✗ VM[{}] not found", vm_id);
        }
    }
}

fn vm_stop(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let force = cmd.flags.get("force").unwrap_or(&false);

    if args.is_empty() {
        println!("Error: No VM specified");
        println!("Usage: vm stop [OPTIONS] <VM_ID>");
        return;
    }

    for vm_name in args {
        if let Ok(vm_id) = vm_name.parse::<usize>() {
            stop_vm_by_id(vm_id, *force);
        } else {
            println!("Error: Invalid VM ID: {}", vm_name);
        }
    }
}

fn stop_vm_by_id(vm_id: usize, force: bool) {
    match with_vm(vm_id, |vm| {
        let status = vm.vm_status();

        // Validate state transition using helper function
        if let Err(err) = can_stop_vm(status, force) {
            println!("⚠ VM[{}] {}", vm_id, err);
            return Err(err);
        }

        // Print appropriate message based on status
        match status {
            VMStatus::Stopping if force => {
                println!("Force stopping VM[{}]...", vm_id);
            }
            VMStatus::Running => {
                if force {
                    println!("Force stopping VM[{}]...", vm_id);
                } else {
                    println!("Gracefully stopping VM[{}]...", vm_id);
                }
            }
            VMStatus::Loading | VMStatus::Loaded => {
                println!(
                    "⚠ VM[{}] is in {:?} state, stopping anyway...",
                    vm_id, status
                );
            }
            _ => {}
        }

        // Call shutdown
        match vm.shutdown() {
            Ok(_) => Ok(()),
            Err(_err) => {
                // Revert status on failure
                Err("Failed to shutdown VM")
            }
        }
    }) {
        Some(Ok(_)) => {
            println!("✓ VM[{}] stop signal sent successfully", vm_id);
            println!(
                "  Note: vCPU threads will exit gracefully, VM status will transition to Stopped"
            );
        }
        Some(Err(err)) => {
            println!("✗ Failed to stop VM[{}]: {:?}", vm_id, err);
        }
        None => {
            println!("✗ VM[{}] not found", vm_id);
        }
    }
}

/// Restart a VM by stopping it (if running) and then starting it again.(functionality incomplete)
fn vm_restart(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let force = cmd.flags.get("force").unwrap_or(&false);

    if args.is_empty() {
        println!("Error: No VM specified");
        println!("Usage: vm restart [OPTIONS] <VM_ID>");
        return;
    }

    for vm_name in args {
        if let Ok(vm_id) = vm_name.parse::<usize>() {
            restart_vm_by_id(vm_id, *force);
        } else {
            println!("Error: Invalid VM ID: {}", vm_name);
        }
    }
}

fn restart_vm_by_id(vm_id: usize, force: bool) {
    println!("Restarting VM[{}]...", vm_id);

    // Check current status
    let current_status = with_vm(vm_id, |vm| vm.vm_status());
    if current_status.is_none() {
        println!("✗ VM[{}] not found", vm_id);
        return;
    }

    let status = current_status.unwrap();
    match status {
        VMStatus::Stopped | VMStatus::Loaded => {
            // VM is already stopped, just start it
            println!("VM[{}] is already stopped, starting...", vm_id);
            start_vm_by_id(vm_id);
        }
        VMStatus::Suspended | VMStatus::Running => {
            // Stop the VM (this will wake up suspended VCpus automatically)
            println!("Stopping VM[{}]...", vm_id);
            stop_vm_by_id(vm_id, force);

            // Wait for VM to fully stop
            println!("Waiting for VM[{}] to stop completely...", vm_id);
            let max_wait_iterations = 50; // 5 seconds timeout (50 * 100ms)
            let mut iterations = 0;

            loop {
                if let Some(vm_status) = with_vm(vm_id, |vm| vm.vm_status()) {
                    match vm_status {
                        VMStatus::Stopped => {
                            println!("✓ VM[{}] stopped successfully", vm_id);
                            break;
                        }
                        VMStatus::Stopping => {
                            // Still stopping, wait a bit
                            iterations += 1;
                            if iterations >= max_wait_iterations {
                                println!(
                                    "⚠ VM[{}] stop timeout, it may still be shutting down",
                                    vm_id
                                );
                                println!("  Use 'vm status {}' to check status manually", vm_id);
                                return;
                            }
                            // Sleep for 100ms
                            std::os::arceos::modules::axhal::time::busy_wait(
                                core::time::Duration::from_millis(100),
                            );
                        }
                        _ => {
                            println!("⚠ VM[{}] in unexpected state: {:?}", vm_id, vm_status);
                            return;
                        }
                    }
                } else {
                    println!("✗ VM[{}] no longer exists", vm_id);
                    return;
                }
            }

            // Now restart the VM
            println!("Starting VM[{}]...", vm_id);
            start_vm_by_id(vm_id);
        }
        VMStatus::Stopping => {
            if force {
                println!(
                    "⚠ VM[{}] is currently stopping, waiting for shutdown to complete...",
                    vm_id
                );
                // Could implement similar wait logic here if needed
            } else {
                println!("⚠ VM[{}] is currently stopping", vm_id);
                println!(
                    "  Wait for shutdown to complete, then use 'vm start {}'",
                    vm_id
                );
                println!("  Or use --force to wait and then restart");
            }
        }
        VMStatus::Loading => {
            println!("✗ VM[{}] is still loading, cannot restart", vm_id);
        }
    }
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

    let result = with_vm(vm_id, |vm| {
        let status = vm.vm_status();

        // Check if VM can be suspended
        if let Err(err_msg) = can_suspend_vm(status) {
            return Err(err_msg);
        }

        // Set VM status to Suspended
        vm.set_vm_status(VMStatus::Suspended);
        info!("VM[{}] status set to Suspended", vm_id);

        Ok(())
    });

    match result {
        Some(Ok(_)) => {
            println!("✓ VM[{}] suspend signal sent", vm_id);

            // Get VM to check VCpu count
            let vcpu_count = with_vm(vm_id, |vm| vm.vcpu_num()).unwrap_or(0);
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
                if let Some(vm) = crate::vmm::vm_list::get_vm_by_id(vm_id) {
                    let vcpu_states: Vec<_> =
                        vm.vcpu_list().iter().map(|vcpu| vcpu.state()).collect();

                    let blocked_count = vcpu_states
                        .iter()
                        .filter(|s| matches!(s, axvcpu::VCpuState::Blocked))
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
                std::os::arceos::modules::axhal::time::busy_wait(
                    core::time::Duration::from_millis(100),
                );
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
            println!("✗ Failed to suspend VM[{}]: {}", vm_id, err);
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

    let result = with_vm(vm_id, |vm| {
        let status = vm.vm_status();

        // Check if VM can be resumed
        if let Err(err_msg) = can_resume_vm(status) {
            return Err(err_msg);
        }

        // Set VM status back to Running
        vm.set_vm_status(VMStatus::Running);

        // Notify all VCpus to wake up
        vcpus::notify_all_vcpus(vm_id);

        info!("VM[{}] resumed", vm_id);
        Ok(())
    });

    match result {
        Some(Ok(_)) => {
            println!("✓ VM[{}] resumed successfully", vm_id);
        }
        Some(Err(err)) => {
            println!("✗ Failed to resume VM[{}]: {}", vm_id, err);
        }
        None => {
            println!("✗ VM[{}] not found", vm_id);
        }
    }
}

fn vm_delete(cmd: &ParsedCommand) {
    let args = &cmd.positional_args;
    let force = cmd.flags.get("force").unwrap_or(&false);
    let keep_data = cmd.flags.get("keep-data").unwrap_or(&false);

    if args.is_empty() {
        println!("Error: No VM specified");
        println!("Usage: vm delete [OPTIONS] <VM_ID>");
        return;
    }

    let vm_name = &args[0];

    if let Ok(vm_id) = vm_name.parse::<usize>() {
        // Check if VM exists and get its status first
        let vm_status = with_vm(vm_id, |vm| vm.vm_status());

        if vm_status.is_none() {
            println!("✗ VM[{}] not found", vm_id);
            return;
        }

        let status = vm_status.unwrap();

        // Check if VM is running
        match status {
            VMStatus::Running => {
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
            VMStatus::Stopping => {
                if !force {
                    println!("⚠ VM[{}] is currently stopping", vm_id);
                    println!("  Wait for it to stop completely, or use '--force' to force delete");
                    return;
                }
                println!("⚠ Force deleting stopping VM[{}]...", vm_id);
            }
            VMStatus::Stopped => {
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

        delete_vm_by_id(vm_id, *keep_data);
    } else {
        println!("Error: Invalid VM ID: {}", vm_name);
    }
}

fn delete_vm_by_id(vm_id: usize, keep_data: bool) {
    // First check VM status and try to stop it if running
    let vm_status = with_vm(vm_id, |vm| {
        let status = vm.vm_status();

        // If VM is running, suspended, or stopping, send shutdown signal
        match status {
            VMStatus::Running | VMStatus::Suspended | VMStatus::Stopping => {
                println!(
                    "  VM[{}] is {:?}, sending shutdown signal...",
                    vm_id, status
                );
                vm.set_vm_status(VMStatus::Stopping);
                let _ = vm.shutdown();
            }
            VMStatus::Loaded => {
                // Transition from Loaded to Stopped
                vm.set_vm_status(VMStatus::Stopped);
            }
            _ => {}
        }

        use alloc::sync::Arc;
        let count = Arc::strong_count(&vm);
        println!("  [Debug] VM Arc strong_count: {}", count);

        status
    });

    if vm_status.is_none() {
        println!("✗ VM[{}] not found or already removed", vm_id);
        return;
    }

    let status = vm_status.unwrap();

    // Remove VM from global list
    // Note: This drops the reference from the global list, but the VM object
    // will only be fully destroyed when all vCPU threads exit and drop their references
    match crate::vmm::vm_list::remove_vm(vm_id) {
        Some(vm) => {
            println!("✓ VM[{}] removed from VM list", vm_id);

            // Wait for vCPU threads to exit if VM has VCpu tasks
            match status {
                VMStatus::Running
                | VMStatus::Suspended
                | VMStatus::Stopping
                | VMStatus::Stopped => {
                    println!("  Waiting for vCPU threads to exit...");

                    // Debug: Check Arc count before cleanup
                    use alloc::sync::Arc;
                    println!(
                        "  [Debug] VM Arc count before cleanup: {}",
                        Arc::strong_count(&vm)
                    );

                    // Clean up VCpu resources after threads have exited
                    println!("  Cleaning up VCpu resources...");
                    vcpus::cleanup_vm_vcpus(vm_id);

                    // Debug: Check Arc count after final wait
                    println!(
                        "  [Debug] VM Arc count after final wait: {}",
                        Arc::strong_count(&vm)
                    );
                }
                _ => {
                    // VM not running, no vCPU threads to wait for
                    // But still need to clean up VCpu queue entry if it exists
                    vcpus::cleanup_vm_vcpus(vm_id);
                }
            }

            if keep_data {
                println!("✓ VM[{}] deleted (configuration and data preserved)", vm_id);
            } else {
                println!("✓ VM[{}] deleted completely", vm_id);

                // Debug: Check Arc count - should be 1 now (only this variable)
                // TaskExt uses Weak reference, so it doesn't count
                use alloc::sync::Arc;
                let count = Arc::strong_count(&vm);
                println!("  [Debug] VM Arc strong_count: {}", count);

                if count == 1 {
                    println!("  ✓ Perfect! VM will be freed immediately when function returns");
                } else {
                    println!(
                        "  ⚠ Warning: Unexpected Arc count {}, possible reference leak!",
                        count
                    );
                }

                // TODO: Clean up VM-related data files
                // - Remove disk images
                // - Remove configuration files
                // - Remove log files
            }

            // When function returns, the 'vm' variable is dropped
            // Since Arc count is 1, AxVM::drop() is called immediately
            println!("  VM[{}] will be freed now", vm_id);
        }
        None => {
            println!(
                "✗ Failed to remove VM[{}] from list (may have been removed already)",
                vm_id
            );
        }
    }

    // When function returns, the 'vm' Arc is dropped
    // If all vCPU threads have exited (ref_count was 1), AxVM::drop() is called here
    println!("✓ VM[{}] deletion completed", vm_id);
}

fn vm_list_simple() {
    let vms = vm_list::get_vm_list();
    println!("ID    NAME           STATE      VCPU   MEMORY");
    println!("----  -----------    -------    ----   ------");
    for vm in vms {
        let status = vm.vm_status();

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

    let display_vms = vm_list::get_vm_list();

    if display_vms.is_empty() {
        println!("No virtual machines found.");
        return;
    }

    if format == "json" {
        // JSON output
        println!("{{");
        println!("  \"vms\": [");
        for (i, vm) in display_vms.iter().enumerate() {
            let status = vm.vm_status();
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
            let status = vm.vm_status();
            let total_memory: usize = vm.memory_regions().iter().map(|region| region.size()).sum();

            // Get VCpu ID list
            let vcpu_ids: Vec<String> = vm
                .vcpu_list()
                .iter()
                .map(|vcpu| vcpu.id().to_string())
                .collect();
            let vcpu_id_list = vcpu_ids.join(",");

            // Get VCpu state summary
            let mut state_counts = std::collections::BTreeMap::new();
            for vcpu in vm.vcpu_list() {
                let state = match vcpu.state() {
                    axvcpu::VCpuState::Free => "Free",
                    axvcpu::VCpuState::Running => "Run",
                    axvcpu::VCpuState::Blocked => "Blk",
                    axvcpu::VCpuState::Invalid => "Inv",
                    axvcpu::VCpuState::Created => "Cre",
                    axvcpu::VCpuState::Ready => "Rdy",
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
    let show_config = cmd.flags.get("config").unwrap_or(&false);
    let show_stats = cmd.flags.get("stats").unwrap_or(&false);
    let show_full = cmd.flags.get("full").unwrap_or(&false);

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
        if *show_full {
            show_vm_full_details(vm_id);
        } else {
            show_vm_basic_details(vm_id, *show_config, *show_stats);
        }
    } else {
        println!("Error: Invalid VM ID: {}", vm_name);
    }
}

/// Show basic VM information (default view)
fn show_vm_basic_details(vm_id: usize, show_config: bool, show_stats: bool) {
    match with_vm(vm_id, |vm| {
        let status = vm.vm_status();

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
            VMStatus::Suspended => {
                println!();
                println!("  ℹ VM is paused. Use 'vm resume {}' to continue.", vm_id);
            }
            VMStatus::Stopped => {
                println!();
                println!("  ℹ VM is stopped. Use 'vm delete {}' to clean up.", vm_id);
            }
            VMStatus::Loaded => {
                println!();
                println!("  ℹ VM is ready. Use 'vm start {}' to boot.", vm_id);
            }
            _ => {}
        }

        // VCPU Summary
        println!();
        println!("VCPU Summary:");
        let mut state_counts = std::collections::BTreeMap::new();
        for vcpu in vm.vcpu_list() {
            let state = match vcpu.state() {
                axvcpu::VCpuState::Free => "Free",
                axvcpu::VCpuState::Running => "Running",
                axvcpu::VCpuState::Blocked => "Blocked",
                axvcpu::VCpuState::Invalid => "Invalid",
                axvcpu::VCpuState::Created => "Created",
                axvcpu::VCpuState::Ready => "Ready",
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
                println!("  Interrupt Mode: {:?}", cfg.interrupt_mode());
                if let Some(dtb_addr) = cfg.image_config().dtb_load_gpa {
                    println!("  DTB Address:    {:#x}", dtb_addr.as_usize());
                }
            });
        }

        // Device Summary
        if show_stats {
            println!();
            println!("Device Summary:");
            println!(
                "  MMIO Devices:   {}",
                vm.get_devices().iter_mmio_dev().count()
            );
            println!(
                "  SysReg Devices: {}",
                vm.get_devices().iter_sys_reg_dev().count()
            );
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
    match with_vm(vm_id, |vm| {
        let status = vm.vm_status();

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
        println!("  EPT Root:  {:#x}", vm.ept_root().as_usize());

        // Add state-specific information
        match status {
            VMStatus::Suspended => {
                println!(
                    "    ℹ VM is paused, VCpu tasks are waiting. Use 'vm resume {}' to continue.",
                    vm_id
                );
            }
            VMStatus::Stopping => {
                println!("    ℹ VM is shutting down, VCpu tasks are exiting.");
            }
            VMStatus::Stopped => {
                println!(
                    "    ℹ VM is stopped, all VCpu tasks have exited. Use 'vm delete {}' to clean up.",
                    vm_id
                );
            }
            VMStatus::Loaded => {
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
        for vcpu in vm.vcpu_list() {
            let state = match vcpu.state() {
                axvcpu::VCpuState::Free => "Free",
                axvcpu::VCpuState::Running => "Running",
                axvcpu::VCpuState::Blocked => "Blocked",
                axvcpu::VCpuState::Invalid => "Invalid",
                axvcpu::VCpuState::Created => "Created",
                axvcpu::VCpuState::Ready => "Ready",
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

        for vcpu in vm.vcpu_list() {
            let vcpu_state = match vcpu.state() {
                axvcpu::VCpuState::Free => "Free",
                axvcpu::VCpuState::Running => "Running",
                axvcpu::VCpuState::Blocked => "Blocked",
                axvcpu::VCpuState::Invalid => "Invalid",
                axvcpu::VCpuState::Created => "Created",
                axvcpu::VCpuState::Ready => "Ready",
            };

            if let Some(phys_cpu_set) = vcpu.phys_cpu_set() {
                println!(
                    "  VCPU {}: {} (Affinity: {:#x})",
                    vcpu.id(),
                    vcpu_state,
                    phys_cpu_set
                );
            } else {
                println!("  VCPU {}: {} (No affinity)", vcpu.id(), vcpu_state);
            }
        }

        // Add note for Suspended VMs
        if status == VMStatus::Suspended {
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
            println!("  Interrupt Mode: {:?}", cfg.interrupt_mode());

            if let Some(dtb_addr) = cfg.image_config().dtb_load_gpa {
                println!("  DTB Address:    {:#x}", dtb_addr.as_usize());
            }

            // Show kernel info
            println!(
                "  Kernel GPA:     {:#x}",
                cfg.image_config().kernel_load_gpa.as_usize()
            );

            // Show passthrough devices
            if !cfg.pass_through_devices().is_empty() {
                println!();
                println!(
                    "  Passthrough Devices: ({} device(s))",
                    cfg.pass_through_devices().len()
                );
                for device in cfg.pass_through_devices() {
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

            // Show passthrough addresses
            if !cfg.pass_through_addresses().is_empty() {
                println!();
                println!(
                    "  Passthrough Memory Regions: ({} region(s))",
                    cfg.pass_through_addresses().len()
                );
                for pt_addr in cfg.pass_through_addresses() {
                    println!(
                        "    - GPA[{:#x}~{:#x}] ({})",
                        pt_addr.base_gpa,
                        pt_addr.base_gpa + pt_addr.length,
                        format_memory_size(pt_addr.length)
                    );
                }
            }

            // Show passthrough SPIs (ARM specific)
            #[cfg(target_arch = "aarch64")]
            {
                let spis = cfg.pass_through_spis();
                if !spis.is_empty() {
                    println!();
                    println!("  Passthrough SPIs: {:?}", spis);
                }
            }

            // Show emulated devices
            if !cfg.emu_devices().is_empty() {
                println!();
                println!(
                    "  Emulated Devices: ({} device(s))",
                    cfg.emu_devices().len()
                );
                for (idx, device) in cfg.emu_devices().iter().enumerate() {
                    println!("    {}. {:?}", idx + 1, device);
                }
            }
        });

        // Devices
        println!();
        let mmio_dev_count = vm.get_devices().iter_mmio_dev().count();
        let sysreg_dev_count = vm.get_devices().iter_sys_reg_dev().count();
        println!("Devices:");
        println!("  MMIO Devices:   {}", mmio_dev_count);
        println!("  SysReg Devices: {}", sysreg_dev_count);

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

    let restart_cmd = CommandNode::new("Restart a virtual machine")
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
        .add_subcommand("restart", restart_cmd)
        .add_subcommand("delete", delete_cmd)
        .add_subcommand("list", list_cmd)
        .add_subcommand("show", show_cmd);

    tree.insert("vm".to_string(), vm_node);
}
