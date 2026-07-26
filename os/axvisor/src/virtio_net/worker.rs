//! Guest delivery worker lifecycle for the virtio-net device.
//!
//! One worker per VM drains the backend's inbound queue (deterministic echo
//! replies for the smoke path, or frames the switch/uplink pushed onto this
//! port's ingress for the raw-uplink path) and writes each frame into the guest
//! RX virtqueue via `receive_frame`, pulsing the IRQ when the device asks for
//! notification. The worker is an event-driven host task (it blocks on the
//! backend wake queue, it does not busy-poll) and cooperates with shutdown
//! through a cancel flag.
//!
//! The worker never touches the host `TxQueue`/`RxQueue`: host RX reclaim, host
//! TX and L2 switching belong to the single uplink worker in
//! [`super::raw_uplink`]. This is what lets several VMs share one uplink without
//! racing the host RX queue (design §2.1, §6.2).
//!
//! Workers are tracked in a process-global registry keyed by VM id. On VM
//! stop/reset/remove the orchestrator cancels and joins the worker before the
//! device graph is torn down; after a reset (which re-prepares the VM at a new
//! generation) a fresh worker is started for the new device, so a stale worker
//! can never inject into a newer generation.

use alloc::collections::BTreeMap;
use alloc::sync::Arc;

use axdevice_base::IrqLine;
use axvirtio_net::{RxOutcome, VirtioMmioNetDevice};
use axvm::{
    AxVMRef, WorkerTask, get_vm_list, host_cpu_count, spawn_worker_task,
    spawn_worker_task_with_affinity,
};

use super::adapter::VirtioNetDeviceAdapter;
use super::backend::AxvisorNetworkBackend;

/// Worker stack size (the delivery path and `receive_frame` copy are shallow).
const WORKER_STACK_SIZE: usize = 0x2_0000;

/// Handle used to cancel and join a running worker.
struct WorkerHandle {
    backend: AxvisorNetworkBackend,
    cancel: Arc<core::sync::atomic::AtomicBool>,
    task: WorkerTask,
}

impl WorkerHandle {
    /// Requests cancellation, wakes a blocked worker, and waits for exit.
    fn cancel_and_join(self) {
        self.cancel
            .store(true, core::sync::atomic::Ordering::Release);
        self.backend.wake_worker();
        let _ = self.task.join();
    }
}

static WORKERS: spin::Mutex<BTreeMap<usize, WorkerHandle>> = spin::Mutex::new(BTreeMap::new());

/// Starts the guest delivery worker for `vm`'s virtio-net device, if it has one.
///
/// Looks up the device by downcasting from the VM's device registry. If a worker
/// is already registered for this VM (e.g. leftover from a failed reset), it is
/// cancelled and joined first so only one generation is ever active.
pub fn start_for_vm(vm: &AxVMRef) {
    let vm_id = vm.id();
    let Some((device, irq, backend)) = find_virtio_net_endpoint(vm) else {
        info!("VM[{vm_id}] has no virtio-net device; no delivery worker started");
        return;
    };

    // Replace any stale worker for this VM before starting a fresh one.
    let stale = WORKERS.lock().remove(&vm_id);
    if let Some(handle) = stale {
        warn!("VM[{vm_id}] starting a new delivery worker; joining a stale one first");
        handle.cancel_and_join();
    }

    let cancel = Arc::new(core::sync::atomic::AtomicBool::new(false));
    let name = alloc::format!("VM[{vm_id}]-virtio-net-rx");
    let handle_backend = backend.clone();
    let handle_cancel = cancel.clone();
    let worker_cpu = select_worker_cpu(vm);
    let task = if let Some(cpu_id) = worker_cpu {
        info!("VM[{vm_id}] virtio-net worker assigned to host CPU {cpu_id}");
        spawn_worker_task_with_affinity(name, WORKER_STACK_SIZE, 1usize << cpu_id, move || {
            run_delivery_loop(device, irq, backend, cancel);
        })
    } else {
        warn!("VM[{vm_id}] has no host CPU reserved for its virtio-net worker");
        spawn_worker_task(name, WORKER_STACK_SIZE, move || {
            run_delivery_loop(device, irq, backend, cancel);
        })
    };
    WORKERS.lock().insert(
        vm_id,
        WorkerHandle {
            backend: handle_backend,
            cancel: handle_cancel,
            task,
        },
    );
    info!("VM[{vm_id}] virtio-net delivery worker started");
}

