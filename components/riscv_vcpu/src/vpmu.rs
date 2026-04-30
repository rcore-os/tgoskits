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

//! Non-passthrough virtual SBI PMU support for a RISC-V guest vCPU.
//!
//! The physical PMU is not an independently assignable device. Its counters,
//! event selector CSRs, inhibit bits, and overflow state are shared per hart or
//! per core, and programming them from one guest can affect host execution and
//! other guests. A passthrough implementation would therefore expose global
//! machine state through the SBI PMU extension and make ownership of
//! `mhpmevent*`, `hpmcounter*`, and overflow interrupts ambiguous.
//!
//! This module provides a conservative virtual PMU facade instead. Each vCPU
//! owns a small virtual counter bank, and guest SBI PMU calls update that
//! virtual state instead of programming the host PMU. Fixed PMU counters
//! (`cycle` and `instret`) are represented as virtual values plus a hardware
//! baseline captured at `START` time. Firmware counters are incremented
//! explicitly by the vCPU when the corresponding hypervisor-visible action is
//! performed. The architectural `time` CSR is intentionally not exposed as an
//! SBI PMU counter because the SBI PMU event list has no standard event that
//! maps to wall-clock time.
//!
//! The current design intentionally favors isolation and predictable guest ABI
//! behavior over complete hardware profiling fidelity. Unsupported PMU event
//! classes return SBI errors instead of falling back to passthrough.
//!
//! Implemented:
//! - SBI PMU dispatch for `NUM_COUNTERS`, `COUNTER_GET_INFO`,
//!   `COUNTER_CONFIG_MATCHING`, `COUNTER_START`, `COUNTER_STOP`,
//!   `COUNTER_FW_READ`, and `COUNTER_FW_READ_HI`.
//! - Per-vCPU virtual counter state (`configured`, `started`, selected event,
//!   virtual value, and fixed-counter hardware baseline).
//! - Fixed PMU counter discovery and internal virtual value tracking for
//!   `cycle` and `instret`.
//! - Conservative flag handling for `SKIP_MATCH`, `CLEAR_VALUE`, `AUTO_START`,
//!   `START_SET_INIT_VALUE`, and `STOP_RESET`.
//! - Firmware counters for hypervisor-observable events: `SET_TIMER`,
//!   `ILLEGAL_INSN`, `ACCESS_LOAD`, `ACCESS_STORE`, and the RFENCE/HFENCE
//!   "sent" events that pass through this vCPU's SBI path.
//!
//! Not implemented yet:
//! - Guest CSR reads of `cycle`, `time`, and `instret` still observe the
//!   hardware view until the vCPU traps and emulates those CSR reads using this
//!   module's virtual fixed-counter values or a separate virtual time source.
//! - Virtual `hpmcounter3..31` and `mhpmevent3..31` state, cache/raw/platform
//!   hardware events, and host PMU scheduling or multiplexing.
//! - PMU overflow detection, overflow interrupt injection, and overflow
//!   pending state.
//! - SBI PMU snapshot shared memory (`SNAPSHOT_SET_SHMEM`).
//! - Privilege-mode filter enforcement for the `VUINH`/`VSINH`/`UINH`/
//!   `SINH`/`MINH` configuration flags.
//! - Firmware counters that do not currently pass through this vCPU path, such
//!   as received RFENCE/HFENCE events, IPI counters, and platform-specific
//!   counters.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use rustsbi::Pmu;
use sbi_spec::{binary::SbiRet, pmu};

/// Per-vCPU virtual implementation of the SBI PMU extension.
///
/// A `VirtualPmu` instance is embedded in the SBI object owned by one virtual
/// CPU. The instance stores all guest-visible PMU state locally and never
/// programs host PMU selector CSRs. This keeps guest PMU operations isolated
/// from other guests even when the surrounding virtualization model forwards
/// many other SBI calls to the host firmware.
///
/// Counter layout:
///
/// - Counter 0 exposes the architectural fixed `cycle` counter.
/// - Counter 1 exposes the architectural fixed `instret` counter.
/// - Counters 2 and above expose selected SBI firmware counters whose events
///   can be observed by this vCPU implementation.
///
/// Fixed PMU counters use a virtual offset model. When a fixed counter is
/// started, the current hardware CSR value is captured as `hardware_base`.
/// While the counter remains running, the guest-visible value is
/// `value + (hardware_now - hardware_base)`. When the counter is stopped
/// without `STOP_RESET`, the current delta is folded back into `value`,
/// allowing later starts to continue from the same guest-visible value or from
/// a supplied initial value.
pub(crate) struct VirtualPmu {
    /// Guest-visible counter state indexed by virtual counter number.
    counters: [VirtualPmuCounter; Self::NUM_COUNTERS],
}

