//! Single-owner host uplink runtime for the shared virtio-net switch.
//!
//! One [`HostUplinkRuntime`] exists per host NIC (keyed by its MAC in
//! [`UPLINKS`]). It is the *only* place that touches the host `TxQueue` /
//! `RxQueue` and the host IRQ: reclaiming TX completions, submitting host TX,
//! reclaiming host RX and handing frames to the [`VirtualSwitch`] for
//! distribution. Per-VM guest delivery workers never reach the host queues, so
//! two VMs sharing one uplink cannot race a frame away from each other
//! (design §2.1, §3.2).
//!
//! The runtime owns the switch, the concrete port table the uplink worker
//! round-robins for egress, the epoch-based [`UplinkWorkSignal`] producers wake,
//! and the single uplink worker task. It lives for the whole AxVisor lifetime —
//! the last port detaching leaves the worker asleep rather than releasing the
//! NIC, so reset does not rebuild the DMA rings or re-arm the EVENT_IDX startup
//! IRQ window (design §8.4).

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

use ax_driver::net::{PlatformNetDevice, take_rd_net_device};
use ax_hal::irq::IrqReturn;
use axvirtio_switch::{
    EgressOutcome, SwitchError, SwitchPortId, SwitchPortRegistration, VirtualSwitch,
};
use axvm::{
    WorkerTask, WorkerWaitQueue, get_vm_list, host_cpu_count, spawn_worker_task,
    spawn_worker_task_with_affinity, yield_now,
};
use rd_net::{NetError, RxQueue, TxQueue};

use super::backend::PortEndpoint;

/// Per round, reclaim at most this many host TX completions / RX frames.
const HOST_RX_BUDGET: usize = 64;
/// Per round, submit at most this many frames to host TX across all ports.
const HOST_TX_BUDGET: usize = 64;
/// Per round, drain at most this many frames from one port's egress.
const PORT_TX_QUANTUM: usize = 8;

/// Uplink worker stack size (shallow: queue ops + frame copy + switch call).
const UPLINK_WORKER_STACK_SIZE: usize = 0x2_0000;

struct UplinkQueues {
    tx: TxQueue,
    rx: RxQueue,
}

/// Shared state the uplink worker and the runtime hand to port attachments.
///
/// Kept in its own `Arc` so the worker closure owns `Arc<UplinkWorkerCore>`
/// rather than `Arc<HostUplinkRuntime>`, avoiding a strong-reference cycle with
/// the `WorkerTask` the runtime stores (design §3.2).
struct UplinkWorkerCore {
    host_mac: [u8; 6],
    queues: spin::Mutex<UplinkQueues>,
    switch: Arc<VirtualSwitch>,
    signal: Arc<UplinkWorkSignal>,
    ports: spin::Mutex<BTreeMap<SwitchPortId, Arc<PortEndpoint>>>,
    tx_rotation: spin::Mutex<usize>,
}

/// Edge-preserving wake signal shared by all producers (host IRQ, guest TX,
/// port attach) and the single uplink worker (design §6.1).
pub struct UplinkWorkSignal {
    epoch: core::sync::atomic::AtomicU64,
    wake: WorkerWaitQueue,
}

impl UplinkWorkSignal {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            epoch: core::sync::atomic::AtomicU64::new(0),
            wake: WorkerWaitQueue::new(),
        })
    }

    /// Producer side: bump the epoch then wake. A worker that already observed
    /// the previous epoch re-reads this on the way to sleep and notices the bump.
    pub fn signal(&self) {
        self.epoch
            .fetch_add(1, core::sync::atomic::Ordering::Release);
        self.wake.wake_one();
    }

    /// Worker side: block until the epoch has advanced past `observed`, then
    /// update `observed` to the new value and return.
    fn wait_observed(&self, observed: &mut u64) {
        let target = *observed;
        let wake = &self.wake;
        let epoch = &self.epoch;
        // Predicate is read-only so the closure can be `Fn` (wait_until re-checks
        // it after every wake, so a signal that lands between the last drain and
        // the sleep is not lost).
        wake.wait_until(|| epoch.load(core::sync::atomic::Ordering::Acquire) != target);
        *observed = epoch.load(core::sync::atomic::Ordering::Acquire);
    }
}

/// Persistent host uplink: host NIC queues + IRQ + switch + uplink worker.
pub struct HostUplinkRuntime {
    core: Arc<UplinkWorkerCore>,
    signal: Arc<UplinkWorkSignal>,
    _task: WorkerTask,
    _irq_registration: ax_runtime::irq::Registration,
}