fn select_worker_cpu(vm: &AxVMRef) -> Option<usize> {
    let vcpu_mask = get_vm_list()
        .iter()
        .flat_map(|vm| vm.get_vcpu_affinities_pcpu_ids())
        .filter_map(|(_, affinity, _)| affinity)
        .fold(0usize, |used, affinity| used | affinity);
    let current_vm_mask = vm
        .get_vcpu_affinities_pcpu_ids()
        .into_iter()
        .filter_map(|(_, affinity, _)| affinity)
        .fold(0usize, |used, affinity| used | affinity);
    select_worker_cpu_from_masks(host_cpu_count(), vcpu_mask, current_vm_mask)
}

fn select_worker_cpu_from_masks(
    cpu_count: usize,
    all_vcpu_mask: usize,
    current_vm_mask: usize,
) -> Option<usize> {
    // The uplink worker reserves the highest host CPU. Prefer the next one for
    // all guest delivery workers so static VMs created later can use low CPUs
    // without colliding with workers that were already spawned.
    if let Some(cpu_id) = cpu_count.checked_sub(2)
        && all_vcpu_mask & (1usize << cpu_id) == 0
    {
        return Some(cpu_id);
    }
    (0..cpu_count).find(|cpu_id| current_vm_mask & (1usize << cpu_id) == 0)
}

/// Cancels and joins the delivery worker for `vm_id`, if one is running.
pub fn stop_for_vm(vm_id: usize) {
    let Some(handle) = WORKERS.lock().remove(&vm_id) else {
        return;
    };
    handle.cancel_and_join();
    info!("VM[{vm_id}] virtio-net delivery worker stopped");
}

/// Finds the virtio-net adapter in `vm`'s device registry and returns the
/// runtime endpoint the worker needs (device model, IRQ line, backend).
fn find_virtio_net_endpoint(
    vm: &AxVMRef,
) -> Option<(
    Arc<VirtioMmioNetDevice<AxvisorNetworkBackend, axvm::AxvmGuestMemoryAccessor>>,
    IrqLine,
    AxvisorNetworkBackend,
)> {
    let devices = vm.get_devices().ok()?;
    for device in devices.devices() {
        if let Some(adapter) = device.as_any().downcast_ref::<VirtioNetDeviceAdapter>() {
            return Some((
                adapter.device().clone(),
                adapter.irq().clone(),
                adapter.backend().clone(),
            ));
        }
    }
    None
}

/// The guest delivery worker main loop.
fn run_delivery_loop(
    device: Arc<VirtioMmioNetDevice<AxvisorNetworkBackend, axvm::AxvmGuestMemoryAccessor>>,
    irq: IrqLine,
    backend: AxvisorNetworkBackend,
    cancel: Arc<core::sync::atomic::AtomicBool>,
) {
    loop {
        if cancel.load(core::sync::atomic::Ordering::Acquire) {
            break;
        }
        backend.wake_queue().wait_until(|| {
            cancel.load(core::sync::atomic::Ordering::Acquire) || backend.rx_ready()
        });
        if cancel.load(core::sync::atomic::Ordering::Acquire) {
            break;
        }
        backend.clear_rx_ready();
        drain_and_deliver(&device, &irq, &backend);
    }
}

/// Drains all currently-buffered inbound frames and delivers each to the guest.
fn drain_and_deliver(
    device: &Arc<VirtioMmioNetDevice<AxvisorNetworkBackend, axvm::AxvmGuestMemoryAccessor>>,
    irq: &IrqLine,
    backend: &AxvisorNetworkBackend,
) {
    while let Some(frame) = backend.drain_rx() {
        if let Err(frame) = deliver_one(device, irq, frame) {
            backend.requeue_rx(frame);
            return;
        }
    }
}

/// Delivers one frame, returning it to the caller when the guest had no RX
/// buffer so it can be requeued and retried on the next guest RX kick.
fn deliver_one(
    device: &Arc<VirtioMmioNetDevice<AxvisorNetworkBackend, axvm::AxvmGuestMemoryAccessor>>,
    irq: &IrqLine,
    frame: alloc::vec::Vec<u8>,
) -> Result<(), alloc::vec::Vec<u8>> {
    match device.receive_frame(&frame) {
        Ok(RxOutcome::Delivered { frame_len, notify }) => {
            if notify && let Err(error) = irq.pulse() {
                warn!("virtio-net RX IRQ pulse failed: {error:?}");
            }
            debug!("virtio-net delivered RX frame ({frame_len} bytes)");
            Ok(())
        }
        Ok(RxOutcome::NoGuestBuffer) => Err(frame),
        Err(error) => {
            warn!("virtio-net receive_frame failed, dropping frame: {error:?}");
            Ok(())
        }
    }
}