/// Mutable state for one virtual PMU counter.
///
/// The fields are atomic because SBI calls and event recording hooks may be
/// reached through different execution paths. The current vCPU model normally
/// serializes access per vCPU, so relaxed ordering is sufficient: the atomics
/// provide race-free storage without imposing cross-counter synchronization
/// semantics that the virtual PMU does not require.
#[derive(Default)]
struct VirtualPmuCounter {
    /// SBI event index currently bound to this counter.
    ///
    /// The value is `UNCONFIGURED_EVENT` until `COUNTER_CONFIG_MATCHING`
    /// successfully associates the counter with a supported event.
    event_idx: AtomicUsize,
    /// Whether the counter has been configured for a guest-visible event.
    ///
    /// A counter must be configured before it can be started or read through
    /// SBI PMU operations.
    configured: AtomicBool,
    /// Whether the counter is currently counting.
    ///
    /// Firmware counters only increment while this flag is set. Fixed PMU
    /// counters use it to decide whether `hardware_base` should be applied to
    /// the stored virtual value.
    started: AtomicBool,
    /// Stored virtual counter value.
    ///
    /// For firmware counters this is the complete counter value. For fixed PMU
    /// counters this is the accumulated guest-visible value at the last clear,
    /// explicit initialization, or stop point; while running, the live hardware
    /// delta is added on top.
    value: AtomicU64,
    /// Hardware CSR baseline for a running fixed PMU counter.
    ///
    /// This field is meaningful only for `cycle` and `instret` while `started`
    /// is true. It is reset to zero when a fixed counter is stopped or cleared.
    hardware_base: AtomicU64,
}

impl VirtualPmu {
    /// Number of virtual counters exposed to the guest through SBI PMU.
    const NUM_COUNTERS: usize = 13;
    /// Virtual counter index for the architectural `cycle` CSR.
    const CYCLE_COUNTER: usize = 0;
    /// Virtual counter index for the architectural `instret` CSR.
    const INSTRET_COUNTER: usize = 1;
    /// Firmware counter index for `SBI_PMU_FW_SET_TIMER`.
    const FW_SET_TIMER_COUNTER: usize = 2;
    /// Firmware counter index for `SBI_PMU_FW_ILLEGAL_INSN`.
    const FW_ILLEGAL_INSN_COUNTER: usize = 3;
    /// Firmware counter index for `SBI_PMU_FW_ACCESS_LOAD`.
    const FW_ACCESS_LOAD_COUNTER: usize = 4;
    /// Firmware counter index for `SBI_PMU_FW_ACCESS_STORE`.
    const FW_ACCESS_STORE_COUNTER: usize = 5;
    /// Firmware counter index for `SBI_PMU_FW_FENCE_I_SENT`.
    const FW_FENCE_I_SENT_COUNTER: usize = 6;
    /// Firmware counter index for `SBI_PMU_FW_SFENCE_VMA_SENT`.
    const FW_SFENCE_VMA_SENT_COUNTER: usize = 7;
    /// Firmware counter index for `SBI_PMU_FW_SFENCE_VMA_ASID_SENT`.
    const FW_SFENCE_VMA_ASID_SENT_COUNTER: usize = 8;
    /// Firmware counter index for `SBI_PMU_FW_HFENCE_GVMA_SENT`.
    const FW_HFENCE_GVMA_SENT_COUNTER: usize = 9;
    /// Firmware counter index for `SBI_PMU_FW_HFENCE_GVMA_VMID_SENT`.
    const FW_HFENCE_GVMA_VMID_SENT_COUNTER: usize = 10;
    /// Firmware counter index for `SBI_PMU_FW_HFENCE_VVMA_SENT`.
    const FW_HFENCE_VVMA_SENT_COUNTER: usize = 11;
    /// Firmware counter index for `SBI_PMU_FW_HFENCE_VVMA_ASID_SENT`.
    const FW_HFENCE_VVMA_ASID_SENT_COUNTER: usize = 12;
    /// Reported fixed-counter width encoded in SBI `COUNTER_GET_INFO`.
    ///
    /// SBI encodes the width as the most significant valid bit index rather
    /// than the number of bits. `usize::BITS - 1` matches the architectural CSR
    /// width used by this target.
    const COUNTER_WIDTH: usize = usize::BITS as usize - 1;
    /// CSR number for the architectural `cycle` counter.
    const CSR_CYCLE: usize = 0xc00;
    /// CSR number for the architectural `instret` counter.
    const CSR_INSTRET: usize = 0xc02;
    /// SBI counter-info marker for a firmware counter.
    const FIRMWARE_COUNTER_TYPE: usize = 1 << (usize::BITS as usize - 1);
    /// Sentinel event index stored in counters that have not been configured.
    const UNCONFIGURED_EVENT: usize = usize::MAX;

