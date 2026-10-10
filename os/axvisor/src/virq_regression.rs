//! Bounded, opt-in parked-vCPU delivery workload. All scheduling and IRQ state
//! remain owned by AxVM and its interrupt controller; this module only drives
//! the public narrow per-run inspection ports: one guest-memory window for the
//! mailbox and one run-bound virtual interrupt endpoint.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use axvm::{GuestMemoryPort, GuestPhysAddr, RunId, VmHandle, VmStatus};

const VM_ID: usize = 2;
const TARGET_VCPU: usize = 1;
const TARGET_VECTOR: u32 = 48;
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

fn mailbox(memory: &GuestMemoryPort, address: GuestPhysAddr) -> Result<[u32; 5]> {
    let mut bytes = [0u8; 20];
    memory.read_bytes(address, &mut bytes)?;
    Ok(core::array::from_fn(|index| {
        let offset = index * 4;
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }))
}

fn wait_for(
    vm: &VmHandle,
    memory: &GuestMemoryPort,
    address: GuestPhysAddr,
    deadline: Instant,
    ready: impl Fn(&[u32; 5]) -> bool,
) -> Result<[u32; 5]> {
    loop {
        ensure!(
            vm.snapshot().state == VmStatus::Running,
            "VM stopped before the workload completed"
        );
        let state = mailbox(memory, address)?;
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
    let vm = crate::manager::manager()
        .get(VM_ID)
        .context("regression VM 2 was not initialized")?;
    ensure!(
        vm.snapshot().cpu.vcpu_num == 2,
        "regression requires two vCPUs"
    );

    // Both narrow ports are bound to the exact run observed in the snapshot, so
    // they close when the run retires or its translation revision changes.
    let run: RunId = vm
        .snapshot()
        .run
        .context("regression VM has no active run; start VM 2 before the workload")?;
    let memory = vm
        .guest_memory(run)
        .context("open the regression mailbox window")?;
    let interrupt = vm
        .interrupt_port(run, TARGET_VCPU, TARGET_VECTOR)
        .context("bind the run-owned virtual interrupt endpoint")?;

    crate::guest_console::attach(VM_ID)?;
    crate::guest_console::activate(VM_ID);
    let deadline = Instant::now() + Duration::from_secs(30);
    let initial = wait_for(&vm, &memory, address, deadline, |state| {
        state[0] == 1 && state[1] == 1 && state[3] != 0
    })?;
    ensure!(initial[2] == 0, "guest received an unrequested interrupt");
    let idle_parks = initial[3];
    for count in 1..=SAMPLES {
        // One outstanding edge at a time: the controller pending bits coalesce
        // edges; neither this test nor AxVM promises a counted interrupt FIFO.
        interrupt
            .pulse()
            .context("pulse the regression interrupt")?;
        let state = wait_for(&vm, &memory, address, deadline, |state| state[2] >= count)?;
        ensure!(state[2] == count, "duplicate interrupt: {state:?}");
        ensure!(
            state[3] == idle_parks,
            "unrelated vCPU left suspend: {state:?}"
        );
    }
    info!("VIRQ_INJECT_COMPLETE vm=2 vcpu=1 vector=48 samples=300 errors=0");
    info!("E1_COUNTERS idle_vcpu_returns=0 acknowledgements=300");
    memory.write_bytes(address + 16, &1u32.to_le_bytes())?;
    Ok(())
}