static UPLINKS: spin::Mutex<BTreeMap<[u8; 6], Arc<HostUplinkRuntime>>> =
    spin::Mutex::new(BTreeMap::new());

impl HostUplinkRuntime {
    /// Returns the persistent uplink for `mac`, claiming the host NIC on first
    /// use. The same MAC always resolves to the same runtime, so several VMs
    /// share one host queue owner (design §3.2).
    pub fn claim_or_get(mac: [u8; 6]) -> Result<Arc<Self>, String> {
        if let Some(uplink) = UPLINKS.lock().get(&mac).cloned() {
            return Ok(uplink);
        }
        let uplink = Arc::new(Self::claim(mac)?);
        let mut uplinks = UPLINKS.lock();
        if let Some(existing) = uplinks.get(&mac) {
            // Another caller raced and won; discard ours, reuse theirs. Ours
            // drops and releases the NIC queues it just created only if the
            // existing one had not already claimed them — but claim() took the
            // device out of the rdrive registry, so a race means the other
            // path's take_rd_net_device would already have failed. The mutex
            // serializes claim_or_get, so this branch is defensive only.
            return Ok(existing.clone());
        }
        uplinks.insert(mac, uplink.clone());
        Ok(uplink)
    }

    fn claim(mac: [u8; 6]) -> Result<Self, String> {
        let mut matches = Vec::new();
        for device in rdrive::get_list::<PlatformNetDevice>() {
            let device_mac = device
                .lock()
                .map_err(|_| format!("failed to inspect host NIC for MAC {}", fmt_mac(mac)))?
                .mac_address();
            if device_mac == Some(mac) {
                matches.push(device);
            }
        }
        if matches.len() != 1 {
            return Err(format!(
                "host uplink MAC {} matched {} devices; expected exactly one",
                fmt_mac(mac),
                matches.len()
            ));
        }

        let (mut net, name, binding_irq) = take_rd_net_device(matches.remove(0))
            .map_err(|error| format!("failed to claim host uplink {}: {error}", fmt_mac(mac)))?;
        let mut irq_handler = net
            .take_irq_handler()
            .ok_or_else(|| format!("host uplink {name} has no owned IRQ handler"))?;
        let binding_irq =
            binding_irq.ok_or_else(|| format!("host uplink {name} has no IRQ binding"))?;
        let irq = ax_runtime::irq::resolve_binding_irq(binding_irq)
            .map_err(|error| format!("failed to resolve {name} IRQ: {error:?}"))?;
        let signal = UplinkWorkSignal::new();
        let irq_signal = signal.clone();
        let registration = ax_runtime::irq::Registration::register_shared(
            format!("{name}-axvisor-uplink"),
            irq,
            move |_context| {
                // IRQ top half: ACK only, then wake the uplink worker. No port
                // table / host queue / guest device locks, no frame copy
                // (design §6.1, §7.1).
                let event = irq_handler.handle_irq();
                let handled = event.tx_queue.iter().next().is_some()
                    || event.rx_queue.iter().next().is_some();
                if handled {
                    irq_signal.signal();
                    IrqReturn::Wake
                } else {
                    IrqReturn::Unhandled
                }
            },
        )
        .map_err(|error| format!("failed to register {name} IRQ: {error:?}"))?;
        // Register the handler before RX prefill so an EVENT_IDX transport can
        // interrupt as soon as the first buffers are published.
        let tx = net
            .create_tx_queue()
            .map_err(|error| format!("failed to create {name} TX queue: {error}"))?;
        let rx = net
            .create_rx_queue()
            .map_err(|error| format!("failed to create {name} RX queue: {error}"))?;
        net.enable_irq();
        info!("claimed host network uplink {name} at MAC {}", fmt_mac(mac));

        let core = Arc::new(UplinkWorkerCore {
            host_mac: mac,
            queues: spin::Mutex::new(UplinkQueues { tx, rx }),
            switch: VirtualSwitch::new(),
            signal: signal.clone(),
            ports: spin::Mutex::new(BTreeMap::new()),
            tx_rotation: spin::Mutex::new(0),
        });

        let worker_core = core.clone();
        let worker_name = format!("{name}-axvisor-uplink-worker");
        // The AxVisor task scheduler is non-preemptive: a worker sharing a CPU
        // with a running vCPU is starved until the vCPU traps out. Pin the
        // single uplink worker to a host CPU that runs no vCPU so host RX/TX is
        // serviced promptly (design §2.1; the per-VM delivery worker does the
        // same via select_worker_cpu).
        let uplink_cpu = select_uplink_cpu();
        let _task = match uplink_cpu {
            Some(cpu_id) => spawn_worker_task_with_affinity(
                worker_name,
                UPLINK_WORKER_STACK_SIZE,
                1usize << cpu_id,
                move || run_uplink_worker(worker_core),
            ),
            None => spawn_worker_task(worker_name, UPLINK_WORKER_STACK_SIZE, move || {
                run_uplink_worker(worker_core)
            }),
        };

        Ok(Self {
            core,
            signal,
            _task,
            _irq_registration: registration,
        })
    }