    /// `COUNTER_CONFIG_MATCHING` flag requesting direct use of the supplied set.
    const CFG_FLAG_SKIP_MATCH: usize = 1 << 0;
    /// `COUNTER_CONFIG_MATCHING` flag requesting that the counter value be reset.
    const CFG_FLAG_CLEAR_VALUE: usize = 1 << 1;
    /// `COUNTER_CONFIG_MATCHING` flag requesting that the counter start immediately.
    const CFG_FLAG_AUTO_START: usize = 1 << 2;
    /// Mask of configuration flags accepted by this implementation.
    ///
    /// The SBI PMU specification currently defines mode-inhibit bits in the
    /// low flag byte as well. They are accepted for ABI compatibility, although
    /// this virtual PMU does not yet enforce privilege-mode filtering.
    const CFG_VALID_FLAGS: usize = 0xff;
    /// `COUNTER_START` flag requesting initialization to `initial_value`.
    const START_FLAG_SET_INIT_VALUE: usize = 1 << 0;
    /// `COUNTER_STOP` flag requesting that the counter be reset after stopping.
    const STOP_FLAG_RESET: usize = 1 << 0;

    /// Build SBI `COUNTER_GET_INFO` metadata for a CSR-backed fixed PMU counter.
    ///
    /// The SBI return value combines the CSR number with the reported counter
    /// width. Fixed PMU counters are advertised as direct CSR counters so a
    /// guest can discover the architectural CSR associated with each virtual
    /// counter.
    #[inline]
    fn counter_info(csr: usize) -> usize {
        csr | (Self::COUNTER_WIDTH << 12)
    }

    /// Test whether `counter_idx` is selected by an SBI counter set.
    ///
    /// SBI PMU calls address a set of counters with `(counter_idx_base,
    /// counter_idx_mask)`. Bit `n` in the mask selects counter
    /// `counter_idx_base + n`. This helper performs the bounds arithmetic
    /// defensively so overflow or negative offsets never wrap into a match.
    #[inline]
    fn counter_in_set(
        counter_idx: usize,
        counter_idx_base: usize,
        counter_idx_mask: usize,
    ) -> bool {
        let Some(offset) = counter_idx.checked_sub(counter_idx_base) else {
            return false;
        };

        offset < usize::BITS as usize && (counter_idx_mask & (1usize << offset)) != 0
    }

    /// Validate that a guest-supplied counter set is non-empty and in range.
    ///
    /// The method checks every selected bit in the mask. A set is invalid if it
    /// selects no counters, overflows while adding the base, or refers to a
    /// counter beyond the virtual bank exposed by `NUM_COUNTERS`.
    #[inline]
    fn validate_counter_set(counter_idx_base: usize, counter_idx_mask: usize) -> SbiRet {
        if counter_idx_mask == 0 {
            return SbiRet::invalid_param();
        }

        for offset in 0..usize::BITS as usize {
            if (counter_idx_mask & (1usize << offset)) == 0 {
                continue;
            }

            let Some(counter_idx) = counter_idx_base.checked_add(offset) else {
                return SbiRet::invalid_param();
            };
            if counter_idx >= Self::NUM_COUNTERS {
                return SbiRet::invalid_param();
            }
        }

        SbiRet::success(0)
    }

    /// Return the first virtual counter selected by an SBI counter set.
    ///
    /// This is used for `SKIP_MATCH`, where the guest asks the implementation
    /// to use the selected counter directly rather than searching for a counter
    /// that supports the event.
    #[inline]
    fn first_counter_in_set(counter_idx_base: usize, counter_idx_mask: usize) -> Option<usize> {
        (0..Self::NUM_COUNTERS).find(|&counter_idx| {
            Self::counter_in_set(counter_idx, counter_idx_base, counter_idx_mask)
        })
    }

