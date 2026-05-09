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
/// - Counters 2..=12 expose selected SBI firmware counters whose events
///   can be observed by this vCPU implementation.
/// - Counters 13 and above are hardware performance counters (HPM) dynamically
///   allocated by the backend. The number of HPM slots equals
///   `PmuBackend::num_hpm_counters()` and may be zero (default for QEMU).
///
/// Fixed and HPM counters use a virtual offset model. When a hardware-backed
/// counter is started, the current hardware value is captured as
/// `hardware_base`. While running, the guest-visible value is
/// `value + (hardware_now - hardware_base)`. When stopped without `STOP_RESET`,
/// the current delta is folded back into `value`.
pub(crate) struct VirtualPmu<B = QemuPmuBackend>
where
    B: PmuBackend,
{
    /// Guest-visible counter state indexed by virtual counter number.
    ///
    /// Length is `FIXED_COUNTERS + FIRMWARE_COUNTERS + backend.num_hpm_counters()`
    /// and is fixed for the lifetime of this instance.
    counters: alloc::vec::Vec<VirtualPmuCounter>,
    /// Backend used as the source for physical or emulated PMU values.
    backend: B,
}

/// Backend boundary between guest-visible vPMU state and the underlying
/// hardware PMU source.
///
/// `VirtualPmu` owns the SBI PMU ABI, guest-visible counter lifecycle, and
/// per-vCPU virtual values. The backend supplies:
///
/// - reads from architectural fixed counter CSRs (`cycle`, `instret`),
/// - optional hardware performance counter (`hpmcounter3..31`) allocation,
///   programming, reading, and release,
/// - vCPU lifecycle hooks (`on_bind`, `on_unbind`) for saving/restoring
///   physical PMU state across vCPU context switches.
///
/// The default implementation (`QemuPmuBackend`) only reads local CSRs and
/// exposes no HPM counters. A board-specific implementation can override
/// every method to schedule real hardware counters with full isolation.
pub(crate) trait PmuBackend {
    /// Read the underlying source for the architectural `cycle` counter.
    fn read_cycle(&self) -> u64;

    /// Read the underlying source for the architectural `instret` counter.
    fn read_instret(&self) -> u64;

    /// Number of additional hardware PMU counters this backend can expose.
    ///
    /// These slots are appended to the virtual bank after the fixed and
    /// firmware counters. Returning zero means no hardware performance counters
    /// beyond `cycle` and `instret` are available. The value must remain
    /// constant for the lifetime of the backend.
    fn num_hpm_counters(&self) -> usize;

    /// Check whether the backend can count the given SBI event index.
    ///
    /// Used by the `SKIP_MATCH` configuration path to test event compatibility
    /// without performing an allocation. A backend returning `true` here must
    /// be able to produce an `allocate_hpm` success for the same event, subject
    /// only to counter availability.
    fn can_handle_event(&self, event_idx: usize) -> bool;

    /// Allocate a hardware performance counter for the given SBI event.
    ///
    /// On success, returns an opaque hardware slot identifier. The caller
    /// stores the slot in the virtual counter and releases it via `release_hpm`
    /// when the virtual counter is reset. Returns `None` if the event is
    /// unsupported or no physical counter is currently free.
    fn allocate_hpm(&self, event_idx: usize, event_data: u64) -> Option<usize>;

    /// Release a hardware performance counter previously allocated with
    /// `allocate_hpm`. The slot identifier must not be used after this call.
    fn release_hpm(&self, hw_slot: usize);

    /// Read the current value of an allocated hardware performance counter.
    fn read_hpm(&self, hw_slot: usize) -> u64;

    /// Called immediately after the owning vCPU is bound to a hart.
    ///
    /// A real-hardware backend should restore `mhpmevent*` and `hpmcounter*`
    /// state so the guest sees its own counters.
    fn on_bind(&self);

    /// Called immediately before the owning vCPU is unbound from a hart.
    ///
    /// A real-hardware backend should save `mhpmevent*` and `hpmcounter*`
    /// state so it can be restored by the next `on_bind` call.
    fn on_unbind(&self);
}

