//! PCI node view specialization.

use core::ops::{Deref, Range};

use alloc::vec::Vec;
use fdt_raw::{FdtError, Phandle};

use super::NodeView;
use crate::{NodeGeneric, NodeGenericMut, Property, ViewMutOp, ViewOp};

// ---------------------------------------------------------------------------
// PCI types
// ---------------------------------------------------------------------------

/// PCI address space types.
#[derive(Clone, Debug, PartialEq)]
pub enum PciSpace {
    /// I/O space
    IO,
    /// 32-bit memory space
    Memory32,
    /// 64-bit memory space
    Memory64,
}

/// PCI address range entry.
///
/// Represents a range of addresses in PCI address space with mapping to CPU address space.
#[derive(Clone, Debug, PartialEq)]
pub struct PciRange {
    /// The PCI address space type
    pub space: PciSpace,
    /// Address on the PCI bus
    pub bus_address: u64,
    /// Address in CPU physical address space
    pub cpu_address: u64,
    /// Size of the range in bytes
    pub size: u64,
    /// Whether the memory region is prefetchable
    pub prefetchable: bool,
}

/// PCI interrupt mapping entry.
///
/// Represents a mapping from PCI device interrupts to parent interrupt controller inputs.
#[derive(Clone, Debug)]
pub struct PciInterruptMap {
    /// Child device address (masked)
    pub child_address: Vec<u32>,
    /// Child device IRQ (masked)
    pub child_irq: Vec<u32>,
    /// Phandle of the interrupt parent controller
    pub interrupt_parent: Phandle,
    /// Parent controller IRQ inputs
    pub parent_irq: Vec<u32>,
}

/// PCI interrupt information.
///
/// Contains the resolved interrupt information for a PCI device.
#[derive(Clone, Debug, PartialEq)]
pub struct PciInterruptInfo {
    /// List of IRQ numbers
    pub irqs: Vec<u32>,
}

// ---------------------------------------------------------------------------
// PciNodeView
// ---------------------------------------------------------------------------

/// Specialized view for PCI bridge nodes.
#[derive(Clone, Copy)]
pub struct PciNodeView<'a> {
    pub(super) inner: NodeGeneric<'a>,
}

impl<'a> Deref for PciNodeView<'a> {
    type Target = NodeGeneric<'a>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a> ViewOp<'a> for PciNodeView<'a> {
    fn as_view(&self) -> NodeView<'a> {
        self.inner.as_view()
    }
}

impl<'a> PciNodeView<'a> {
    pub(crate) fn try_from_view(view: NodeView<'a>) -> Option<Self> {
        if view.as_node().is_pci() {
            Some(Self {
                inner: NodeGeneric { inner: view },
            })
        } else {
            None
        }
    }

    /// Returns the `#interrupt-cells` property value.
    ///
    /// Defaults to 1 for PCI devices if not specified.
    pub fn interrupt_cells(&self) -> u32 {
        self.as_view()
            .as_node()
            .get_property("#interrupt-cells")
            .and_then(|prop| prop.get_u32())
            .unwrap_or(1)
    }

    /// Get the interrupt-map-mask property if present.
    pub fn interrupt_map_mask(&self) -> Option<Vec<u32>> {
        self.as_view()
            .as_node()
            .get_property("interrupt-map-mask")
            .map(|prop| prop.get_u32_iter().collect())
    }

    /// Get the bus range property if present.
    pub fn bus_range(&self) -> Option<Range<u32>> {
        self.as_view()
            .as_node()
            .get_property("bus-range")
            .and_then(|prop| {
                let mut iter = prop.get_u32_iter();
                let start = iter.next()?;
                let end = iter.next()?;
                Some(start..end)
            })
    }

    /// Decode PCI address space from the high cell of PCI address.
    ///
    /// PCI address high cell format:
    /// - Bits 31-28: 1 for IO space, 2 for Memory32, 3 for Memory64
    /// - Bit 30: Prefetchable for memory spaces
    fn decode_pci_address_space(&self, pci_hi: u32) -> (PciSpace, bool) {
        let space_code = (pci_hi >> 24) & 0x03;
        let prefetchable = (pci_hi >> 30) & 0x01 == 1;

        let space = match space_code {
            1 => PciSpace::IO,
            2 => PciSpace::Memory32,
            3 => PciSpace::Memory64,
            _ => PciSpace::Memory32,
        };

        (space, prefetchable)
    }