    /// Map an SBI PMU event index to the virtual counter that can count it.
    ///
    /// Only events that can be represented without host PMU passthrough are
    /// mapped. General hardware events are limited to architectural fixed
    /// counters, and firmware events are limited to actions that this vCPU
    /// implementation can observe directly. Unsupported hardware, cache, raw,
    /// and platform events return `None` so the SBI call can report
    /// `SBI_ERR_NOT_SUPPORTED`.
    #[inline]
    fn event_counter(event_idx: usize) -> Option<usize> {
        let event_type = event_idx >> 16;
        let event_code = event_idx & 0xffff;

        match (event_type, event_code) {
            (pmu::event_type::HARDWARE_GENERAL, pmu::hardware_event::CPU_CYCLES) => {
                Some(Self::CYCLE_COUNTER)
            }
            (pmu::event_type::HARDWARE_GENERAL, pmu::hardware_event::INSTRUCTIONS) => {
                Some(Self::INSTRET_COUNTER)
            }
            (pmu::event_type::FIRMWARE, pmu::firmware_event::SET_TIMER) => {
                Some(Self::FW_SET_TIMER_COUNTER)
            }
            (pmu::event_type::FIRMWARE, pmu::firmware_event::ILLEGAL_INSN) => {
                Some(Self::FW_ILLEGAL_INSN_COUNTER)
            }
            (pmu::event_type::FIRMWARE, pmu::firmware_event::ACCESS_LOAD) => {
                Some(Self::FW_ACCESS_LOAD_COUNTER)
            }
            (pmu::event_type::FIRMWARE, pmu::firmware_event::ACCESS_STORE) => {
                Some(Self::FW_ACCESS_STORE_COUNTER)
            }
            (pmu::event_type::FIRMWARE, pmu::firmware_event::FENCE_I_SENT) => {
                Some(Self::FW_FENCE_I_SENT_COUNTER)
            }
            (pmu::event_type::FIRMWARE, pmu::firmware_event::SFENCE_VMA_SENT) => {
                Some(Self::FW_SFENCE_VMA_SENT_COUNTER)
            }
            (pmu::event_type::FIRMWARE, pmu::firmware_event::SFENCE_VMA_ASID_SENT) => {
                Some(Self::FW_SFENCE_VMA_ASID_SENT_COUNTER)
            }
            (pmu::event_type::FIRMWARE, pmu::firmware_event::HFENCE_GVMA_SENT) => {
                Some(Self::FW_HFENCE_GVMA_SENT_COUNTER)
            }
            (pmu::event_type::FIRMWARE, pmu::firmware_event::HFENCE_GVMA_VMID_SENT) => {
                Some(Self::FW_HFENCE_GVMA_VMID_SENT_COUNTER)
            }
            (pmu::event_type::FIRMWARE, pmu::firmware_event::HFENCE_VVMA_SENT) => {
                Some(Self::FW_HFENCE_VVMA_SENT_COUNTER)
            }
            (pmu::event_type::FIRMWARE, pmu::firmware_event::HFENCE_VVMA_ASID_SENT) => {
                Some(Self::FW_HFENCE_VVMA_ASID_SENT_COUNTER)
            }
            _ => None,
        }
    }

    /// Check whether a specific virtual counter can count `event_idx`.
    ///
    /// This helper is used by the `SKIP_MATCH` configuration path. Even when
    /// the guest asks to skip matching, the chosen counter still has to be
    /// compatible with the requested event.
    #[inline]
    fn counter_supports_event(counter_idx: usize, event_idx: usize) -> bool {
        Self::event_counter(event_idx) == Some(counter_idx)
    }

    /// Return whether a counter is one of the virtual firmware counters.
    ///
    /// Firmware counters are read through `COUNTER_FW_READ` rather than direct
    /// CSR reads. They are incremented by explicit hooks in the vCPU code when
    /// the corresponding SBI or trap-handling operation occurs.
    #[inline]
    fn counter_is_firmware(counter_idx: usize) -> bool {
        (Self::FW_SET_TIMER_COUNTER..Self::NUM_COUNTERS).contains(&counter_idx)
    }

    /// Return whether a counter is one of the fixed CSR-backed PMU counters.
    ///
    /// Fixed PMU counters use the offset/baseline model implemented by
    /// `fixed_virtual_value` instead of explicit event increments.
    #[inline]
    fn counter_is_fixed(counter_idx: usize) -> bool {
        counter_idx <= Self::INSTRET_COUNTER
    }

    /// Read the current host hardware value for a fixed architectural counter.
    ///
    /// The returned value is used only as a delta source for virtual fixed
    /// counters. It is not exposed directly as the guest-visible value unless
    /// the guest has started the virtual counter with a zero offset and no
    /// later stop/clear/init operation.
    ///
    /// Returns zero for non-fixed counter indices; callers normally guard with
    /// `counter_is_fixed` before invoking this method.
    #[inline]
    fn fixed_hardware_value(counter_idx: usize) -> u64 {
        let value: usize;
        match counter_idx {
            Self::CYCLE_COUNTER => unsafe {
                core::arch::asm!("csrr {value}, cycle", value = out(reg) value);
            },
            Self::INSTRET_COUNTER => unsafe {
                core::arch::asm!("csrr {value}, instret", value = out(reg) value);
            },
            _ => return 0,
        }
        value as u64
    }

    /// Compute the guest-visible value for a fixed PMU counter.
    ///
    /// If the counter is stopped, the stored virtual value is already complete.
    /// If it is running, the method adds the elapsed hardware-counter delta
    /// since the last start to the stored virtual value. Wrapping arithmetic is
    /// used to preserve architectural counter behavior across hardware counter
    /// wraparound.
    #[inline]
    fn fixed_virtual_value(&self, counter_idx: usize) -> u64 {
        let counter = &self.counters[counter_idx];
        let value = counter.value.load(Ordering::Relaxed);
        if !counter.started.load(Ordering::Relaxed) {
            return value;
        }

        let hardware_base = counter.hardware_base.load(Ordering::Relaxed);
        let hardware_now = Self::fixed_hardware_value(counter_idx);
        value.wrapping_add(hardware_now.wrapping_sub(hardware_base))
    }

