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

//! Interrupt routing model — translates guest-visible interrupt lines through
//! controllers to target vCPUs.
//!
//! # Motivation
//!
//! Real hardware has multiple levels of interrupt routing:
//!
//! ```text
//!   Device → GSIs (IOAPIC) → vCPU (LAPIC)
//!   Device → SPI/PPI (GIC) → vCPU (GIC CPU interface)
//!   Device → MSI (PCI) → IOAPIC or directly to vCPU
//! ```
//!
//! The `IrqRoutingTable` captures this topology at VM-config time, so device
//! emulators only need to declare their interrupt lines — they don't know or
//! care about the controller topology.
//!
//! # Usage

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Display;
use core::ops::Range;

use crate::r#trait::*;

// ============================================================
// Trigger mode
// ============================================================

/// How an interrupt is triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerMode {
    /// Edge-triggered (MSI, legacy PCI INTx# edge).
    Edge,
    /// Level-triggered, active high or low.
    Level { high: bool },
}

impl Display for TriggerMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Edge => write!(f, "edge"),
            Self::Level { high: true } => write!(f, "level-high"),
            Self::Level { high: false } => write!(f, "level-low"),
        }
    }
}

// ============================================================
// Interrupt message (what a device emits)
// ============================================================

/// The signal a device sends when it needs attention from the guest.
///
/// - **Legacy**: line-based, goes through a controller's pin (GIC SPI, IOAPIC).
/// - **Msi**: message-signaled, carries address+data (PCI MSI/MSI-x).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IrqMessage {
    /// Line-based interrupt.
    Legacy {
        /// Guest-visible line number (GSI on x86, SPI/PPI number on ARM, etc.).
        line: IrqLine,
    },
    /// Message-signaled interrupt (PCI MSI/MSI-x).
    Msi {
        /// MSI address (typically 0xfeeX_XXXX on x86).
        addr: u64,
        /// MSI data (vector + delivery mode).
        data: u32,
    },
}

impl IrqMessage {
    /// Build a legacy message from a line number.
    pub fn leg(line: impl Into<IrqLine>) -> Self {
        Self::Legacy { line: line.into() }
    }

    /// Build an MSI message from address and data.
    pub fn msi(addr: u64, data: u32) -> Self {
        Self::Msi { addr, data }
    }

    /// Return the `IrqLine` if this is a legacy message.
    pub fn as_line(&self) -> Option<IrqLine> {
        match self {
            Self::Legacy { line } => Some(*line),
            Self::Msi { .. } => None,
        }
    }
}

// ============================================================
// Routing entry
// ============================================================

/// A single entry in the interrupt routing table.
///
/// Maps a guest-visible interrupt source to a controller device + pin.
#[derive(Debug, Clone)]
pub struct IrqRoutingEntry {
    /// Human-readable name for diagnostics.
    pub name: String,
    /// The controller device that handles this interrupt.
    pub controller: DeviceId,
    /// Pin/IRQ number on the controller side.
    pub controller_pin: u32,
    /// Trigger mode.
    pub trigger: TriggerMode,
    /// Target vCPU(s) — `None` = let the controller decide.
    pub target: Option<IrqTarget>,
}

// ============================================================
// Routing table
// ============================================================

/// Maps interrupt sources to controller devices.
///
/// Two lookup paths:
/// - **Legacy**: `IrqLine → controller + pin`
/// - **MSI**: `address → controller` (via MSI address window)
///
/// Populated once at VM creation, read-only at runtime (no lock needed).
pub struct IrqRoutingTable {
    /// Legacy line → entry index.
    legacy_map: BTreeMap<IrqLine, usize>,
    /// MSI address range → controller DeviceId.
    msi_windows: Vec<MsiWindow>,
    /// All entries.
    entries: Vec<IrqRoutingEntry>,
}

/// A window of MSI addresses mapped to a controller.
#[derive(Debug, Clone)]
struct MsiWindow {
    range: Range<u64>,
    controller: DeviceId,
}

impl IrqRoutingTable {
    /// Create an empty routing table.
    pub fn new() -> Self {
        Self {
            legacy_map: BTreeMap::new(),
            msi_windows: Vec::new(),
            entries: Vec::new(),
        }
    }

    /// Add a legacy line → controller mapping.
    pub fn add_legacy(
        &mut self,
        line: IrqLine,
        controller: DeviceId,
        controller_pin: u32,
        trigger: TriggerMode,
        target: Option<IrqTarget>,
        name: impl Into<String>,
    ) -> &mut Self {
        let entry = IrqRoutingEntry {
            name: name.into(),
            controller,
            controller_pin,
            trigger,
            target,
        };
        let idx = self.entries.len();
        self.entries.push(entry);
        self.legacy_map.insert(line, idx);
        self
    }

    /// Add an MSI address window → controller mapping.
    pub fn add_msi_range(
        &mut self,
        range: Range<u64>,
        controller: DeviceId,
    ) -> &mut Self {
        self.msi_windows.push(MsiWindow { range, controller });
        self
    }