    /// Get the ranges property for address translation.
    pub fn ranges(&self) -> Option<Vec<PciRange>> {
        let prop = self.as_view().as_node().get_property("ranges")?;
        let mut data = prop.as_reader();
        let mut ranges = Vec::new();

        // PCI ranges format: <child-bus-address parent-bus-address size>
        // child-bus-address: 3 cells (pci.hi pci.mid pci.lo) - PCI 地址固定 3 cells
        // parent-bus-address: 使用父节点的 #address-cells
        // size: 使用当前节点的 #size-cells

        // Get parent's address-cells
        let parent_addr_cells = if let Some(parent) = self.as_view().parent() {
            parent.as_view().address_cells().unwrap_or(2) as usize
        } else {
            2_usize
        };

        let size_cells = self.as_view().size_cells().unwrap_or(2) as usize;

        while let Some(pci_hi) = data.read_u32() {
            // Parse child bus address (3 cells for PCI: phys.hi, phys.mid, phys.lo)
            // pci_hi 用于解析地址空间类型，bus_address 由 pci_mid 和 pci_lo 组成
            let pci_mid = data.read_u32()?;
            let pci_lo = data.read_u32()?;
            let bus_address = ((pci_mid as u64) << 32) | (pci_lo as u64);

            // Parse parent bus address (使用父节点的 #address-cells)
            let mut parent_addr = 0u64;
            for _ in 0..parent_addr_cells {
                let cell = data.read_u32()? as u64;
                parent_addr = (parent_addr << 32) | cell;
            }

            // Parse size (使用当前节点的 #size-cells)
            let mut size = 0u64;
            for _ in 0..size_cells {
                let cell = data.read_u32()? as u64;
                size = (size << 32) | cell;
            }

            // Extract PCI address space and prefetchable from child_addr[0]
            let (space, prefetchable) = self.decode_pci_address_space(pci_hi);

            ranges.push(PciRange {
                space,
                bus_address,
                cpu_address: parent_addr,
                size,
                prefetchable,
            });
        }

        Some(ranges)
    }

    /// 解析 interrupt-map 属性
    pub fn interrupt_map(&self) -> Result<Vec<PciInterruptMap>, FdtError> {
        let prop = self
            .as_view()
            .as_node()
            .get_property("interrupt-map")
            .ok_or(FdtError::NotFound)?;

        // 将 mask 转换为 Vec 以便索引访问
        let mask: Vec<u32> = self
            .interrupt_map_mask()
            .ok_or(FdtError::NotFound)?
            .into_iter()
            .collect();

        let mut data = prop.as_reader();
        let mut mappings = Vec::new();

        // 计算每个条目的大小
        // 格式: <child-address child-irq interrupt-parent parent-address parent-irq...>
        let child_addr_cells = self.as_view().address_cells().unwrap_or(3) as usize;
        let child_irq_cells = self.interrupt_cells() as usize;

        loop {
            // 解析子地址
            let mut child_address = Vec::with_capacity(child_addr_cells);
            for _ in 0..child_addr_cells {
                match data.read_u32() {
                    Some(v) => child_address.push(v),
                    None => return Ok(mappings), // 数据结束
                }
            }

            // 解析子 IRQ
            let mut child_irq = Vec::with_capacity(child_irq_cells);
            for _ in 0..child_irq_cells {
                match data.read_u32() {
                    Some(v) => child_irq.push(v),
                    None => return Ok(mappings),
                }
            }

            // 解析中断父 phandle
            let interrupt_parent_raw = match data.read_u32() {
                Some(v) => v,
                None => return Ok(mappings),
            };
            let interrupt_parent = Phandle::from(interrupt_parent_raw);

            // 通过 phandle 查找中断父节点以获取其 #address-cells 和 #interrupt-cells
            // 根据 devicetree 规范，interrupt-map 中的 parent unit address 使用中断父节点的 #address-cells
            let (parent_addr_cells, parent_irq_cells) =
                if let Some(irq_parent) = self.as_view().fdt().get_by_phandle(interrupt_parent) {
                    // 直接使用中断父节点的 #address-cells
                    let addr_cells = irq_parent.as_view().address_cells().unwrap_or(0) as usize;

                    let irq_cells = irq_parent
                        .as_view()
                        .as_node()
                        .get_property("#interrupt-cells")
                        .and_then(|p| p.get_u32())
                        .unwrap_or(3) as usize;
                    (addr_cells, irq_cells)
                } else {
                    // 默认值：address_cells=0, interrupt_cells=3 (GIC 格式)
                    (0, 3)
                };

            // 跳过父地址 cells
            for _ in 0..parent_addr_cells {
                if data.read_u32().is_none() {
                    return Ok(mappings);
                }
            }

            // 解析父 IRQ
            let mut parent_irq = Vec::with_capacity(parent_irq_cells);
            for _ in 0..parent_irq_cells {
                match data.read_u32() {
                    Some(v) => parent_irq.push(v),
                    None => return Ok(mappings),
                }
            }

            // 应用 mask 到子地址和 IRQ
            let masked_address: Vec<u32> = child_address
                .iter()
                .enumerate()
                .map(|(i, value)| {
                    let mask_value = mask.get(i).copied().unwrap_or(0xffff_ffff);
                    value & mask_value
                })
                .collect();
            let masked_irq: Vec<u32> = child_irq
                .iter()
                .enumerate()
                .map(|(i, value)| {
                    let mask_value = mask
                        .get(child_addr_cells + i)
                        .copied()
                        .unwrap_or(0xffff_ffff);
                    value & mask_value
                })
                .collect();

            mappings.push(PciInterruptMap {
                child_address: masked_address,
                child_irq: masked_irq,
                interrupt_parent,
                parent_irq,
            });
        }
    }