    /// Reset a counter to the unconfigured state.
    ///
    /// This implements the reset side effect requested by `COUNTER_STOP` with
    /// `STOP_RESET`. The selected event, configured/start state, virtual value,
    /// and fixed-counter baseline are all cleared.
    #[inline]
    fn reset_counter(&self, counter_idx: usize) {
        let counter = &self.counters[counter_idx];
        counter
            .event_idx
            .store(Self::UNCONFIGURED_EVENT, Ordering::Relaxed);
        counter.configured.store(false, Ordering::Relaxed);
        counter.started.store(false, Ordering::Relaxed);
        counter.value.store(0, Ordering::Relaxed);
        counter.hardware_base.store(0, Ordering::Relaxed);
    }
}

impl Default for VirtualPmu {
    /// Create an empty virtual PMU counter bank.
    ///
    /// All counters start unconfigured and stopped. Firmware counters begin at
    /// zero, and fixed PMU counters have no hardware baseline until they are
    /// started by the guest.
    fn default() -> Self {
        let counters = core::array::from_fn(|_| VirtualPmuCounter {
            event_idx: AtomicUsize::new(Self::UNCONFIGURED_EVENT),
            configured: AtomicBool::new(false),
            started: AtomicBool::new(false),
            value: AtomicU64::new(0),
            hardware_base: AtomicU64::new(0),
        });

        Self { counters }
    }
}

impl VirtualPmu {
    /// Record one guest-visible `SET_TIMER` firmware event.
    ///
    /// The vCPU should call this when it handles or forwards a guest timer SBI
    /// request. The event is counted only if the guest configured and started
    /// the corresponding firmware counter.
    #[inline]
    pub(crate) fn record_set_timer(&self) {
        self.record_firmware_event(Self::FW_SET_TIMER_COUNTER);
    }

    /// Record one guest-visible illegal-instruction firmware event.
    ///
    /// This tracks illegal instruction traps that are observed by the vCPU
    /// emulation path. It does not count illegal instructions handled entirely
    /// outside this vCPU path.
    #[inline]
    pub(crate) fn record_illegal_insn(&self) {
        self.record_firmware_event(Self::FW_ILLEGAL_INSN_COUNTER);
    }

    /// Record one guest-visible access-load firmware event.
    ///
    /// This is intended for guest load access faults that are handled by the
    /// hypervisor, such as MMIO emulation paths.
    #[inline]
    pub(crate) fn record_access_load(&self) {
        self.record_firmware_event(Self::FW_ACCESS_LOAD_COUNTER);
    }

    /// Record one guest-visible access-store firmware event.
    ///
    /// This is intended for guest store or AMO access faults that are handled
    /// by the hypervisor, such as MMIO emulation paths.
    #[inline]
    pub(crate) fn record_access_store(&self) {
        self.record_firmware_event(Self::FW_ACCESS_STORE_COUNTER);
    }

    /// Record one guest-visible sent `FENCE.I` firmware event.
    ///
    /// The event corresponds to an RFENCE-style operation initiated by this
    /// vCPU path. Received remote fence events are not currently modeled.
    #[inline]
    pub(crate) fn record_fence_i_sent(&self) {
        self.record_firmware_event(Self::FW_FENCE_I_SENT_COUNTER);
    }

    /// Record one guest-visible sent `SFENCE.VMA` firmware event.
    ///
    /// This counts sent remote TLB flush requests that do not include an ASID
    /// qualifier.
    #[inline]
    pub(crate) fn record_sfence_vma_sent(&self) {
        self.record_firmware_event(Self::FW_SFENCE_VMA_SENT_COUNTER);
    }

    /// Record one guest-visible sent `SFENCE.VMA` with ASID firmware event.
    ///
    /// This counts sent remote TLB flush requests that include an ASID
    /// qualifier.
    #[inline]
    pub(crate) fn record_sfence_vma_asid_sent(&self) {
        self.record_firmware_event(Self::FW_SFENCE_VMA_ASID_SENT_COUNTER);
    }

    /// Record one guest-visible sent `HFENCE.GVMA` firmware event.
    ///
    /// This counts sent hypervisor guest-physical TLB flush requests that do
    /// not include a VMID qualifier.
    #[inline]
    pub(crate) fn record_hfence_gvma_sent(&self) {
        self.record_firmware_event(Self::FW_HFENCE_GVMA_SENT_COUNTER);
    }

    /// Record one guest-visible sent `HFENCE.GVMA` with VMID firmware event.
    ///
    /// This counts sent hypervisor guest-physical TLB flush requests that
    /// include a VMID qualifier.
    #[inline]
    pub(crate) fn record_hfence_gvma_vmid_sent(&self) {
        self.record_firmware_event(Self::FW_HFENCE_GVMA_VMID_SENT_COUNTER);
    }