    /// Look up a legacy (line-based) interrupt in the routing table.
    pub fn lookup_legacy(&self, line: IrqLine) -> Option<(DeviceId, &IrqRoutingEntry)> {
        let idx = self.legacy_map.get(&line)?;
        let entry = &self.entries[*idx];
        Some((entry.controller, entry))
    }

    /// Look up an MSI interrupt by address window.
    pub fn lookup_msi(&self, addr: u64) -> Option<DeviceId> {
        self.msi_windows
            .iter()
            .find(|w| w.range.contains(&addr))
            .map(|w| w.controller)
    }

    /// Look up by message (delegates to `lookup_legacy`; MSI returns `None`).
    /// Prefer `lookup_legacy()` / `lookup_msi()` for clarity.
    pub fn lookup(&self, msg: &IrqMessage) -> Option<(DeviceId, &IrqRoutingEntry)> {
        match msg {
            IrqMessage::Legacy { line } => self.lookup_legacy(*line),
            IrqMessage::Msi { .. } => None,
        }
    }

    /// Iterate over all routing entries.
    pub fn entries(&self) -> &[IrqRoutingEntry] {
        &self.entries
    }

    /// Number of legacy routes.
    pub fn legacy_count(&self) -> usize {
        self.legacy_map.len()
    }

    /// Number of MSI windows.
    pub fn msi_window_count(&self) -> usize {
        self.msi_windows.len()
    }
}

// ============================================================
// Enhanced InterruptControllerOps
// ============================================================

/// Extended operations for an interrupt controller device.
///
/// Each architecture provides its own implementation:
/// - **aarch64**: `Vgic` (wraps GICv2/v3 emulation)
/// - **x86_64**: `Ioapic` + `LocalApic` (two controllers, chained)
/// - **riscv64**: `VPlicGlobal` (PLIC) + AIA IMSIC (upcoming)
/// - **loongarch64**: `LoongArchPchPIC` + `LoongArchExtIOC` (two controllers)
pub trait InterruptControllerOps: Send + Sync {
    /// Inject an interrupt on the given controller pin.
    fn inject_irq(&self, pin: u32, trigger: TriggerMode, target: Option<IrqTarget>) -> Result<()>;

    /// De-assert a level-triggered interrupt.
    fn deactivate_irq(&self, pin: u32) -> Result<()>;

    /// Handle an MSI write (address → controller, controller decodes the message).
    /// Returns `None` if this controller doesn't handle MSI at the given address.
    fn handle_msi(&self, addr: u64, data: u32) -> Result<()> {
        let _ = addr;
        let _ = data;
        Err(DeviceError::NotFound)
    }
}

// ============================================================
// IrqSink — device-side interrupt handle
// ============================================================

/// A lightweight, clonable handle that device backends hold to signal
/// interrupts without knowing the architecture or controller topology.
///
/// Created by [`BusRouter::create_irq_sink`] after the routing table is
/// populated. The sink captures the inject/deactivate callbacks as closures,
/// so the device never needs a reference to the router or the VM.
#[derive(Clone)]
pub struct IrqSink {
    line: IrqLine,
    trigger: TriggerMode,
    injector: Arc<dyn Fn(IrqMessage) -> Result<()> + Send + Sync>,
    deactivator: Arc<dyn Fn(IrqLine) -> Result<()> + Send + Sync>,
}

impl IrqSink {
    /// Create a new IrqSink with explicit callbacks.
    pub fn new(
        line: IrqLine,
        trigger: TriggerMode,
        injector: Arc<dyn Fn(IrqMessage) -> Result<()> + Send + Sync>,
        deactivator: Arc<dyn Fn(IrqLine) -> Result<()> + Send + Sync>,
    ) -> Self {
        Self { line, trigger, injector, deactivator }
    }

    /// Assert the interrupt (edge: pulse, level: raise).
    pub fn raise(&self) -> Result<()> {
        (self.injector)(IrqMessage::Legacy { line: self.line })
    }

    /// De-assert a level-triggered interrupt.
    pub fn lower(&self) -> Result<()> {
        (self.deactivator)(self.line)
    }

    /// The interrupt line this sink is bound to.
    pub fn line(&self) -> IrqLine {
        self.line
    }

    /// The trigger mode configured for this line.
    pub fn trigger(&self) -> TriggerMode {
        self.trigger
    }
}

impl core::fmt::Debug for IrqSink {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("IrqSink")
            .field("line", &self.line)
            .field("trigger", &self.trigger)
            .finish()
    }
}

// ============================================================
// Resource builder for devices
// ============================================================

/// Builder for attaching interrupt resources to a device.
///

pub struct InterruptBuilder {
    name: String,
    lines: Vec<IrqLine>,
}