/// Default PMU backend used by the QEMU-oriented software vPMU.
///
/// Reads architectural fixed counter CSRs directly for use as delta sources.
/// Exposes no HPM counters. All lifecycle hooks are no-ops. Sufficient for
/// virtual-only counter tracking on QEMU and other software-emulated platforms.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct QemuPmuBackend;

impl PmuBackend for QemuPmuBackend {
    #[inline]
    fn read_cycle(&self) -> u64 {
        let value: usize;
        unsafe {
            core::arch::asm!("csrr {value}, cycle", value = out(reg) value);
        }
        value as u64
    }

    #[inline]
    fn read_instret(&self) -> u64 {
        let value: usize;
        unsafe {
            core::arch::asm!("csrr {value}, instret", value = out(reg) value);
        }
        value as u64
    }

    #[inline]
    fn num_hpm_counters(&self) -> usize {
        0
    }

    #[inline]
    fn can_handle_event(&self, _event_idx: usize) -> bool {
        false
    }

    #[inline]
    fn allocate_hpm(&self, _event_idx: usize, _event_data: u64) -> Option<usize> {
        None
    }

    #[inline]
    fn release_hpm(&self, _hw_slot: usize) {}

    #[inline]
    fn read_hpm(&self, _hw_slot: usize) -> u64 {
        0
    }

    #[inline]
    fn on_bind(&self) {}

    #[inline]
    fn on_unbind(&self) {}
}

/// Mutable state for one virtual PMU counter.
///
/// The fields are atomic because SBI calls and event recording hooks may be
/// reached through different execution paths. The current vCPU model normally
/// serializes access per vCPU, so relaxed ordering is sufficient: the atomics
/// provide race-free storage without imposing cross-counter synchronization
/// semantics that the virtual PMU does not require.
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
    /// Firmware counters only increment while this flag is set. Hardware-backed
    /// counters (fixed and HPM) use it to decide whether `hardware_base` should
    /// be applied to the stored virtual value.
    started: AtomicBool,
    /// Stored virtual counter value.
    ///
    /// For firmware counters this is the complete counter value. For
    /// hardware-backed counters this is the accumulated guest-visible value at
    /// the last clear, explicit initialization, or stop point; while running,
    /// the live hardware delta is added on top.
    value: AtomicU64,
    /// Hardware counter baseline captured at start time.
    ///
    /// Meaningful only for hardware-backed counters (`cycle`, `instret`, HPM)
    /// while `started` is true. Reset to zero when the counter is stopped or
    /// cleared.
    hardware_base: AtomicU64,
    /// Opaque hardware slot identifier allocated by the backend.
    ///
    /// Set to `HW_SLOT_NONE` for unconfigured counters, firmware counters
    /// (which are always purely virtual), and fixed counters (`cycle`,
    /// `instret`). For HPM counters it holds the slot returned by
    /// `PmuBackend::allocate_hpm` and is released via `release_hpm` on reset.
    hw_slot: AtomicUsize,
}