    /// Record one guest-visible sent `HFENCE.VVMA` firmware event.
    ///
    /// This counts sent virtual-address TLB flush requests for a guest virtual
    /// machine that do not include an ASID qualifier.
    #[inline]
    pub(crate) fn record_hfence_vvma_sent(&self) {
        self.record_firmware_event(Self::FW_HFENCE_VVMA_SENT_COUNTER);
    }

    /// Record one guest-visible sent `HFENCE.VVMA` with ASID firmware event.
    ///
    /// This counts sent virtual-address TLB flush requests for a guest virtual
    /// machine that include an ASID qualifier.
    #[inline]
    pub(crate) fn record_hfence_vvma_asid_sent(&self) {
        self.record_firmware_event(Self::FW_HFENCE_VVMA_ASID_SENT_COUNTER);
    }

    /// Read the virtual `cycle` fixed PMU counter.
    ///
    /// This helper is intended for a future CSR emulation path. It returns the
    /// virtual value maintained by this PMU rather than the raw hardware CSR.
    /// The counter must have been configured by the guest through SBI PMU.
    #[inline]
    pub(crate) fn read_cycle(&self) -> SbiRet {
        self.read_fixed_counter(Self::CYCLE_COUNTER)
    }

    /// Read the virtual `instret` fixed PMU counter.
    ///
    /// This helper is intended for a future CSR emulation path. Until CSR reads
    /// are trapped and redirected here, a guest direct CSR read may still see
    /// the underlying hardware value.
    #[inline]
    pub(crate) fn read_instret(&self) -> SbiRet {
        self.read_fixed_counter(Self::INSTRET_COUNTER)
    }

    /// Read a configured fixed PMU counter by virtual counter index.
    ///
    /// Returns `SBI_ERR_INVALID_PARAM` if the counter has not been configured.
    /// This matches the conservative behavior used for firmware-counter reads:
    /// guest code should configure a counter before consuming its value through
    /// the PMU interface.
    #[inline]
    fn read_fixed_counter(&self, counter_idx: usize) -> SbiRet {
        let counter = &self.counters[counter_idx];
        if !counter.configured.load(Ordering::Relaxed) {
            return SbiRet::invalid_param();
        }

        SbiRet::success(self.fixed_virtual_value(counter_idx) as usize)
    }

