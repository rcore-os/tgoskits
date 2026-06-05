//! Kprobe / kretprobe perf events. Owns a registered probe + the list of
//! callback ids it has attached to the probe, so `Drop` can detach.
//!
//! Ported from `Starry-OS/StarryOS:ebpf-kmod` (`kernel/src/perf/kprobe.rs`).
//! Symbol resolution goes through the real in-kernel `.kallsyms` blob
//! (`crate::pseudofs::proc::KALLSYMS`), the same table `/proc/kallsyms` reads.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    any::Any,
    sync::atomic::{AtomicU32, Ordering},
};

use ax_errno::{AxError, AxResult};
use axpoll::Pollable;
use kbpf_basic::perf::{PerfProbeArgs, PerfProbeConfig};
use kprobe::{CallBackFunc, KretprobeBuilder, ProbeBuilder, PtRegs};

use crate::{
    file::FileLike,
    kprobe::{
        KernelKprobe, KernelKretprobe, KernelRawMutex, KprobeAuxiliary, register_kprobe,
        register_kretprobe, unregister_kprobe, unregister_kretprobe,
    },
    perf::{PerfEventOps, bpf::OwnedEbpfVm},
    uprobe::{KernelUprobe, unregister_uprobe},
};

/// One of {kprobe, kretprobe, uprobe}. Kprobe/kretprobe live in the global
/// kernel-text manager; uprobe lives in the firing process' per-process manager
/// (`ProcessData::uprobe_manager`), but exposes the same probe API.
#[derive(Debug)]
pub enum ProbeTy {
    Kprobe(Arc<KernelKprobe>),
    Kretprobe(Arc<KernelKretprobe>),
    Uprobe(Arc<KernelUprobe>),
}

/// Per-fd perf event wrapping a kprobe/kretprobe registration.
#[derive(Debug)]
pub struct ProbePerfEvent {
    _args: PerfProbeArgs,
    probe: ProbeTy,
    callback_list: Vec<u32>,
}

impl ProbePerfEvent {
    /// Build a perf event tied to an already-registered probe.
    pub fn new(args: PerfProbeArgs, probe: ProbeTy) -> Self {
        Self {
            _args: args,
            probe,
            callback_list: Vec::new(),
        }
    }
}

impl Drop for ProbePerfEvent {
    fn drop(&mut self) {
        for cid in &self.callback_list {
            match self.probe {
                ProbeTy::Kprobe(ref k) => k.unregister_event_callback(*cid),
                ProbeTy::Kretprobe(ref k) => k.unregister_event_callback(*cid),
                ProbeTy::Uprobe(ref u) => u.unregister_event_callback(*cid),
            }
        }
        match self.probe {
            ProbeTy::Kprobe(ref k) => unregister_kprobe(k.clone()),
            ProbeTy::Kretprobe(ref k) => unregister_kretprobe(k.clone()),
            ProbeTy::Uprobe(ref u) => unregister_uprobe(u.clone()),
        }
    }
}

impl Pollable for ProbePerfEvent {
    fn poll(&self) -> axpoll::IoEvents {
        axpoll::IoEvents::empty()
    }

    fn register(&self, _context: &mut core::task::Context<'_>, _events: axpoll::IoEvents) {
        // No-op: kprobe perf events do not deliver poll readiness; reads
        // happen through the attached BPF program / ringbuf, not the event
        // fd itself.
    }
}

impl PerfEventOps for ProbePerfEvent {
    fn enable(&mut self) -> AxResult<()> {
        match self.probe {
            ProbeTy::Kprobe(ref k) => k.enable(),
            ProbeTy::Kretprobe(ref k) => k.kprobe().enable(),
            ProbeTy::Uprobe(ref u) => u.enable(),
        }
        Ok(())
    }