    /// 获取 PCI 设备的中断信息
    /// 参数: bus, device, function, pin (1=INTA, 2=INTB, 3=INTC, 4=INTD)
    pub fn child_interrupts(
        &self,
        bus: u8,
        device: u8,
        function: u8,
        interrupt_pin: u8,
    ) -> Result<PciInterruptInfo, FdtError> {
        // 获取 interrupt-map 和 mask
        let interrupt_map = self.interrupt_map()?;

        // 将 mask 转换为 Vec 以便索引访问
        let mask: Vec<u32> = self
            .interrupt_map_mask()
            .ok_or(FdtError::NotFound)?
            .into_iter()
            .collect();

        // 构造 PCI 设备的子地址
        // 格式: [bus_num, device_num, func_num] 在适当的位
        let child_addr_high = ((bus as u32 & 0xff) << 16)
            | ((device as u32 & 0x1f) << 11)
            | ((function as u32 & 0x07) << 8);
        let child_addr_mid = 0u32;
        let child_addr_low = 0u32;

        let child_addr_cells = self.as_view().address_cells().unwrap_or(3) as usize;
        let child_irq_cells = self.interrupt_cells() as usize;

        let encoded_address = [child_addr_high, child_addr_mid, child_addr_low];
        let mut masked_child_address = Vec::with_capacity(child_addr_cells);

        // 应用 mask 到子地址
        for (idx, value) in encoded_address.iter().take(child_addr_cells).enumerate() {
            let mask_value = mask.get(idx).copied().unwrap_or(0xffff_ffff);
            masked_child_address.push(value & mask_value);
        }

        // 如果 encoded_address 比 child_addr_cells 短，填充 0
        let remaining = child_addr_cells.saturating_sub(encoded_address.len());
        masked_child_address.extend(core::iter::repeat_n(0, remaining));

        let encoded_irq = [interrupt_pin as u32];
        let mut masked_child_irq = Vec::with_capacity(child_irq_cells);

        // 应用 mask 到子 IRQ
        for (idx, value) in encoded_irq.iter().take(child_irq_cells).enumerate() {
            let mask_value = mask
                .get(child_addr_cells + idx)
                .copied()
                .unwrap_or(0xffff_ffff);
            masked_child_irq.push(value & mask_value);
        }

        // 如果 encoded_irq 比 child_irq_cells 短，填充 0
        let remaining_irq = child_irq_cells.saturating_sub(encoded_irq.len());
        masked_child_irq.extend(core::iter::repeat_n(0, remaining_irq));

        // 在 interrupt-map 中查找匹配的条目
        for mapping in &interrupt_map {
            if mapping.child_address == masked_child_address
                && mapping.child_irq == masked_child_irq
            {
                return Ok(PciInterruptInfo {
                    irqs: mapping.parent_irq.clone(),
                });
            }
        }

        // 回退到简单的 IRQ 计算
        let simple_irq = (device as u32 * 4 + interrupt_pin as u32) % 32;
        Ok(PciInterruptInfo {
            irqs: vec![simple_irq],
        })
    }
}

// ---------------------------------------------------------------------------
// PciNodeViewMut
// ---------------------------------------------------------------------------

/// Mutable view for PCI bridge nodes.
pub struct PciNodeViewMut<'a> {
    pub(super) inner: NodeGenericMut<'a>,
}

impl<'a> Deref for PciNodeViewMut<'a> {
    type Target = NodeGenericMut<'a>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<'a> ViewOp<'a> for PciNodeViewMut<'a> {
    fn as_view(&self) -> NodeView<'a> {
        self.inner.as_view()
    }
}

impl<'a> ViewMutOp<'a> for PciNodeViewMut<'a> {
    fn new(node: NodeGenericMut<'a>) -> Self {
        let mut s = Self { inner: node };
        let n = s.inner.inner.as_node_mut();
        // Set PCI-specific properties
        n.set_property(Property::new("device_type", b"pci\0".to_vec()));
        // PCI uses #address-cells = 3, #size-cells = 2
        n.set_property(Property::new(
            "#address-cells",
            (3u32).to_be_bytes().to_vec(),
        ));
        n.set_property(Property::new("#size-cells", (2u32).to_be_bytes().to_vec()));
        n.set_property(Property::new(
            "#interrupt-cells",
            (1u32).to_be_bytes().to_vec(),
        ));
        s
    }
}

impl<'a> PciNodeViewMut<'a> {
    pub(crate) fn try_from_view(view: NodeView<'a>) -> Option<Self> {
        if view.as_node().is_pci() {
            Some(Self {
                inner: NodeGenericMut { inner: view },
            })
        } else {
            None
        }
    }
}