    /// Increment a firmware counter if it is currently active.
    ///
    /// Firmware counters are purely virtual; they advance only when the vCPU
    /// explicitly observes the matching event. Unconfigured or stopped counters
    /// ignore events so start/stop windows behave as the guest requested.
    #[inline]
    fn record_firmware_event(&self, counter_idx: usize) {
        let counter = &self.counters[counter_idx];
        if counter.configured.load(Ordering::Relaxed) && counter.started.load(Ordering::Relaxed) {
            counter.value.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Pmu for VirtualPmu {
    /// Return the number of virtual counters exposed by this PMU instance.
    #[inline]
    fn num_counters(&self) -> usize {
        Self::NUM_COUNTERS
    }

    /// Return SBI metadata for a virtual counter.
    ///
    /// Fixed PMU counters are reported as CSR-backed counters with their
    /// architectural CSR numbers. Firmware counters are reported as firmware
    /// counters. Any index outside the virtual counter bank is rejected.
    #[inline]
    fn counter_get_info(&self, counter_idx: usize) -> SbiRet {
        match counter_idx {
            Self::CYCLE_COUNTER => SbiRet::success(Self::counter_info(Self::CSR_CYCLE)),
            Self::INSTRET_COUNTER => SbiRet::success(Self::counter_info(Self::CSR_INSTRET)),
            Self::FW_SET_TIMER_COUNTER => SbiRet::success(Self::FIRMWARE_COUNTER_TYPE),
            Self::FW_ILLEGAL_INSN_COUNTER => SbiRet::success(Self::FIRMWARE_COUNTER_TYPE),
            Self::FW_ACCESS_LOAD_COUNTER => SbiRet::success(Self::FIRMWARE_COUNTER_TYPE),
            Self::FW_ACCESS_STORE_COUNTER => SbiRet::success(Self::FIRMWARE_COUNTER_TYPE),
            Self::FW_FENCE_I_SENT_COUNTER => SbiRet::success(Self::FIRMWARE_COUNTER_TYPE),
            Self::FW_SFENCE_VMA_SENT_COUNTER => SbiRet::success(Self::FIRMWARE_COUNTER_TYPE),
            Self::FW_SFENCE_VMA_ASID_SENT_COUNTER => SbiRet::success(Self::FIRMWARE_COUNTER_TYPE),
            Self::FW_HFENCE_GVMA_SENT_COUNTER => SbiRet::success(Self::FIRMWARE_COUNTER_TYPE),
            Self::FW_HFENCE_GVMA_VMID_SENT_COUNTER => SbiRet::success(Self::FIRMWARE_COUNTER_TYPE),
            Self::FW_HFENCE_VVMA_SENT_COUNTER => SbiRet::success(Self::FIRMWARE_COUNTER_TYPE),
            Self::FW_HFENCE_VVMA_ASID_SENT_COUNTER => SbiRet::success(Self::FIRMWARE_COUNTER_TYPE),
            _ => SbiRet::invalid_param(),
        }
    }

    /// Configure a virtual counter for an SBI PMU event.
    ///
    /// The implementation supports only events that can be represented by the
    /// fixed PMU counters or by vCPU-observable firmware hooks. When
    /// `SKIP_MATCH` is set, the first selected counter is used only if it
    /// supports the requested event. Otherwise the event is mapped to its
    /// dedicated virtual counter and then checked against the guest-supplied
    /// counter set.
    ///
    /// `CLEAR_VALUE` resets the stored virtual value, and `AUTO_START` starts
    /// the counter immediately. For fixed PMU counters, auto-start also
    /// captures the current hardware CSR value as the baseline for future
    /// virtual delta calculation. A stopped, already-configured counter may be
    /// reconfigured for another supported event. A running counter is rejected
    /// with `SBI_ERR_ALREADY_STARTED` even if `AUTO_START` is not set, matching
    /// the SBI PMU lifecycle rule for `COUNTER_CONFIG_MATCHING`.
    #[inline]
    fn counter_config_matching(
        &self,
        counter_idx_base: usize,
        counter_idx_mask: usize,
        config_flags: usize,
        event_idx: usize,
        _event_data: u64,
    ) -> SbiRet {
        if (config_flags & !Self::CFG_VALID_FLAGS) != 0 {
            return SbiRet::invalid_param();
        }
        let ret = Self::validate_counter_set(counter_idx_base, counter_idx_mask);
        if ret.is_err() {
            return ret;
        }

        let counter_idx = if (config_flags & Self::CFG_FLAG_SKIP_MATCH) != 0 {
            let Some(counter_idx) = Self::first_counter_in_set(counter_idx_base, counter_idx_mask)
            else {
                return SbiRet::invalid_param();
            };
            if !Self::counter_supports_event(counter_idx, event_idx) {
                return SbiRet::not_supported();
            }
            counter_idx
        } else {
            let Some(counter_idx) = Self::event_counter(event_idx) else {
                return SbiRet::not_supported();
            };
            if !Self::counter_in_set(counter_idx, counter_idx_base, counter_idx_mask) {
                return SbiRet::not_supported();
            }
            counter_idx
        };

        let counter = &self.counters[counter_idx];
        if counter.started.load(Ordering::Relaxed) {
            return SbiRet::already_started();
        }

        counter.event_idx.store(event_idx, Ordering::Relaxed);
        counter.configured.store(true, Ordering::Relaxed);
        if (config_flags & Self::CFG_FLAG_CLEAR_VALUE) != 0 {
            counter.value.store(0, Ordering::Relaxed);
            counter.hardware_base.store(0, Ordering::Relaxed);
        }
        if (config_flags & Self::CFG_FLAG_AUTO_START) != 0 {
            if Self::counter_is_fixed(counter_idx) {
                counter
                    .hardware_base
                    .store(Self::fixed_hardware_value(counter_idx), Ordering::Relaxed);
            }
            counter.started.store(true, Ordering::Relaxed);
        }
        SbiRet::success(counter_idx)
    }

    /// Start one or more configured virtual counters.
    ///
    /// All selected counters are validated before any counter is modified. This
    /// keeps the operation atomic from the guest's perspective: if one selected
    /// counter is unconfigured or already running, no selected counter is
    /// started. When `START_SET_INIT_VALUE` is present, each selected counter's
    /// stored virtual value is replaced with `initial_value` before counting
    /// begins. Fixed PMU counters capture a fresh hardware baseline at start
    /// time.
    #[inline]
    fn counter_start(
        &self,
        counter_idx_base: usize,
        counter_idx_mask: usize,
        start_flags: usize,
        initial_value: u64,
    ) -> SbiRet {
        if (start_flags & !Self::START_FLAG_SET_INIT_VALUE) != 0 {
            return SbiRet::invalid_param();
        }
        let ret = Self::validate_counter_set(counter_idx_base, counter_idx_mask);
        if ret.is_err() {
            return ret;
        }
        for counter_idx in 0..Self::NUM_COUNTERS {
            if !Self::counter_in_set(counter_idx, counter_idx_base, counter_idx_mask) {
                continue;
            }

            let counter = &self.counters[counter_idx];
            if !counter.configured.load(Ordering::Relaxed) {
                return SbiRet::invalid_param();
            }
            if counter.started.load(Ordering::Relaxed) {
                return SbiRet::already_started();
            }
        }

        for counter_idx in 0..Self::NUM_COUNTERS {
            if Self::counter_in_set(counter_idx, counter_idx_base, counter_idx_mask) {
                if (start_flags & Self::START_FLAG_SET_INIT_VALUE) != 0 {
                    self.counters[counter_idx]
                        .value
                        .store(initial_value, Ordering::Relaxed);
                }
                if Self::counter_is_fixed(counter_idx) {
                    self.counters[counter_idx]
                        .hardware_base
                        .store(Self::fixed_hardware_value(counter_idx), Ordering::Relaxed);
                }
                self.counters[counter_idx]
                    .started
                    .store(true, Ordering::Relaxed);
            }
        }

        SbiRet::success(0)
    }

    /// Stop one or more running virtual counters.
    ///
    /// As with `counter_start`, validation is performed for the entire selected
    /// set before mutating any counter. Stopping a fixed PMU counter without
    /// `STOP_RESET` folds the live hardware delta into the stored virtual value
    /// so a later read or restart sees a stable guest-visible value. If
    /// `STOP_RESET` is supplied, the counter is returned to the unconfigured
    /// state directly and the fixed-counter fold is skipped because the stored
    /// value would be discarded.
    ///
    /// This implementation intentionally returns `SBI_ERR_INVALID_PARAM` when
    /// the selected counter has never been configured. The SBI PMU
    /// specification defines invalid-parameter errors for invalid counter
    /// indices, while a never-configured counter is also stopped in the narrow
    /// state-machine sense. Returning `ALREADY_STOPPED` for that case would be
    /// spec-compatible, but the stricter error makes guest lifecycle mistakes
    /// easier to diagnose.
    #[inline]
    fn counter_stop(
        &self,
        counter_idx_base: usize,
        counter_idx_mask: usize,
        stop_flags: usize,
    ) -> SbiRet {
        if (stop_flags & !Self::STOP_FLAG_RESET) != 0 {
            return SbiRet::invalid_param();
        }
        let ret = Self::validate_counter_set(counter_idx_base, counter_idx_mask);
        if ret.is_err() {
            return ret;
        }

        for counter_idx in 0..Self::NUM_COUNTERS {
            if !Self::counter_in_set(counter_idx, counter_idx_base, counter_idx_mask) {
                continue;
            }

            let counter = &self.counters[counter_idx];
            if !counter.configured.load(Ordering::Relaxed) {
                return SbiRet::invalid_param();
            }
            if !counter.started.load(Ordering::Relaxed) {
                return SbiRet::already_stopped();
            }
        }

        for counter_idx in 0..Self::NUM_COUNTERS {
            if !Self::counter_in_set(counter_idx, counter_idx_base, counter_idx_mask) {
                continue;
            }

            let counter = &self.counters[counter_idx];
            if (stop_flags & Self::STOP_FLAG_RESET) != 0 {
                self.reset_counter(counter_idx);
                continue;
            }
            if Self::counter_is_fixed(counter_idx) {
                counter
                    .value
                    .store(self.fixed_virtual_value(counter_idx), Ordering::Relaxed);
                counter.hardware_base.store(0, Ordering::Relaxed);
            }
            counter.started.store(false, Ordering::Relaxed);
        }

        SbiRet::success(0)
    }

    /// Read the low word of a configured firmware counter.
    ///
    /// SBI PMU exposes firmware-counter values through explicit read calls
    /// rather than direct CSR access. This method rejects fixed PMU counters and
    /// unconfigured firmware counters.
    #[inline]
    fn counter_fw_read(&self, counter_idx: usize) -> SbiRet {
        if !Self::counter_is_firmware(counter_idx) {
            return SbiRet::invalid_param();
        }

        let counter = &self.counters[counter_idx];
        if !counter.configured.load(Ordering::Relaxed) {
            return SbiRet::invalid_param();
        }

        SbiRet::success(counter.value.load(Ordering::Relaxed) as usize)
    }

    /// Read the high word of a configured firmware counter.
    ///
    /// On RV32, the high 32 bits are returned so a guest can reconstruct the
    /// full 64-bit firmware counter value. On RV64, SBI specifies that the high
    /// read returns zero because the low read already carries the full value.
    #[inline]
    fn counter_fw_read_hi(&self, counter_idx: usize) -> SbiRet {
        if !Self::counter_is_firmware(counter_idx) {
            return SbiRet::invalid_param();
        }

        let counter = &self.counters[counter_idx];
        if !counter.configured.load(Ordering::Relaxed) {
            return SbiRet::invalid_param();
        }

        #[cfg(target_pointer_width = "32")]
        {
            SbiRet::success((counter.value.load(Ordering::Relaxed) >> 32) as usize)
        }
        #[cfg(not(target_pointer_width = "32"))]
        {
            SbiRet::success(0)
        }
    }
}