    /// Creates a port for `(id, guest_mac)` on this uplink's switch and returns
    /// the concrete endpoint (shared with the device backend and the guest
    /// delivery worker) plus the RAII [`PortAttachment`] that removes it on
    /// teardown.
    ///
    /// Errors: the switch rejects a duplicate id or guest MAC; the host MAC
    /// must not equal the guest MAC (defensive — the rdrive lookup guarantees
    /// distinctness, but the check stays explicit per design §4).
    pub fn attach_port(
        self: &Arc<Self>,
        id: SwitchPortId,
        guest_mac: [u8; 6],
    ) -> Result<(Arc<PortEndpoint>, PortAttachment), String> {
        if guest_mac == self.core.host_mac {
            return Err(format!(
                "guest MAC {} must not equal host uplink MAC",
                fmt_mac(guest_mac)
            ));
        }
        let endpoint = PortEndpoint::new(id, guest_mac, self.signal.clone());
        let registration = self
            .core
            .switch
            .register_owned(endpoint.clone())
            .map_err(|error| switch_error_string(&error))?;
        self.core.ports.lock().insert(id, endpoint.clone());
        endpoint.activate();
        info!(
            "VM[{}] virtio-net port {:?} attached to uplink {}",
            id.vm_id,
            guest_mac,
            fmt_mac(self.core.host_mac)
        );
        let attachment = PortAttachment {
            core: Arc::downgrade(&self.core),
            registration: Some(registration),
        };
        Ok((endpoint, attachment))
    }
}

/// RAII handle that detaches a port when the adapter is torn down.
///
/// Drop order matches design §8.2: deactivate (reject new egress/ingress) ->
/// remove from the uplink worker's concrete port table -> release the switch
/// registration (remove from `by_id`/`by_mac`). The guest delivery worker is
/// cancelled and joined separately by `stop_for_vm` before the adapter drops.
pub struct PortAttachment {
    core: Weak<UplinkWorkerCore>,
    registration: Option<SwitchPortRegistration>,
}

impl Drop for PortAttachment {
    fn drop(&mut self) {
        let Some(core) = self.core.upgrade() else {
            // Runtime already gone (AxVisor shutdown); nothing to clean up.
            return;
        };
        if let Some(reg) = self.registration.take() {
            let id = reg.id();
            // Deactivate first so the uplink worker and switch stop using the
            // port before it leaves the tables.
            if let Some(endpoint) = core.ports.lock().get(&id) {
                endpoint.deactivate();
            }
            core.ports.lock().remove(&id);
            reg.release();
            info!("VM[{}] virtio-net port detached from uplink", id.vm_id);
        }
    }
}

/// The uplink worker main loop: drain work while there is any, sleep on the
/// epoch signal otherwise. Runs for the AxVisor lifetime (no cancellation).
fn run_uplink_worker(core: Arc<UplinkWorkerCore>) {
    let mut observed_epoch = 0u64;
    loop {
        let did_work = progress_tx(&core) | progress_rx(&core);
        if did_work {
            // The scheduler is non-preemptive: yield between bursts so a guest
            // delivery worker sharing this CPU can drain ingress we just filled.
            yield_now();
            continue;
        }
        core.signal.wait_observed(&mut observed_epoch);
    }
}

/// Picks a host CPU that runs no guest vCPU, for the uplink worker.
///
/// VMs fill low-numbered pCPUs first, so searching from the top finds a
/// host-only CPU even when vCPU affinities are not yet populated at claim time
/// (then the mask is empty and the highest CPU is chosen). Returns `None` only
/// in an overcommitted layout where every CPU runs a vCPU.
fn select_uplink_cpu() -> Option<usize> {
    let vcpu_mask = get_vm_list()
        .iter()
        .flat_map(|vm| vm.get_vcpu_affinities_pcpu_ids())
        .filter_map(|(_vcpu, affinity, _pcpu)| affinity)
        .fold(0usize, |used, affinity| used | affinity);
    (0..host_cpu_count())
        .rev()
        .find(|cpu_id| vcpu_mask & (1usize << cpu_id) == 0)
}