impl InterruptBuilder {
    /// Start building interrupt resources for a named device.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            lines: Vec::new(),
        }
    }

    /// Add an interrupt line.
    pub fn irq(mut self, line: IrqLine) -> Self {
        self.lines.push(line);
        self
    }

    /// Build the resource list (interrupts only). Add alongside MMIO/PIO resources.
    pub fn build(self) -> Vec<Resource> {
        self.lines
            .into_iter()
            .map(|line| Resource::Irq(line))
            .collect()
    }

    /// Get the device name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(missing_docs, dead_code)]
mod tests {
    use super::*;

    fn d42() -> DeviceId {
        DeviceId(42)
    }
    fn d7() -> DeviceId {
        DeviceId(7)
    }

    #[test]
    fn test_legacy_route() {
        let mut table = IrqRoutingTable::new();
        table.add_legacy(IrqLine(33), d42(), 0, TriggerMode::Edge, None, "uart0");

        let (ctrl, entry) = table.lookup_legacy(IrqLine(33)).unwrap();
        assert_eq!(ctrl, d42());
        assert_eq!(entry.controller_pin, 0);
        assert_eq!(entry.trigger, TriggerMode::Edge);
    }

    #[test]
    fn test_route_unknown_line() {
        let table = IrqRoutingTable::new();
        assert!(table.lookup_legacy(IrqLine(99)).is_none());
    }

    #[test]
    fn test_msi_window() {
        let mut table = IrqRoutingTable::new();
        table.add_msi_range(0xfee0_0000..0xfee1_0000, d42());

        assert_eq!(table.lookup_msi(0xfee0_1234), Some(d42()));
        assert_eq!(table.lookup_msi(0xfee1_0000), None); // window end is exclusive
        assert_eq!(table.lookup_msi(0x1000), None);
    }

    #[test]
    fn test_multiple_lines() {
        let mut table = IrqRoutingTable::new();
        table
            .add_legacy(IrqLine(33), d42(), 0, TriggerMode::Edge, None, "dev1")
            .add_legacy(IrqLine(34), d42(), 1, TriggerMode::Level { high: true }, None, "dev2")
            .add_legacy(IrqLine(50), d7(), 0, TriggerMode::Edge, None, "dev3");

        assert_eq!(table.legacy_count(), 3);

        let (ctrl, entry) = table.lookup_legacy(IrqLine(34)).unwrap();
        assert_eq!(ctrl, d42());
        assert_eq!(entry.controller_pin, 1);
        assert_eq!(entry.trigger, TriggerMode::Level { high: true });
    }

    #[test]
    fn test_interrupt_builder() {
        let res = InterruptBuilder::new("uart0")
            .irq(IrqLine(33))
            .build();
        assert_eq!(res.len(), 1);
        if let Resource::Irq(line) = &res[0] {
            assert_eq!(*line, IrqLine(33));
        } else {
            panic!("expected Irq resource");
        }
    }

    #[test]
    fn test_irq_sink_raise_lower() {
        use alloc::sync::Arc;
        use std::sync::Mutex;

        let raised = Arc::new(Mutex::new(Vec::<IrqMessage>::new()));
        let lowered = Arc::new(Mutex::new(Vec::<IrqLine>::new()));

        let r = raised.clone();
        let l = lowered.clone();

        let sink = IrqSink::new(
            IrqLine(5),
            TriggerMode::Level { high: true },
            Arc::new(move |msg| { r.lock().unwrap().push(msg); Ok(()) }),
            Arc::new(move |line| { l.lock().unwrap().push(line); Ok(()) }),
        );

        assert_eq!(sink.line(), IrqLine(5));
        assert_eq!(sink.trigger(), TriggerMode::Level { high: true });

        sink.raise().unwrap();
        sink.raise().unwrap();
        sink.lower().unwrap();

        let raised = raised.lock().unwrap();
        assert_eq!(raised.len(), 2);
        assert!(matches!(raised[0], IrqMessage::Legacy { line: IrqLine(5) }));

        let lowered = lowered.lock().unwrap();
        assert_eq!(lowered.len(), 1);
        assert_eq!(lowered[0], IrqLine(5));
    }

    #[test]
    fn test_irq_sink_clone() {
        use alloc::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        let count = Arc::new(AtomicU32::new(0));
        let c = count.clone();

        let sink = IrqSink::new(
            IrqLine(10),
            TriggerMode::Edge,
            Arc::new(move |_| { c.fetch_add(1, Ordering::Relaxed); Ok(()) }),
            Arc::new(|_| Ok(())),
        );

        let sink2 = sink.clone();
        sink.raise().unwrap();
        sink2.raise().unwrap();

        assert_eq!(count.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_irq_sink_debug() {
        use alloc::sync::Arc;
        let sink = IrqSink::new(
            IrqLine(7),
            TriggerMode::Edge,
            Arc::new(|_| Ok(())),
            Arc::new(|_| Ok(())),
        );
        let dbg = format!("{sink:?}");
        assert!(dbg.contains("IrqSink"));
        assert!(dbg.contains("7"));
    }
}