    fn disable(&mut self) -> AxResult<()> {
        match self.probe {
            ProbeTy::Kprobe(ref k) => k.disable(),
            ProbeTy::Kretprobe(ref k) => k.kprobe().disable(),
            ProbeTy::Uprobe(ref u) => u.disable(),
        }
        Ok(())
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn set_bpf_prog(&mut self, bpf_prog: Arc<dyn FileLike>) -> AxResult<()> {
        let vm = OwnedEbpfVm::new(bpf_prog)?;

        // Monotonically-increasing per-probe callback id. `fetch_add` is
        // itself atomic and never returns the same value twice regardless
        // of ordering, so `Relaxed` is sufficient — we only require atomic
        // unique-id allocation, not synchronization with any other memory
        // location. The counter is `u32`-wide so a tight 4-billion-attach
        // loop would eventually wrap; we never anticipate that here, but
        // if it ever became real we'd switch to `AtomicU64`.
        static CALLBACK_ID: AtomicU32 = AtomicU32::new(0);
        let id = CALLBACK_ID.fetch_add(1, Ordering::Relaxed);

        let callback = Box::new(KprobePerfCallBack::new(vm));
        match self.probe {
            ProbeTy::Kprobe(ref k) => k.register_event_callback(id, callback),
            ProbeTy::Kretprobe(ref k) => k.register_event_callback(id, callback),
            ProbeTy::Uprobe(ref u) => u.register_event_callback(id, callback),
        }
        self.callback_list.push(id);
        Ok(())
    }
}

/// Callback handed to the `kprobe` crate. When the probe fires, the crate
/// invokes `call(&mut pt_regs)` which we hand to the embedded rbpf VM as
/// its single-pointer context argument.
pub struct KprobePerfCallBack {
    /// `execute_with_ptregs` runs off `&self`, so the VM can be invoked
    /// directly from the immutable `call(&self, ..)` path — no interior
    /// mutability / lock required.
    vm: OwnedEbpfVm,
}

impl KprobePerfCallBack {
    fn new(vm: OwnedEbpfVm) -> Self {
        Self { vm }
    }
}

impl CallBackFunc for KprobePerfCallBack {
    fn call(&self, pt_regs: &mut PtRegs) {
        if let Err(e) = self.vm.execute_with_ptregs(pt_regs) {
            error!("kprobe BPF program failed: {e:?}");
        }
    }
}

fn lookup_symbol_addr(symbol: &str) -> AxResult<usize> {
    // Resolve against the real in-kernel `.kallsyms` blob (the same table
    // `/proc/kallsyms` is built from) rather than a separate stub.
    crate::pseudofs::proc::KALLSYMS
        .get()
        .and_then(|t| t.lookup_name(symbol))
        .map(|addr| addr as usize)
        .ok_or(AxError::NotFound)
}

fn perf_probe_arg_to_kprobe_builder(
    args: &PerfProbeArgs,
) -> AxResult<ProbeBuilder<KprobeAuxiliary>> {
    let symbol = &args.name;
    let addr = lookup_symbol_addr(symbol)?;
    Ok(ProbeBuilder::new()
        .with_symbol(symbol.clone())
        .with_symbol_addr(addr)
        .with_offset(0)
        .with_enable(false))
}

fn perf_probe_arg_to_kretprobe_builder(
    args: &PerfProbeArgs,
) -> AxResult<KretprobeBuilder<KernelRawMutex>> {
    let symbol = &args.name;
    let addr = lookup_symbol_addr(symbol)?;
    Ok(KretprobeBuilder::<KernelRawMutex>::new(10)
        .with_symbol(symbol.clone())
        .with_symbol_addr(addr))
}

/// Build a `ProbePerfEvent` for a `PERF_TYPE_KPROBE` perf_event_open call.
/// Config 0 = kprobe; config 1 = kretprobe (per kbpf-basic convention).
pub fn perf_event_open_kprobe(args: PerfProbeArgs) -> AxResult<ProbePerfEvent> {
    let probe = match args.config {
        PerfProbeConfig::Raw(val) => {
            if val == 0 {
                let builder = perf_probe_arg_to_kprobe_builder(&args)?;
                ProbeTy::Kprobe(register_kprobe(builder))
            } else if val == 1 {
                let builder = perf_probe_arg_to_kretprobe_builder(&args)?;
                ProbeTy::Kretprobe(register_kretprobe(builder))
            } else {
                return Err(AxError::InvalidInput);
            }
        }
        _ => return Err(AxError::InvalidInput),
    };
    Ok(ProbePerfEvent::new(args, probe))
}