/// Reclaims host TX completions, then round-robin drains each port's egress,
/// classifies via the switch and submits host TX for uplink-bound frames.
///
/// Returns whether any frame was submitted or any TX completion reclaimed (so
/// the worker knows it made progress). Resumes the round-robin from where the
/// previous call stopped rather than always starting at the smallest PortId
/// (design §5.3).
fn progress_tx(core: &Arc<UplinkWorkerCore>) -> bool {
    let mut progressed = false;
    match core.queues.lock().tx.reclaim_completed(HOST_TX_BUDGET) {
        Ok(reclaimed) => progressed |= reclaimed > 0,
        Err(error) => warn!(
            "host uplink {} TX reclaim failed: {error}",
            fmt_mac(core.host_mac)
        ),
    }

    let port_ids = core.switch.active_port_ids();
    if port_ids.is_empty() {
        return progressed;
    }
    let start = {
        let mut rotation = core.tx_rotation.lock();
        let s = *rotation % port_ids.len();
        *rotation = (*rotation + 1) % port_ids.len();
        s
    };

    let mut submitted = 0usize;
    for offset in 0..port_ids.len() {
        if submitted >= HOST_TX_BUDGET {
            break;
        }
        let id = port_ids[(start + offset) % port_ids.len()];
        let Some(endpoint) = core.ports.lock().get(&id).cloned() else {
            continue;
        };
        for _ in 0..PORT_TX_QUANTUM {
            if submitted >= HOST_TX_BUDGET {
                break;
            }
            let Some(frame) = endpoint.pop_egress() else {
                break; // this port is drained; next port
            };
            let outcome = core.switch.switch_from_port(id, &frame);
            if let EgressOutcome::Forwarded { uplink } = outcome {
                if uplink {
                    match submit_host_tx(core, &frame) {
                        HostTxResult::Submitted => submitted += 1,
                        HostTxResult::Retried => {
                            // Ring full: put the frame back at the head of this
                            // port's egress and let a TX-completion IRQ retry.
                            endpoint.requeue_egress(frame);
                            return progressed || submitted > 0;
                        }
                    }
                }
            }
            progressed = true;
        }
    }
    progressed || submitted > 0
}

enum HostTxResult {
    Submitted,
    /// TX ring temporarily full; caller requeues and waits for completion IRQ.
    Retried,
}

/// Copies `frame` into a host TX buffer and submits it. `Retry` means the ring
/// is momentarily full, not an error.
fn submit_host_tx(core: &Arc<UplinkWorkerCore>, frame: &[u8]) -> HostTxResult {
    let mut queues = core.queues.lock();
    let result = queues.tx.prepare_send(frame.len(), |buffer| {
        buffer.copy_from_slice(frame);
    });
    let result = match result {
        Ok(((), mut pending)) => pending.try_submit(),
        Err(error) => {
            warn!(
                "host uplink {} dropped TX frame: {error}",
                fmt_mac(core.host_mac)
            );
            return HostTxResult::Submitted; // drop; not a retry
        }
    };
    match result {
        Ok(()) => HostTxResult::Submitted,
        Err(NetError::Retry) => HostTxResult::Retried,
        Err(error) => {
            warn!(
                "host uplink {} dropped TX frame: {error}",
                fmt_mac(core.host_mac)
            );
            HostTxResult::Submitted
        }
    }
}

/// Reclaims host RX frames and hands each to the switch for distribution.
fn progress_rx(core: &Arc<UplinkWorkerCore>) -> bool {
    let mut progressed = false;
    for _ in 0..HOST_RX_BUDGET {
        let frame = {
            let mut queues = core.queues.lock();
            queues.rx.receive(|bytes| bytes.to_vec())
        };
        let Some(frame) = frame else {
            break;
        };
        core.switch.switch_from_uplink(&frame);
        progressed = true;
    }
    progressed
}

fn fmt_mac(mac: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn switch_error_string(error: &SwitchError) -> String {
    match error {
        SwitchError::DuplicatePortId(id) => {
            format!(
                "duplicate switch port id vm={} gen={}",
                id.vm_id, id.generation
            )
        }
        SwitchError::DuplicateMac(mac) => format!("duplicate guest MAC {}", fmt_mac(*mac)),
    }
}
