//! Bounded, opt-in parked-vCPU delivery workload. All scheduling and IRQ state
//! remain owned by AxVM and its VGIC; this module only drives the public API.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use axvm::{AxVMRef, CpuMask, GuestPhysAddr};

const VM_ID: usize = 2;
const SAMPLES: u32 = 300;
const POLL_INTERVAL: Duration = Duration::from_millis(1);

pub(crate) fn start() {
    std::thread::Builder::new()
        .name("virq-regression".into())
        .spawn(|| {
            if let Err(error) = run() {
                // Host logs are buffered while attached to a running guest.
                // A failed guest may never shut down, so publish the failure
                // directly to the host output worker for the bounded runner.
                let message = format!("\r\nVIRQ_TEST_FAILED {error:#}\r\n");
                crate::guest_console::submit_host_bytes(message.as_bytes());
            }
        })
        .expect("vIRQ regression worker creation");
}

fn mailbox(vm: &AxVMRef, address: GuestPhysAddr) -> Result<[u32; 5]> {
    let mut bytes = [0u8; 20];
    vm.read_from_guest(address, &mut bytes)?;
    Ok(core::array::from_fn(|index| {
        let offset = index * 4;
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }))
}

fn wait_for(
    vm: &AxVMRef,
    address: GuestPhysAddr,
    deadline: Instant,
    ready: impl Fn(&[u32; 5]) -> bool,
) -> Result<[u32; 5]> {
    loop {
        ensure!(vm.running(), "VM stopped before the workload completed");
        let state = mailbox(vm, address)?;
        if ready(&state) {
            return Ok(state);
        }
        if Instant::now() >= deadline {
            bail!("guest acknowledgement timed out: {state:?}");
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn run() -> Result<()> {
    let address = usize::from_str_radix(
        option_env!("AXVISOR_VIRQ_MAILBOX")
            .context("prepare the Zephyr guest and set AXVISOR_VIRQ_MAILBOX")?,
        16,
    )?;
    let address = GuestPhysAddr::from(address);
    let vm = axvm::get_vm_by_id(VM_ID).context("regression VM 2 was not initialized")?;
    ensure!(vm.vcpu_num() == 2, "regression requires two vCPUs");
    crate::guest_console::attach(VM_ID)?;
    crate::guest_console::activate(VM_ID);
    let deadline = Instant::now() + Duration::from_secs(30);
    let initial = wait_for(&vm, address, deadline, |state| {
        state[0] == 1 && state[1] == 1 && state[3] != 0
    })?;
    ensure!(initial[2] == 0, "guest received an unrequested interrupt");
    let idle_parks = initial[3];
    let target = CpuMask::one_shot(1);
    for count in 1..=SAMPLES {
        // One outstanding edge at a time: GIC pending bits coalesce edges;
        // neither this test nor AxVM promises a counted interrupt FIFO.
        vm.inject_interrupt_to_vcpu(target, 48)?;
        let state = wait_for(&vm, address, deadline, |state| state[2] >= count)?;
        ensure!(state[2] == count, "duplicate interrupt: {state:?}");
        ensure!(
            state[3] == idle_parks,
            "unrelated vCPU left suspend: {state:?}"
        );
    }
    info!("VIRQ_INJECT_COMPLETE vm=2 vcpu=1 vector=48 samples=300 errors=0");
    info!("E1_COUNTERS idle_vcpu_returns=0 acknowledgements=300");
    vm.write_to_guest(address + 16, &1u32.to_le_bytes())?;
    Ok(())
}