impl<B> VirtualPmu<B>
where
    B: PmuBackend,
{
    /// Number of fixed CSR-backed PMU counters (cycle and instret).
    const FIXED_COUNTERS: usize = 2;
    /// Number of virtual firmware counters in the static counter bank.
    const FIRMWARE_COUNTERS: usize = 11;
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
    /// Reported counter width encoded in SBI `COUNTER_GET_INFO`.
    ///
    /// SBI encodes the width as the most significant valid bit index rather
    /// than the number of bits. `usize::BITS - 1` matches the architectural CSR
    /// width used by this target.
    const COUNTER_WIDTH: usize = usize::BITS as usize - 1;
    /// CSR number for the architectural `cycle` counter.
    const CSR_CYCLE: usize = 0xc00;
    /// CSR number for the architectural `instret` counter.
    const CSR_INSTRET: usize = 0xc02;
    /// Base CSR number for hardware performance counters (`hpmcounter3`).
    const CSR_HPM_BASE: usize = 0xc03;
    /// SBI counter-info marker for a firmware counter.
    const FIRMWARE_COUNTER_TYPE: usize = 1 << (usize::BITS as usize - 1);
    /// Sentinel event index stored in counters that have not been configured.
    const UNCONFIGURED_EVENT: usize = usize::MAX;
    /// Sentinel `hw_slot` value meaning no hardware slot is currently allocated.
    const HW_SLOT_NONE: usize = usize::MAX;

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

    /// Build SBI `COUNTER_GET_INFO` metadata for a CSR-backed counter.
    ///
    /// The SBI return value combines the CSR number with the reported counter
    /// width. Both fixed PMU counters and HPM counters are advertised as direct
    /// CSR counters so a guest can discover the architectural CSR.
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
    /// counter beyond the live virtual bank.
    #[inline]
    fn validate_counter_set(&self, counter_idx_base: usize, counter_idx_mask: usize) -> SbiRet {
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
            if counter_idx >= self.counters.len() {
                return SbiRet::invalid_param();
            }
        }

        SbiRet::success(0)
    }

    /// Return the first virtual counter selected by an SBI counter set.
    ///
    /// Used for `SKIP_MATCH` where the guest selects the counter directly.
    #[inline]
    fn first_counter_in_set(
        &self,
        counter_idx_base: usize,
        counter_idx_mask: usize,
    ) -> Option<usize> {
        (0..self.counters.len()).find(|&counter_idx| {
            Self::counter_in_set(counter_idx, counter_idx_base, counter_idx_mask)
        })
    }

    /// Map an SBI PMU event to the fixed or firmware virtual counter for it.
    ///
    /// Returns the dedicated virtual counter index for events covered by the
    /// static counter bank (`cycle`, `instret`, and the eleven firmware events).
    /// Returns `None` for any other event, including hardware cache/raw/platform
    /// events that must be routed through the HPM backend path.
    #[inline]
    fn event_counter_static(event_idx: usize) -> Option<usize> {
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
    /// For fixed and firmware counters the check is a direct static-mapping
    /// lookup. For HPM virtual slots the backend is queried via
    /// `can_handle_event` without performing any allocation.
    #[inline]
    fn counter_supports_event(&self, counter_idx: usize, event_idx: usize) -> bool {
        if Self::event_counter_static(event_idx) == Some(counter_idx) {
            return true;
        }
        self.counter_is_hpm(counter_idx) && self.backend.can_handle_event(event_idx)
    }

    /// Return whether a counter is one of the virtual firmware counters.
    ///
    /// Firmware counters are read through `COUNTER_FW_READ` rather than direct
    /// CSR reads.
    #[inline]
    fn counter_is_firmware(counter_idx: usize) -> bool {
        let fw_end = Self::FIXED_COUNTERS + Self::FIRMWARE_COUNTERS;
        counter_idx >= Self::FIXED_COUNTERS && counter_idx < fw_end
    }

    /// Return whether a counter is an HPM virtual slot backed by the backend.
    #[inline]
    fn counter_is_hpm(&self, counter_idx: usize) -> bool {
        let hpm_start = Self::FIXED_COUNTERS + Self::FIRMWARE_COUNTERS;
        counter_idx >= hpm_start && counter_idx < self.counters.len()
    }

    /// Return whether a counter uses the hardware baseline/delta model.
    ///
    /// Both fixed counters and HPM counters capture `hardware_base` at start
    /// and accumulate a delta; only firmware counters use direct increment.
    #[inline]
    fn counter_is_hardware_backed(&self, counter_idx: usize) -> bool {
        !Self::counter_is_firmware(counter_idx)
    }

    /// Read the current hardware value for a hardware-backed counter.
    ///
    /// For fixed counters this reads the architectural CSR via the backend.
    /// For HPM counters it delegates to `backend.read_hpm`. Returns zero for
    /// firmware counter indices or HPM counters with no allocated slot.
    #[inline]
    fn hardware_value(&self, counter_idx: usize) -> u64 {
        match counter_idx {
            Self::CYCLE_COUNTER => self.backend.read_cycle(),
            Self::INSTRET_COUNTER => self.backend.read_instret(),
            idx if self.counter_is_hpm(idx) => {
                let hw_slot = self.counters[idx].hw_slot.load(Ordering::Relaxed);
                if hw_slot != Self::HW_SLOT_NONE {
                    self.backend.read_hpm(hw_slot)
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    /// Compute the guest-visible value for a hardware-backed counter.
    ///
    /// If the counter is stopped, the stored virtual value is already complete.
    /// If it is running, the live hardware delta since the last start is added
    /// on top. Wrapping arithmetic preserves architectural wraparound behavior.
    #[inline]
    fn hardware_virtual_value(&self, counter_idx: usize) -> u64 {
        let counter = &self.counters[counter_idx];
        let value = counter.value.load(Ordering::Relaxed);
        if !counter.started.load(Ordering::Relaxed) {
            return value;
        }

        let hardware_base = counter.hardware_base.load(Ordering::Relaxed);
        let hardware_now = self.hardware_value(counter_idx);
        value.wrapping_add(hardware_now.wrapping_sub(hardware_base))
    }

    /// Reset a counter to the unconfigured state.
    ///
    /// Releases any allocated HPM slot back to the backend, then clears all
    /// counter fields. This is the only code path that calls `release_hpm`.
    #[inline]
    fn reset_counter(&self, counter_idx: usize) {
        let counter = &self.counters[counter_idx];
        // Release any allocated HPM slot before clearing the rest of the state.
        let hw_slot = counter.hw_slot.swap(Self::HW_SLOT_NONE, Ordering::Relaxed);
        if hw_slot != Self::HW_SLOT_NONE {
            self.backend.release_hpm(hw_slot);
        }
        counter
            .event_idx
            .store(Self::UNCONFIGURED_EVENT, Ordering::Relaxed);
        counter.configured.store(false, Ordering::Relaxed);
        counter.started.store(false, Ordering::Relaxed);
        counter.value.store(0, Ordering::Relaxed);
        counter.hardware_base.store(0, Ordering::Relaxed);
    }
}

impl<B> Default for VirtualPmu<B>
where
    B: PmuBackend + Default,
{
    fn default() -> Self {
        Self::new(B::default())
    }
}

impl<B> VirtualPmu<B>
where
    B: PmuBackend,
{
    /// Create an empty virtual PMU counter bank with an explicit backend.
    ///
    /// All counters start unconfigured and stopped. Firmware counters begin at
    /// zero, and fixed PMU counters have no hardware baseline until they are
    /// started by the guest.
    pub(crate) fn new(backend: B) -> Self {
        let total = Self::FIXED_COUNTERS + Self::FIRMWARE_COUNTERS + backend.num_hpm_counters();
        let counters = (0..total)
            .map(|_| VirtualPmuCounter {
                event_idx: AtomicUsize::new(Self::UNCONFIGURED_EVENT),
                configured: AtomicBool::new(false),
                started: AtomicBool::new(false),
                value: AtomicU64::new(0),
                hardware_base: AtomicU64::new(0),
                hw_slot: AtomicUsize::new(Self::HW_SLOT_NONE),
            })
            .collect();

        Self { counters, backend }
    }

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

    /// Notify the backend that the owning vCPU is being bound to a hart.
    ///
    /// Must be called at the end of `AxArchVCpu::bind()` after all VS-mode
    /// CSRs have been restored. A real-hardware backend uses this hook to
    /// restore `mhpmevent*` and `hpmcounter*` state so the guest counter view
    /// is consistent on the newly scheduled hart.
    #[inline]
    pub(crate) fn backend_bind(&self) {
        self.backend.on_bind();
    }

    /// Notify the backend that the owning vCPU is being unbound from a hart.
    ///
    /// Must be called at the beginning of `AxArchVCpu::unbind()` before
    /// VS-mode CSRs are saved. A real-hardware backend uses this hook to save
    /// `mhpmevent*` and `hpmcounter*` state so it can be restored by the next
    /// `backend_bind` call.
    #[inline]
    pub(crate) fn backend_unbind(&self) {
        self.backend.on_unbind();
    }
}

impl<B> Pmu for VirtualPmu<B>
where
    B: PmuBackend,
{
    /// Return the number of virtual counters exposed by this PMU instance.
    ///
    /// The total is the number of counters in the live bank, which equals
    /// `FIXED_COUNTERS + FIRMWARE_COUNTERS + backend.num_hpm_counters()`.
    #[inline]
    fn num_counters(&self) -> usize {
        self.counters.len()
    }

    /// Return SBI metadata for a virtual counter.
    ///
    /// Fixed PMU counters and HPM counters are reported as CSR-backed counters
    /// with their architectural CSR numbers. Firmware counters are reported as
    /// firmware counters. Any index outside the virtual counter bank is
    /// rejected.
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
            idx if self.counter_is_hpm(idx) => {
                // Report the architectural `hpmcounterN` CSR for this HPM slot.
                let hpm_local = idx - (Self::FIXED_COUNTERS + Self::FIRMWARE_COUNTERS);
                SbiRet::success(Self::counter_info(Self::CSR_HPM_BASE + hpm_local))
            }
            _ => SbiRet::invalid_param(),
        }
    }

    /// Configure a virtual counter for an SBI PMU event.
    ///
    /// For fixed and firmware events the dedicated static virtual slot is used.
    /// For all other events the backend is asked to allocate a hardware slot
    /// and a free HPM virtual slot is assigned. `SKIP_MATCH` forces selection
    /// of a specific counter without the event-to-slot search; for HPM slots
    /// the backend's `can_handle_event` is checked first, then an HPM slot is
    /// allocated.
    ///
    /// `CLEAR_VALUE` resets the stored virtual value. `AUTO_START` starts the
    /// counter immediately and captures a hardware baseline. Running counters
    /// are rejected with `SBI_ERR_ALREADY_STARTED`.
    #[inline]
    fn counter_config_matching(
        &self,
        counter_idx_base: usize,
        counter_idx_mask: usize,
        config_flags: usize,
        event_idx: usize,
        event_data: u64,
    ) -> SbiRet {
        if (config_flags & !Self::CFG_VALID_FLAGS) != 0 {
            return SbiRet::invalid_param();
        }
        let ret = self.validate_counter_set(counter_idx_base, counter_idx_mask);
        if ret.is_err() {
            return ret;
        }

        let counter_idx = if (config_flags & Self::CFG_FLAG_SKIP_MATCH) != 0 {
            let Some(idx) = self.first_counter_in_set(counter_idx_base, counter_idx_mask) else {
                return SbiRet::invalid_param();
            };
            if !self.counter_supports_event(idx, event_idx) {
                return SbiRet::not_supported();
            }
            // For HPM slots via SKIP_MATCH: check not-started before allocating
            // to avoid a needless backend roundtrip.
            if self.counter_is_hpm(idx) {
                if self.counters[idx].started.load(Ordering::Relaxed) {
                    return SbiRet::already_started();
                }
                // Release any existing hw_slot (reconfiguration of a stopped
                // HPM counter for a different event).
                let old = self.counters[idx]
                    .hw_slot
                    .swap(Self::HW_SLOT_NONE, Ordering::Relaxed);
                if old != Self::HW_SLOT_NONE {
                    self.backend.release_hpm(old);
                }
                match self.backend.allocate_hpm(event_idx, event_data) {
                    Some(hw_slot) => {
                        self.counters[idx].hw_slot.store(hw_slot, Ordering::Relaxed);
                    }
                    None => return SbiRet::not_supported(),
                }
            }
            idx
        } else {
            // Try the static event→slot mapping first (cycle, instret, firmware).
            if let Some(idx) = Self::event_counter_static(event_idx) {
                if !Self::counter_in_set(idx, counter_idx_base, counter_idx_mask) {
                    return SbiRet::not_supported();
                }
                idx
            } else {
                // Route to an HPM virtual slot via backend allocation.
                let hw_slot = match self.backend.allocate_hpm(event_idx, event_data) {
                    Some(s) => s,
                    None => return SbiRet::not_supported(),
                };
                let hpm_start = Self::FIXED_COUNTERS + Self::FIRMWARE_COUNTERS;
                let mut found = None;
                for idx in hpm_start..self.counters.len() {
                    if !Self::counter_in_set(idx, counter_idx_base, counter_idx_mask) {
                        continue;
                    }
                    if !self.counters[idx].configured.load(Ordering::Relaxed) {
                        self.counters[idx].hw_slot.store(hw_slot, Ordering::Relaxed);
                        found = Some(idx);
                        break;
                    }
                }
                match found {
                    Some(idx) => idx,
                    None => {
                        self.backend.release_hpm(hw_slot);
                        return SbiRet::not_supported();
                    }
                }
            }
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
            if self.counter_is_hardware_backed(counter_idx) {
                counter
                    .hardware_base
                    .store(self.hardware_value(counter_idx), Ordering::Relaxed);
            }
            counter.started.store(true, Ordering::Relaxed);
        }
        SbiRet::success(counter_idx)
    }

    /// Start one or more configured virtual counters.
    ///
    /// All selected counters are validated before any counter is modified. If
    /// one selected counter is unconfigured or already running, no counter is
    /// started. When `START_SET_INIT_VALUE` is present, each counter's stored
    /// virtual value is replaced with `initial_value` before counting begins.
    /// Hardware-backed counters (fixed and HPM) capture a fresh baseline.
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
        let ret = self.validate_counter_set(counter_idx_base, counter_idx_mask);
        if ret.is_err() {
            return ret;
        }
        for counter_idx in 0..self.counters.len() {
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

        for counter_idx in 0..self.counters.len() {
            if Self::counter_in_set(counter_idx, counter_idx_base, counter_idx_mask) {
                if (start_flags & Self::START_FLAG_SET_INIT_VALUE) != 0 {
                    self.counters[counter_idx]
                        .value
                        .store(initial_value, Ordering::Relaxed);
                }
                if self.counter_is_hardware_backed(counter_idx) {
                    self.counters[counter_idx]
                        .hardware_base
                        .store(self.hardware_value(counter_idx), Ordering::Relaxed);
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
    /// Validation is performed for the entire selected set before mutating any
    /// counter. Stopping a hardware-backed counter without `STOP_RESET` folds
    /// the live delta into the stored virtual value. With `STOP_RESET` the
    /// counter is returned to the unconfigured state directly: the fold is
    /// skipped and any HPM slot is released via `reset_counter`.
    ///
    /// Returns `SBI_ERR_INVALID_PARAM` for never-configured counters to
    /// distinguish stop-before-configure lifecycle errors from the normal
    /// already-stopped case.
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
        let ret = self.validate_counter_set(counter_idx_base, counter_idx_mask);
        if ret.is_err() {
            return ret;
        }

        for counter_idx in 0..self.counters.len() {
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

        for counter_idx in 0..self.counters.len() {
            if !Self::counter_in_set(counter_idx, counter_idx_base, counter_idx_mask) {
                continue;
            }

            let counter = &self.counters[counter_idx];
            if (stop_flags & Self::STOP_FLAG_RESET) != 0 {
                // reset_counter releases any HPM slot and clears all fields.
                self.reset_counter(counter_idx);
                continue;
            }
            if self.counter_is_hardware_backed(counter_idx) {
                counter
                    .value
                    .store(self.hardware_virtual_value(counter_idx), Ordering::Relaxed);
                counter.hardware_base.store(0, Ordering::Relaxed);
            }
            counter.started.store(false, Ordering::Relaxed);
        }

        SbiRet::success(0)
    }

    /// Read the low word of a configured firmware counter.
    ///
    /// SBI PMU exposes firmware-counter values through explicit read calls
    /// rather than direct CSR access. This method rejects fixed PMU counters,
    /// HPM counters, and unconfigured firmware counters.
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
