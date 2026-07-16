use alloc::{
    collections::{BTreeMap, BTreeSet},
    format,
    rc::Rc,
    string::{String, ToString},
    vec::Vec,
};
use core::{ptr::NonNull, str::FromStr};

use acpi::{
    AcpiError, AcpiTables, Handler, PhysicalMapping,
    address::{AddressSpace, GenericAddress},
    aml::{
        AmlError, Interpreter,
        namespace::{AmlName, NamespaceLevelKind},
        object::{FieldUnit, FieldUnitKind, FieldUpdateRule, Object, ObjectType},
        op_region::{OpRegion, RegionSpace},
        pci_routing::{IrqDescriptor, PciRoutingTable, Pin},
        resource::{
            AddressSpaceResourceType, InterruptPolarity, InterruptTrigger, Resource,
            resource_descriptor_list,
        },
    },
    platform::{
        AcpiPlatform,
        interrupt::{InterruptModel, Polarity, TriggerMode},
        pci::PciConfigRegions,
    },
    sdt::spcr::{Spcr, SpcrInterfaceType},
};
use ax_kspin::SpinNoPreempt as Mutex;
pub use rdif_base::irq::{AcpiGsiController, AcpiGsiRoute, AcpiIrqPolarity, AcpiIrqTrigger};
use spin::Once;

use crate::{
    DeviceId, PlatformDevice,
    error::DriverError,
    probe::{
        OnProbeError, ProbeError,
        pci::{PciAddress, PciInfo, PciIntxRoute},
    },
    register::{DriverRegister, ProbeKind},
};

const LOONGARCH_PCH_PIC_GSI_COUNT: u16 = 256;
const PCI_ROOT_FALLBACK_PATHS: &[&str] = &["\\_SB.PCI0", "\\_SB.PCI1", "\\_SB.PC00", "\\_SB.PC01"];

static SYSTEM: Once<System> = Once::new();
static NULL_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy)]
pub struct AcpiRoot {
    pub rsdp: usize,
    pub phys_to_virt: fn(usize) -> *mut u8,
}

impl core::fmt::Debug for AcpiRoot {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AcpiRoot")
            .field("rsdp", &self.rsdp)
            .finish_non_exhaustive()
    }
}

impl AcpiRoot {
    pub const fn new(rsdp: usize, phys_to_virt: fn(usize) -> *mut u8) -> Self {
        Self { rsdp, phys_to_virt }
    }

    pub const fn identity(rsdp: usize) -> Self {
        Self::new(rsdp, identity_phys_to_virt)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiId {
    pub hid: &'static str,
    pub cids: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiPciEcam {
    pub segment_group: u16,
    pub bus_start: u8,
    pub bus_end: u8,
    pub base_address: u64,
}

impl AcpiPciEcam {
    pub fn size(&self) -> usize {
        let buses = usize::from(self.bus_end.saturating_sub(self.bus_start)) + 1;
        buses << 20
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiIoApic {
    pub id: u8,
    pub address: u32,
    pub gsi_base: u32,
    pub redirection_entries: u8,
}

impl AcpiIoApic {
    fn gsi_source(self) -> AcpiGsiSource {
        AcpiGsiSource {
            controller: AcpiGsiController::IoApic,
            controller_id: u16::from(self.id),
            controller_address: u64::from(self.address),
            gsi_base: self.gsi_base,
            gsi_count: u16::from(self.redirection_entries),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiPchPic {
    pub id: u16,
    pub address: u64,
    pub mmio_size: u16,
    pub gsi_count: u16,
    pub gsi_base: u32,
}

impl AcpiPchPic {
    fn gsi_source(self) -> AcpiGsiSource {
        AcpiGsiSource {
            controller: AcpiGsiController::PchPic,
            controller_id: self.id,
            controller_address: self.address,
            gsi_base: self.gsi_base,
            gsi_count: self.gsi_count,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AcpiGsiSource {
    controller: AcpiGsiController,
    controller_id: u16,
    controller_address: u64,
    gsi_base: u32,
    gsi_count: u16,
}

impl AcpiGsiSource {
    fn contains_gsi(self, gsi: u32) -> bool {
        gsi.checked_sub(self.gsi_base)
            .is_some_and(|input| input < u32::from(self.gsi_count))
    }

    fn route(
        self,
        gsi: u32,
        trigger: AcpiIrqTrigger,
        polarity: AcpiIrqPolarity,
    ) -> Option<AcpiGsiRoute> {
        let controller_input = u8::try_from(gsi.checked_sub(self.gsi_base)?).ok()?;
        Some(AcpiGsiRoute {
            gsi,
            controller: self.controller,
            controller_id: self.controller_id,
            controller_address: self.controller_address,
            controller_input,
            trigger,
            polarity,
        })
    }
}

#[derive(Debug, Clone)]
pub struct AcpiRouting {
    io_apics: Vec<AcpiIoApic>,
    pch_pics: Vec<AcpiPchPic>,
    isa_overrides: Vec<AcpiIsaIrqOverride>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiIsaIrqOverride {
    pub source: u8,
    pub gsi: u32,
    pub trigger: AcpiIrqTrigger,
    pub polarity: AcpiIrqPolarity,
}

impl AcpiRouting {
    pub const fn new() -> Self {
        Self {
            io_apics: Vec::new(),
            pch_pics: Vec::new(),
            isa_overrides: Vec::new(),
        }
    }

    pub fn add_io_apic(&mut self, io_apic: AcpiIoApic) {
        self.io_apics.push(io_apic);
    }

    pub fn io_apics(&self) -> &[AcpiIoApic] {
        &self.io_apics
    }

    pub fn add_pch_pic(&mut self, pch_pic: AcpiPchPic) {
        self.pch_pics.push(pch_pic);
    }

    pub fn pch_pics(&self) -> &[AcpiPchPic] {
        &self.pch_pics
    }

    pub fn add_isa_irq_override(&mut self, irq_override: AcpiIsaIrqOverride) {
        self.isa_overrides.push(irq_override);
    }

    pub fn resolve_gsi(&self, gsi: u32) -> Option<AcpiGsiRoute> {
        self.gsi_sources()
            .find(|source| source.contains_gsi(gsi))?
            .route(gsi, self.default_trigger(gsi), self.default_polarity(gsi))
    }

    fn gsi_sources(&self) -> impl Iterator<Item = AcpiGsiSource> + '_ {
        self.io_apics
            .iter()
            .copied()
            .map(AcpiIoApic::gsi_source)
            .chain(self.pch_pics.iter().copied().map(AcpiPchPic::gsi_source))
    }

    fn default_trigger(&self, gsi: u32) -> AcpiIrqTrigger {
        self.isa_overrides
            .iter()
            .find(|irq_override| irq_override.gsi == gsi)
            .map(|irq_override| irq_override.trigger)
            .unwrap_or(if gsi < 16 {
                AcpiIrqTrigger::Edge
            } else {
                AcpiIrqTrigger::Level
            })
    }

    fn default_polarity(&self, gsi: u32) -> AcpiIrqPolarity {
        self.isa_overrides
            .iter()
            .find(|irq_override| irq_override.gsi == gsi)
            .map(|irq_override| irq_override.polarity)
            .unwrap_or(if gsi < 16 {
                AcpiIrqPolarity::ActiveHigh
            } else {
                AcpiIrqPolarity::ActiveLow
            })
    }
}

impl Default for AcpiRouting {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{
        string::{String, ToString},
        sync::Arc,
        vec::Vec,
    };
    use core::str::FromStr;

    use acpi::{
        address::{AddressSpace, GenericAddress},
        aml::{
            Interpreter,
            namespace::{AmlName, NamespaceLevelKind},
            object::Object,
            pci_routing::IrqDescriptor,
            resource::{InterruptPolarity, InterruptTrigger},
        },
        registers::{FixedRegisters, Pm1ControlRegisterBlock, Pm1EventRegisterBlock},
    };

    use super::{
        AcpiGsiController, AcpiHandler, AcpiId, AcpiIoApic, AcpiIrqPolarity, AcpiIrqTrigger,
        AcpiIsaIrqOverride, AcpiPchPic, AcpiResourceRange, AcpiRoot, AcpiRouting, LinkIrqResource,
        LinkIrqResourceKind, Mutex, PciLinkAllocator, System, irq_descriptor_gsi,
        is_buffer_field_to_field_unit_store_gap, pci_irq_descriptor_gsi,
        pci_link_irq_field_candidates, route_with_irq_descriptor_flags, select_pci_link_irq,
    };
    use crate::register::{DriverRegister, ProbeKind, ProbeLevel, ProbePriority};

    fn test_fixed_registers(handler: &AcpiHandler) -> Arc<FixedRegisters<AcpiHandler>> {
        let event_gas = GenericAddress {
            address_space: AddressSpace::SystemIo,
            bit_width: 32,
            bit_offset: 0,
            access_size: 3,
            address: 0x1000,
        };
        let control_gas = GenericAddress {
            address_space: AddressSpace::SystemIo,
            bit_width: 16,
            bit_offset: 0,
            access_size: 2,
            address: 0x1004,
        };
        Arc::new(FixedRegisters {
            pm1_event_registers: Pm1EventRegisterBlock {
                pm1_event_length: 4,
                pm1a: unsafe { acpi::address::MappedGas::map_gas(event_gas, handler).unwrap() },
                pm1b: None,
            },
            pm1_control_registers: Pm1ControlRegisterBlock {
                pm1a: unsafe { acpi::address::MappedGas::map_gas(control_gas, handler).unwrap() },
                pm1b: None,
            },
        })
    }

    fn interpreter_with_devices(handler: AcpiHandler) -> Interpreter<AcpiHandler> {
        let interpreter =
            Interpreter::new(handler.clone(), 2, test_fixed_registers(&handler), None);
        {
            let mut namespace = interpreter.namespace.lock();
            let pnp = AmlName::from_str("\\_SB.RTC0").unwrap();
            namespace
                .add_level(pnp.clone(), NamespaceLevelKind::Device)
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str("_HID").unwrap().resolve(&pnp).unwrap(),
                    Object::Integer(0x000b_d041).wrap(),
                )
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str("_CRS").unwrap().resolve(&pnp).unwrap(),
                    Object::Buffer(Vec::from([
                        0x47, 0x01, 0x70, 0x00, 0x70, 0x00, 0x01, 0x08, 0x22, 0x00, 0x01, 0x79,
                        0x00,
                    ]))
                    .wrap(),
                )
                .unwrap();

            let loon = AmlName::from_str("\\_SB.LRTC").unwrap();
            namespace
                .add_level(loon.clone(), NamespaceLevelKind::Device)
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str("_HID").unwrap().resolve(&loon).unwrap(),
                    Object::String(String::from("LOON0001")).wrap(),
                )
                .unwrap();
            namespace
                .insert(
                    AmlName::from_str("_CRS").unwrap().resolve(&loon).unwrap(),
                    Object::Buffer(Vec::from([
                        0x86, 0x09, 0x00, 0x01, 0x00, 0x01, 0x0d, 0x10, 0x00, 0x01, 0x00, 0x00,
                        0x79, 0x00,
                    ]))
                    .wrap(),
                )
                .unwrap();
        }
        interpreter
    }

    fn test_system() -> System {
        let handler = AcpiHandler::new(AcpiRoot::identity(0x1000), Vec::new());
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 0,
            address: 0xfec0_0000,
            gsi_base: 0,
            redirection_entries: 24,
        });
        System {
            ecam_regions: Vec::new(),
            routing,
            interpreter: Some(interpreter_with_devices(handler.clone())),
            handler,
            pci: None,
            probed_names: Mutex::new(alloc::collections::BTreeSet::new()),
            populated_paths: Mutex::new(alloc::collections::BTreeMap::new()),
            populated_resources: Mutex::new(alloc::collections::BTreeMap::new()),
        }
    }

    static LAST_PATH: Mutex<Option<String>> = Mutex::new(None);

    fn probe_rtc(probe: super::ProbeAcpi<'_>) -> Result<(), crate::probe::OnProbeError> {
        let info = probe.info();
        assert_eq!(info.hid(), Some("PNP0B00"));
        assert_eq!(
            info.io_ranges(),
            &[AcpiResourceRange {
                base: 0x70,
                size: 8,
            }]
        );
        assert_eq!(
            info.irq_routes()
                .iter()
                .map(|route| route.gsi)
                .collect::<Vec<_>>(),
            Vec::from([8])
        );
        *LAST_PATH.lock() = Some(info.path.to_string());
        Ok(())
    }

    static RTC_REGISTER: DriverRegister = DriverRegister {
        name: "test acpi rtc",
        level: ProbeLevel::PostKernel,
        priority: ProbePriority::DEFAULT,
        probe_kinds: &[ProbeKind::Acpi {
            ids: &[AcpiId {
                hid: "PNP0B00",
                cids: &[],
            }],
            on_probe: probe_rtc,
        }],
    };

    #[test]
    fn acpi_probe_matches_namespace_device_and_exposes_io_and_irq_resources() {
        let system = test_system();

        let results = system.probe_register(&RTC_REGISTER).unwrap();

        assert_eq!(results.len(), 1);
        assert!(results.into_iter().all(|result| result.is_ok()));
        assert_eq!(LAST_PATH.lock().as_deref(), Some("\\_SB_.RTC0"));
    }

    #[test]
    fn acpi_info_exposes_loongson_memory_resource() {
        let system = test_system();
        let info = system
            .device_infos_for_ids(&[AcpiId {
                hid: "LOON0001",
                cids: &[],
            }])
            .unwrap()
            .into_iter()
            .next()
            .expect("LOON0001 device should be discovered");

        assert_eq!(info.hid.as_deref(), Some("LOON0001"));
        assert_eq!(
            info.memory_ranges.as_slice(),
            &[AcpiResourceRange {
                base: 0x100d_0100,
                size: 0x100,
            }]
        );
    }

    #[test]
    fn acpi_eisa_integer_ids_decode_to_hid_strings() {
        assert_eq!(
            super::decode_eisa_id(0x000b_d041).as_deref(),
            Some("PNP0B00")
        );
        assert_eq!(
            super::decode_eisa_id(0x030a_d041).as_deref(),
            Some("PNP0A03")
        );
        assert_eq!(
            super::decode_eisa_id(0x080a_d041).as_deref(),
            Some("PNP0A08")
        );
    }

    #[test]
    fn ioapic_routes_preserve_firmware_controller_metadata() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 0,
            address: 0xfec0_0000,
            gsi_base: 0,
            redirection_entries: 24,
        });

        let legacy_irq = routing
            .resolve_gsi(4)
            .expect("legacy ISA GSI 4 should be handled by the IOAPIC");
        assert_eq!(legacy_irq.controller_input, 4);
        assert_eq!(legacy_irq.trigger, AcpiIrqTrigger::Edge);
        assert_eq!(legacy_irq.polarity, AcpiIrqPolarity::ActiveHigh);

        let irq = routing
            .resolve_gsi(16)
            .expect("gsi 16 should be handled by the IOAPIC");
        assert_eq!(irq.gsi, 16);
        assert_eq!(irq.controller, AcpiGsiController::IoApic);
        assert_eq!(irq.controller_id, 0);
        assert_eq!(irq.controller_address, 0xfec0_0000);
        assert_eq!(irq.controller_input, 16);
        assert_eq!(irq.trigger, AcpiIrqTrigger::Level);
        assert_eq!(irq.polarity, AcpiIrqPolarity::ActiveLow);
        assert!(routing.resolve_gsi(24).is_none());
    }

    #[test]
    fn pci_link_irq_can_route_to_legacy_ioapic_gsi() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 0,
            address: 0xfec0_0000,
            gsi_base: 0,
            redirection_entries: 24,
        });

        let descriptor = IrqDescriptor {
            is_consumer: false,
            trigger: InterruptTrigger::Level,
            polarity: InterruptPolarity::ActiveLow,
            is_shared: true,
            is_wake_capable: false,
            irq: 1 << 10,
        };
        let gsi = irq_descriptor_gsi(&descriptor).unwrap();

        assert_eq!(gsi, 10);
        let route = routing
            .resolve_gsi(gsi)
            .expect("IOAPIC routes the legacy GSI from the ACPI PCI link");
        assert_eq!(route.gsi, 10);
        assert_eq!(route.controller_input, 10);
    }

    #[test]
    fn pci_link_descriptor_reports_selected_power_of_two_gsi_directly() {
        let resource = LinkIrqResource {
            kind: LinkIrqResourceKind::SmallIrq,
            descriptor: IrqDescriptor {
                is_consumer: false,
                trigger: InterruptTrigger::Level,
                polarity: InterruptPolarity::ActiveLow,
                is_shared: true,
                is_wake_capable: false,
                irq: 1 << 4,
            },
            irqs: alloc::vec![4],
        };

        let descriptor = resource.descriptor_for_irq(4);
        assert_eq!(descriptor.irq, 4);
        assert_eq!(pci_irq_descriptor_gsi(&descriptor), Some(4));
    }

    #[test]
    fn pch_pic_routes_map_acpi_gsi_to_controller_input() {
        let mut routing = AcpiRouting::new();
        routing.add_pch_pic(AcpiPchPic {
            id: 1,
            address: 0x1000_0000,
            mmio_size: 0x1000,
            gsi_count: 64,
            gsi_base: 64,
        });

        let route = routing
            .resolve_gsi(82)
            .expect("GSI 82 should be covered by the PCH-PIC");
        assert_eq!(route.controller, AcpiGsiController::PchPic);
        assert_eq!(route.controller_id, 1);
        assert_eq!(route.controller_address, 0x1000_0000);
        assert_eq!(route.controller_input, 18);
        assert!(routing.resolve_gsi(128).is_none());
    }

    #[test]
    fn ioapic_routes_apply_isa_interrupt_source_overrides() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 0,
            address: 0xfec0_0000,
            gsi_base: 0,
            redirection_entries: 24,
        });
        routing.add_isa_irq_override(AcpiIsaIrqOverride {
            source: 0,
            gsi: 2,
            trigger: AcpiIrqTrigger::Level,
            polarity: AcpiIrqPolarity::ActiveLow,
        });

        let route = routing
            .resolve_gsi(2)
            .expect("overridden ISA GSI should still route through the IOAPIC");
        assert_eq!(route.controller_input, 2);
        assert_eq!(route.trigger, AcpiIrqTrigger::Level);
        assert_eq!(route.polarity, AcpiIrqPolarity::ActiveLow);
    }

    #[test]
    fn ioapic_route_supports_gsi_256_without_using_it_as_a_vector() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 3,
            address: 0xfec0_1000,
            gsi_base: 256,
            redirection_entries: 24,
        });

        let route = routing
            .resolve_gsi(256)
            .expect("the first input of a high-base IOAPIC must be routable");

        assert_eq!(route.gsi, 256);
        assert_eq!(route.controller_id, 3);
        assert_eq!(route.controller_address, 0xfec0_1000);
        assert_eq!(route.controller_input, 0);
    }

    #[test]
    fn sparse_ioapic_gsi_base_preserves_the_controller_local_input() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 4,
            address: 0xfec0_2000,
            gsi_base: 0x1_0000,
            redirection_entries: 24,
        });

        let route = routing
            .resolve_gsi(0x1_0007)
            .expect("a sparse high GSI must remain a firmware routing key");

        assert_eq!(route.gsi, 0x1_0007);
        assert_eq!(route.controller_id, 4);
        assert_eq!(route.controller_input, 7);
    }

    #[test]
    fn ioapic_range_can_end_at_the_largest_u32_gsi() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 5,
            address: 0xfec0_3000,
            gsi_base: u32::MAX - 7,
            redirection_entries: 8,
        });

        let route = routing
            .resolve_gsi(u32::MAX)
            .expect("checked GSI offsets must not lose the top u32 route");

        assert_eq!(route.gsi, u32::MAX);
        assert_eq!(route.controller_input, 7);
    }

    #[test]
    fn two_ioapics_route_equal_local_inputs_by_full_gsi() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 1,
            address: 0xfec0_0000,
            gsi_base: 0,
            redirection_entries: 24,
        });
        routing.add_io_apic(AcpiIoApic {
            id: 2,
            address: 0xfec0_1000,
            gsi_base: 256,
            redirection_entries: 24,
        });

        let low = routing.resolve_gsi(7).expect("low IOAPIC route");
        let high = routing.resolve_gsi(263).expect("high IOAPIC route");

        assert_eq!(low.controller_input, 7);
        assert_eq!(high.controller_input, 7);
        assert_ne!(low.gsi, high.gsi);
        assert_ne!(low.controller_id, high.controller_id);
        assert_ne!(low.controller_address, high.controller_address);
    }

    #[test]
    fn pci_link_irq_selects_prs_when_crs_is_unassigned() {
        let current = LinkIrqResource {
            kind: LinkIrqResourceKind::ExtendedIrq,
            descriptor: IrqDescriptor {
                is_consumer: true,
                trigger: InterruptTrigger::Level,
                polarity: InterruptPolarity::ActiveHigh,
                is_shared: true,
                is_wake_capable: false,
                irq: 0,
            },
            irqs: alloc::vec![0],
        };
        let possible = LinkIrqResource {
            kind: LinkIrqResourceKind::ExtendedIrq,
            descriptor: IrqDescriptor {
                is_consumer: true,
                trigger: InterruptTrigger::Level,
                polarity: InterruptPolarity::ActiveHigh,
                is_shared: true,
                is_wake_capable: false,
                irq: 5,
            },
            irqs: alloc::vec![5, 10, 11],
        };

        let link = "\\_SB.LNKA".parse().unwrap();
        let allocator = PciLinkAllocator::default();
        let selection = select_pci_link_irq(&link, Some(&current), Some(&possible), &allocator)
            .expect("possible IRQs should allocate a PCI link");

        assert_eq!(selection.irq, 10);
        assert!(selection.needs_programming);
        assert_eq!(selection.resource.irqs, alloc::vec![5, 10, 11]);
    }

    #[test]
    fn pci_link_irq_allocator_spreads_unassigned_links() {
        let possible = LinkIrqResource {
            kind: LinkIrqResourceKind::ExtendedIrq,
            descriptor: IrqDescriptor {
                is_consumer: true,
                trigger: InterruptTrigger::Level,
                polarity: InterruptPolarity::ActiveHigh,
                is_shared: true,
                is_wake_capable: false,
                irq: 5,
            },
            irqs: alloc::vec![5, 10, 11],
        };
        let first = "\\_SB.LNKA".parse().unwrap();
        let second = "\\_SB.LNKB".parse().unwrap();
        let mut allocator = PciLinkAllocator::default();

        let first_selection = select_pci_link_irq(&first, None, Some(&possible), &allocator)
            .expect("first link should allocate");
        assert_eq!(first_selection.irq, 10);
        allocator.commit(&first, first_selection.irq);

        let second_selection = select_pci_link_irq(&second, None, Some(&possible), &allocator)
            .expect("second link should allocate");
        assert_eq!(second_selection.irq, 11);
    }

    #[test]
    fn pci_link_srs_gap_detection_is_narrow() {
        let gap = acpi::aml::AmlError::ObjectNotOfExpectedType {
            expected: acpi::aml::object::ObjectType::Integer,
            got: acpi::aml::object::ObjectType::BufferField,
        };
        let other = acpi::aml::AmlError::ObjectNotOfExpectedType {
            expected: acpi::aml::object::ObjectType::Buffer,
            got: acpi::aml::object::ObjectType::BufferField,
        };

        assert!(is_buffer_field_to_field_unit_store_gap(&gap));
        assert!(!is_buffer_field_to_field_unit_store_gap(&other));
    }

    #[test]
    fn qemu_pci_link_names_map_to_pirq_fields() {
        assert_eq!(
            pci_link_irq_field_candidates(&"\\_SB.LNKB".parse().unwrap()),
            ["\\_SB.PRQB", "\\_SB.PRQ1"]
        );
        assert_eq!(
            pci_link_irq_field_candidates(&"\\_SB.LNKG".parse().unwrap()),
            ["\\_SB.PRQG"]
        );
        assert!(pci_link_irq_field_candidates(&"\\_SB.LNKS".parse().unwrap()).is_empty());
    }

    #[test]
    fn pci_irq_route_preserves_descriptor_trigger_and_polarity() {
        let mut routing = AcpiRouting::new();
        routing.add_io_apic(AcpiIoApic {
            id: 0,
            address: 0xfec0_0000,
            gsi_base: 0,
            redirection_entries: 24,
        });
        let route = routing.resolve_gsi(16).unwrap();
        let descriptor = IrqDescriptor {
            is_consumer: true,
            trigger: InterruptTrigger::Edge,
            polarity: InterruptPolarity::ActiveHigh,
            is_shared: false,
            is_wake_capable: false,
            irq: 16,
        };

        let route = route_with_irq_descriptor_flags(route, &descriptor);

        assert_eq!(route.trigger, AcpiIrqTrigger::Edge);
        assert_eq!(route.polarity, AcpiIrqPolarity::ActiveHigh);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiPciIrqRoute {
    pub address: PciAddress,
    pub interrupt_pin: u8,
    pub intx_route: PciIntxRoute,
    pub gsi: AcpiGsiRoute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiResourceRange {
    pub base: u64,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpiResourceDevice {
    pub path: String,
    pub hid: Option<String>,
    pub cids: Vec<String>,
    pub memory_ranges: Vec<AcpiResourceRange>,
    pub io_ranges: Vec<AcpiResourceRange>,
    pub irq_routes: Vec<AcpiGsiRoute>,
}

#[derive(Debug, Clone)]
struct AcpiDeviceInfo {
    path: String,
    hid: Option<String>,
    cids: Vec<String>,
    memory_ranges: Vec<AcpiResourceRange>,
    io_ranges: Vec<AcpiResourceRange>,
    irq_routes: Vec<AcpiGsiRoute>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AcpiResourceAddressSpace {
    Memory,
    Io,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AcpiResourceAddress {
    pub space: AcpiResourceAddressSpace,
    pub base: u64,
}

impl AcpiResourceAddress {
    pub const fn memory(base: u64) -> Self {
        Self {
            space: AcpiResourceAddressSpace::Memory,
            base,
        }
    }

    pub const fn io(base: u64) -> Self {
        Self {
            space: AcpiResourceAddressSpace::Io,
            base,
        }
    }

    pub fn from_generic_address(address: GenericAddress) -> Option<Self> {
        match address.address_space {
            AddressSpace::SystemMemory => Some(Self::memory(address.address)),
            AddressSpace::SystemIo => Some(Self::io(address.address)),
            _ => None,
        }
    }
}

pub struct AcpiInfo<'a> {
    pub root: &'a System,
    pub path: &'a str,
    pub irq_route: Option<AcpiGsiRoute>,
    device: Option<&'a AcpiDeviceInfo>,
}

impl AcpiInfo<'_> {
    pub const fn irq_route(&self) -> Option<AcpiGsiRoute> {
        self.irq_route
    }

    pub fn hid(&self) -> Option<&str> {
        self.device
            .as_ref()
            .and_then(|device| device.hid.as_deref())
    }

    pub fn cids(&self) -> &[String] {
        self.device
            .as_ref()
            .map(|device| device.cids.as_slice())
            .unwrap_or_default()
    }

    pub fn memory_ranges(&self) -> &[AcpiResourceRange] {
        self.device
            .as_ref()
            .map(|device| device.memory_ranges.as_slice())
            .unwrap_or_default()
    }

    pub fn io_ranges(&self) -> &[AcpiResourceRange] {
        self.device
            .as_ref()
            .map(|device| device.io_ranges.as_slice())
            .unwrap_or_default()
    }

    pub fn irq_routes(&self) -> &[AcpiGsiRoute] {
        self.device
            .as_ref()
            .map(|device| device.irq_routes.as_slice())
            .unwrap_or_default()
    }
}

impl From<AcpiDeviceInfo> for AcpiResourceDevice {
    fn from(value: AcpiDeviceInfo) -> Self {
        Self {
            path: value.path,
            hid: value.hid,
            cids: value.cids,
            memory_ranges: value.memory_ranges,
            io_ranges: value.io_ranges,
            irq_routes: value.irq_routes,
        }
    }
}

pub struct ProbeAcpi<'a> {
    info: AcpiInfo<'a>,
    platform: PlatformDevice,
}

impl<'a> ProbeAcpi<'a> {
    #[allow(dead_code)]
    pub(crate) fn new(info: AcpiInfo<'a>, platform: PlatformDevice) -> Self {
        Self { info, platform }
    }

    pub const fn info(&self) -> &AcpiInfo<'a> {
        &self.info
    }

    pub fn into_platform_device(self) -> PlatformDevice {
        self.platform
    }

    pub fn into_parts(self) -> (AcpiInfo<'a>, PlatformDevice) {
        (self.info, self.platform)
    }
}

pub type FnOnProbe = for<'a> fn(ProbeAcpi<'a>) -> Result<(), OnProbeError>;

pub fn check_root(root: AcpiRoot) -> Result<(), DriverError> {
    if root.rsdp == 0 {
        return Err(acpi_error(AcpiError::NoValidRsdp));
    }
    root.tables().map(|_| ()).map_err(acpi_error)
}

pub fn init(root: AcpiRoot) -> Result<(), DriverError> {
    let system = System::new(root)?;
    init_system(system)
}

pub fn init_without_aml(root: AcpiRoot) -> Result<(), DriverError> {
    let system = System::new_without_aml(root)?;
    init_system(system)
}

fn init_system(system: System) -> Result<(), DriverError> {
    info!(
        "ACPI initialized: {} PCI ECAM region(s), {} IOAPIC(s), {} PCH-PIC(s)",
        system.pci_ecam_regions().len(),
        system.routing().io_apics().len(),
        system.routing().pch_pics().len()
    );
    SYSTEM.call_once(|| system);
    Ok(())
}

pub(crate) fn try_probe_register(
    register: &DriverRegister,
) -> Option<Result<alloc::vec::Vec<Result<(), OnProbeError>>, ProbeError>> {
    SYSTEM.get().map(|system| system.probe_register(register))
}

pub(crate) fn try_system() -> Option<&'static System> {
    SYSTEM.get()
}

pub fn with_acpi<T>(f: impl FnOnce(&System) -> T) -> Option<T> {
    try_system().map(f)
}

pub fn spcr_console_device_id() -> Option<DeviceId> {
    try_system().and_then(System::spcr_console_device_id)
}

fn acpi_error(err: AcpiError) -> DriverError {
    DriverError::Unknown(format!("{err:?}"))
}

fn on_probe_error(err: impl core::fmt::Debug) -> OnProbeError {
    OnProbeError::other(format!("{err:?}"))
}

fn identity_phys_to_virt(paddr: usize) -> *mut u8 {
    paddr as *mut u8
}

#[derive(Clone)]
struct AcpiHandler {
    root: AcpiRoot,
    pci_ecam_regions: Rc<Vec<AcpiPciEcam>>,
}

impl AcpiHandler {
    fn new(root: AcpiRoot, pci_ecam_regions: Vec<AcpiPciEcam>) -> Self {
        Self {
            root,
            pci_ecam_regions: Rc::new(pci_ecam_regions),
        }
    }

    fn virt_addr(&self, physical_address: usize) -> usize {
        (self.root.phys_to_virt)(physical_address) as usize
    }

    fn pci_config_ptr(
        &self,
        address: acpi::PciAddress,
        offset: u16,
        width: usize,
    ) -> Option<*mut u8> {
        let offset = usize::from(offset);
        if offset.checked_add(width)? > 4096 {
            return None;
        }

        let bus = address.bus();
        let region = self.pci_ecam_regions.iter().find(|region| {
            address.segment() == region.segment_group
                && bus >= region.bus_start
                && bus <= region.bus_end
        })?;
        let bus_offset = usize::from(bus - region.bus_start) << 20;
        let device_offset = usize::from(address.device()) << 15;
        let function_offset = usize::from(address.function()) << 12;
        let physical_address =
            region.base_address as usize + bus_offset + device_offset + function_offset + offset;

        Some((self.root.phys_to_virt)(physical_address))
    }
}

impl Handler for AcpiHandler {
    unsafe fn map_physical_region<T>(
        &self,
        physical_address: usize,
        size: usize,
    ) -> PhysicalMapping<Self, T> {
        PhysicalMapping {
            physical_start: physical_address,
            virtual_start: NonNull::new(self.virt_addr(physical_address) as *mut T)
                .expect("ACPI physical mapping must not be null"),
            region_length: size,
            mapped_length: size,
            handler: self.clone(),
        }
    }

    fn unmap_physical_region<T>(_region: &PhysicalMapping<Self, T>) {}

    fn read_u8(&self, address: usize) -> u8 {
        unsafe { (self.virt_addr(address) as *const u8).read_volatile() }
    }

    fn read_u16(&self, address: usize) -> u16 {
        unsafe { (self.virt_addr(address) as *const u16).read_volatile() }
    }

    fn read_u32(&self, address: usize) -> u32 {
        unsafe { (self.virt_addr(address) as *const u32).read_volatile() }
    }

    fn read_u64(&self, address: usize) -> u64 {
        unsafe { (self.virt_addr(address) as *const u64).read_volatile() }
    }

    fn write_u8(&self, address: usize, value: u8) {
        unsafe { (self.virt_addr(address) as *mut u8).write_volatile(value) }
    }

    fn write_u16(&self, address: usize, value: u16) {
        unsafe { (self.virt_addr(address) as *mut u16).write_volatile(value) }
    }

    fn write_u32(&self, address: usize, value: u32) {
        unsafe { (self.virt_addr(address) as *mut u32).write_volatile(value) }
    }

    fn write_u64(&self, address: usize, value: u64) {
        unsafe { (self.virt_addr(address) as *mut u64).write_volatile(value) }
    }

    fn read_io_u8(&self, port: u16) -> u8 {
        read_io_u8(port)
    }

    fn read_io_u16(&self, port: u16) -> u16 {
        read_io_u16(port)
    }

    fn read_io_u32(&self, port: u16) -> u32 {
        read_io_u32(port)
    }

    fn write_io_u8(&self, port: u16, value: u8) {
        write_io_u8(port, value);
    }

    fn write_io_u16(&self, port: u16, value: u16) {
        write_io_u16(port, value);
    }

    fn write_io_u32(&self, port: u16, value: u32) {
        write_io_u32(port, value);
    }

    fn read_pci_u8(&self, address: acpi::PciAddress, offset: u16) -> u8 {
        if let Some(ptr) = self.pci_config_ptr(address, offset, 1) {
            return unsafe { ptr.read_volatile() };
        }
        pci_legacy_read_u8(address, offset).unwrap_or(u8::MAX)
    }

    fn read_pci_u16(&self, address: acpi::PciAddress, offset: u16) -> u16 {
        let lo = u16::from(self.read_pci_u8(address, offset));
        let hi = u16::from(self.read_pci_u8(address, offset.saturating_add(1)));
        lo | (hi << 8)
    }

    fn read_pci_u32(&self, address: acpi::PciAddress, offset: u16) -> u32 {
        let b0 = u32::from(self.read_pci_u8(address, offset));
        let b1 = u32::from(self.read_pci_u8(address, offset.saturating_add(1)));
        let b2 = u32::from(self.read_pci_u8(address, offset.saturating_add(2)));
        let b3 = u32::from(self.read_pci_u8(address, offset.saturating_add(3)));
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    fn write_pci_u8(&self, address: acpi::PciAddress, offset: u16, value: u8) {
        if let Some(ptr) = self.pci_config_ptr(address, offset, 1) {
            unsafe { ptr.write_volatile(value) };
            return;
        }
        pci_legacy_write_u8(address, offset, value);
    }

    fn write_pci_u16(&self, address: acpi::PciAddress, offset: u16, value: u16) {
        self.write_pci_u8(address, offset, value as u8);
        self.write_pci_u8(address, offset.saturating_add(1), (value >> 8) as u8);
    }

    fn write_pci_u32(&self, address: acpi::PciAddress, offset: u16, value: u32) {
        self.write_pci_u8(address, offset, value as u8);
        self.write_pci_u8(address, offset.saturating_add(1), (value >> 8) as u8);
        self.write_pci_u8(address, offset.saturating_add(2), (value >> 16) as u8);
        self.write_pci_u8(address, offset.saturating_add(3), (value >> 24) as u8);
    }

    fn nanos_since_boot(&self) -> u64 {
        0
    }

    fn stall(&self, microseconds: u64) {
        for _ in 0..microseconds.saturating_mul(100) {
            core::hint::spin_loop();
        }
    }

    fn sleep(&self, milliseconds: u64) {
        self.stall(milliseconds.saturating_mul(1000));
    }

    fn create_mutex(&self) -> acpi::Handle {
        acpi::Handle(0)
    }

    fn acquire(&self, _mutex: acpi::Handle, _timeout: u16) -> Result<(), acpi::aml::AmlError> {
        let _guard = NULL_LOCK.lock();
        Ok(())
    }

    fn release(&self, _mutex: acpi::Handle) {}
}

impl AcpiRoot {
    fn handler(self) -> AcpiHandler {
        AcpiHandler::new(self, Vec::new())
    }

    fn handler_with_pci_ecam(self, pci_ecam_regions: Vec<AcpiPciEcam>) -> AcpiHandler {
        AcpiHandler::new(self, pci_ecam_regions)
    }

    fn tables(self) -> Result<AcpiTables<AcpiHandler>, AcpiError> {
        unsafe { AcpiTables::from_rsdp(self.handler(), self.rsdp) }
    }
}

pub struct System {
    ecam_regions: Vec<AcpiPciEcam>,
    routing: AcpiRouting,
    interpreter: Option<Interpreter<AcpiHandler>>,
    handler: AcpiHandler,
    pci: Option<AcpiPciNamespace>,
    probed_names: Mutex<BTreeSet<&'static str>>,
    populated_paths: Mutex<BTreeMap<String, DeviceId>>,
    populated_resources: Mutex<BTreeMap<AcpiResourceAddress, DeviceId>>,
}

unsafe impl Send for System {}
unsafe impl Sync for System {}

struct AcpiPciNamespace {
    link_allocator: Mutex<PciLinkAllocator>,
    roots: Vec<AcpiPciRoot>,
}

struct AcpiPciRoot {
    segment: u16,
    bus: u8,
    path: String,
    prt: Option<PciRoutingTable>,
    link_prt: Option<PciLinkRoutingTable>,
}

impl System {
    pub fn new(root: AcpiRoot) -> Result<Self, DriverError> {
        Self::new_with_options(root, true)
    }

    pub fn new_without_aml(root: AcpiRoot) -> Result<Self, DriverError> {
        Self::new_with_options(root, false)
    }

    fn new_with_options(root: AcpiRoot, load_aml: bool) -> Result<Self, DriverError> {
        let handler = root.handler();
        let tables =
            unsafe { AcpiTables::from_rsdp(handler.clone(), root.rsdp) }.map_err(acpi_error)?;
        let ecam_regions = read_pci_ecam_regions(&tables)?;
        let routing = read_interrupt_routing(&tables)?;
        let namespace_handler = root.handler_with_pci_ecam(ecam_regions.clone());
        let (interpreter, pci) = if load_aml {
            let platform =
                AcpiPlatform::new(tables, namespace_handler.clone()).map_err(acpi_error)?;
            let interpreter = Interpreter::new_from_platform(&platform).map_err(acpi_error)?;
            interpreter.initialize_namespace();
            let pci = match read_pci_namespace(&interpreter) {
                Ok(pci) => Some(pci),
                Err(err) => {
                    warn!("failed to discover ACPI PCI namespace: {err:?}");
                    None
                }
            };
            (Some(interpreter), pci)
        } else {
            (None, None)
        };

        Ok(Self {
            ecam_regions,
            routing,
            interpreter,
            handler: namespace_handler,
            pci,
            probed_names: Mutex::new(BTreeSet::new()),
            populated_paths: Mutex::new(BTreeMap::new()),
            populated_resources: Mutex::new(BTreeMap::new()),
        })
    }

    pub fn pci_ecam_regions(&self) -> &[AcpiPciEcam] {
        &self.ecam_regions
    }

    pub fn routing(&self) -> &AcpiRouting {
        &self.routing
    }

    pub fn path_to_device_id(&self, path: &str) -> Option<DeviceId> {
        self.populated_paths.lock().get(path).copied()
    }

    pub fn resource_address_to_device_id(&self, address: AcpiResourceAddress) -> Option<DeviceId> {
        self.populated_resources.lock().get(&address).copied()
    }

    pub fn spcr_console_device_id(&self) -> Option<DeviceId> {
        let tables = unsafe { AcpiTables::from_rsdp(self.handler.clone(), self.handler.root.rsdp) }
            .map_err(acpi_error)
            .ok()?;
        tables
            .find_tables::<Spcr>()
            .filter(|spcr| is_supported_spcr_interface(spcr.interface_type()))
            .find_map(|spcr| {
                spcr_namespace_device_id(self, &spcr)
                    .or_else(|| spcr_resource_device_id(self, &spcr))
            })
    }

    pub fn resource_devices(&self) -> Result<Vec<AcpiResourceDevice>, ProbeError> {
        self.device_infos().map(|devices| {
            devices
                .into_iter()
                .map(AcpiResourceDevice::from)
                .collect::<Vec<_>>()
        })
    }

    pub fn serial_console_memory_range(&self) -> Option<AcpiResourceRange> {
        let tables = self.handler.root.tables().ok()?;
        let spcr = tables.find_table::<acpi::sdt::spcr::Spcr>()?;
        let address = spcr.base_address()?.ok()?;
        if address.address_space != acpi::address::AddressSpace::SystemMemory {
            return None;
        }

        Some(AcpiResourceRange {
            base: address.address,
            size: spcr_uart_register_size(address.access_size),
        })
    }

    pub fn pci_irq_for_endpoint(
        &self,
        info: PciInfo,
    ) -> Result<Option<AcpiPciIrqRoute>, OnProbeError> {
        let Some(intx_route) = info.intx_route else {
            return Ok(None);
        };
        let Some(irq) = self.resolve_endpoint_gsi(info.address, intx_route)? else {
            return Ok(None);
        };
        let Some(gsi) = pci_irq_descriptor_gsi(&irq) else {
            return Err(OnProbeError::other(format!(
                "ACPI PCI endpoint {} pin {} returned an invalid IRQ descriptor: {:?}",
                info.address, intx_route.root_pin, irq
            )));
        };
        if gsi == 0 {
            return Ok(None);
        }

        let Some(route) = self.routing.resolve_gsi(gsi) else {
            return Err(OnProbeError::other(format!(
                "ACPI GSI {} for PCI endpoint {} is not covered by a registered GSI controller",
                gsi, info.address
            )));
        };
        let route = route_with_irq_descriptor_flags(route, &irq);

        Ok(Some(AcpiPciIrqRoute {
            address: info.address,
            interrupt_pin: intx_route.root_pin,
            intx_route,
            gsi: route,
        }))
    }

    fn resolve_endpoint_gsi(
        &self,
        address: PciAddress,
        route: PciIntxRoute,
    ) -> Result<Option<IrqDescriptor>, OnProbeError> {
        let pin = acpi_pin(route.root_pin)?;
        let Some(pci) = &self.pci else {
            return Ok(None);
        };
        let roots = self.pci_root_candidates(address, pci);
        if roots.is_empty() {
            return Ok(None);
        }

        for root in roots {
            let Some(prt) = &root.prt else {
                continue;
            };

            if let Some(link_prt) = &root.link_prt
                && let Some(route) = link_prt
                    .route(
                        u16::from(route.root_device),
                        u16::from(route.root_function),
                        pin,
                        self.interpreter
                            .as_ref()
                            .expect("ACPI PCI routing requires an AML interpreter"),
                        &self.handler,
                        &mut pci.link_allocator.lock(),
                    )
                    .map_err(on_probe_error)?
            {
                return Ok(Some(route));
            }

            match prt.route(
                u16::from(route.root_device),
                u16::from(route.root_function),
                pin,
                self.interpreter
                    .as_ref()
                    .expect("ACPI PCI routing requires an AML interpreter"),
            ) {
                Ok(route) => return Ok(Some(route)),
                Err(AmlError::PrtNoEntry) => {}
                Err(err) => return Err(on_probe_error(err)),
            }
        }

        Ok(None)
    }

    fn pci_root_candidates<'a>(
        &self,
        address: PciAddress,
        pci: &'a AcpiPciNamespace,
    ) -> Vec<&'a AcpiPciRoot> {
        let mut roots = Vec::new();
        if let Some(root) = pci
            .roots
            .iter()
            .find(|root| root.segment == address.segment() && root.bus == address.bus())
            .or_else(|| {
                pci.roots
                    .iter()
                    .find(|root| root.segment == address.segment() && root.bus == 0)
            })
        {
            roots.push(root);
        }
        for path in PCI_ROOT_FALLBACK_PATHS {
            if let Some(root) = pci.roots.iter().find(|root| root.path == *path)
                && !roots.iter().any(|candidate| candidate.path == root.path)
            {
                roots.push(root);
            }
        }
        roots
    }

    fn device_infos_for_ids(&self, ids: &[AcpiId]) -> Result<Vec<AcpiDeviceInfo>, ProbeError> {
        let Some(interpreter) = &self.interpreter else {
            return Ok(Vec::new());
        };
        let mut devices = Vec::new();
        let mut namespace = interpreter.namespace.lock().clone();
        namespace
            .traverse(|path, level| {
                if level.kind != NamespaceLevelKind::Device {
                    return Ok(true);
                }
                let Some((hid, cids)) = acpi_device_ids(interpreter, path)? else {
                    return Ok(true);
                };
                if !acpi_ids_match(&hid, &cids, ids) {
                    return Ok(true);
                }
                let resources = read_device_resources(interpreter, path, &self.routing)?;
                devices.push(AcpiDeviceInfo {
                    path: path.as_string(),
                    hid: Some(hid),
                    cids,
                    memory_ranges: resources.memory_ranges,
                    io_ranges: resources.io_ranges,
                    irq_routes: resources.irq_routes,
                });
                Ok(true)
            })
            .map_err(|err| ProbeError::OnProbe(OnProbeError::other(format!("{err:?}"))))?;
        Ok(devices)
    }

    fn device_infos(&self) -> Result<Vec<AcpiDeviceInfo>, ProbeError> {
        let Some(interpreter) = &self.interpreter else {
            return Ok(Vec::new());
        };
        let mut devices = Vec::new();
        let mut namespace = interpreter.namespace.lock().clone();
        namespace
            .traverse(|path, level| {
                if level.kind != NamespaceLevelKind::Device {
                    return Ok(true);
                }
                let Some((hid, cids)) = acpi_device_ids(interpreter, path)? else {
                    return Ok(true);
                };
                let resources = read_device_resources(interpreter, path, &self.routing)?;
                devices.push(AcpiDeviceInfo {
                    path: path.as_string(),
                    hid: Some(hid),
                    cids,
                    memory_ranges: resources.memory_ranges,
                    io_ranges: resources.io_ranges,
                    irq_routes: resources.irq_routes,
                });
                Ok(true)
            })
            .map_err(|err| ProbeError::OnProbe(OnProbeError::other(format!("{err:?}"))))?;
        Ok(devices)
    }

    fn probe_register(
        &self,
        register: &DriverRegister,
    ) -> Result<Vec<Result<(), OnProbeError>>, ProbeError> {
        let mut out = Vec::new();
        for probe in register.probe_kinds {
            let ProbeKind::Acpi { ids, on_probe } = probe else {
                continue;
            };
            if self.probed_names.lock().contains(register.name) {
                continue;
            }

            if !ids.is_empty() && !is_root_acpi_id_list(ids) {
                for device in self.device_infos_for_ids(ids)? {
                    if self.probed_names.lock().contains(register.name) {
                        continue;
                    }
                    let desc = crate::Descriptor {
                        name: register.name,
                        device_id: DeviceId::new(),
                        irq_parent: None,
                    };
                    let device_id = desc.device_id();
                    let info = AcpiInfo {
                        root: self,
                        path: &device.path,
                        irq_route: device.irq_routes.first().copied(),
                        device: Some(&device),
                    };
                    let res = on_probe(ProbeAcpi::new(info, PlatformDevice::new(desc)));
                    if res.is_ok() {
                        self.probed_names.lock().insert(register.name);
                        self.note_populated_device(device_id, &device);
                    }
                    out.push(res);
                }
                continue;
            }

            let desc = crate::Descriptor {
                name: register.name,
                device_id: DeviceId::new(),
                irq_parent: None,
            };
            let info = AcpiInfo {
                root: self,
                path: "\\",
                irq_route: None,
                device: None,
            };
            let res = on_probe(ProbeAcpi::new(info, PlatformDevice::new(desc)));
            if res.is_ok() {
                self.probed_names.lock().insert(register.name);
            }
            out.push(res);
        }
        Ok(out)
    }

    fn note_populated_device(&self, device_id: DeviceId, device: &AcpiDeviceInfo) {
        self.populated_paths
            .lock()
            .insert(device.path.clone(), device_id);

        let mut resources = self.populated_resources.lock();
        for range in &device.memory_ranges {
            resources.insert(AcpiResourceAddress::memory(range.base), device_id);
        }
        for range in &device.io_ranges {
            resources.insert(AcpiResourceAddress::io(range.base), device_id);
        }
    }
}

fn is_supported_spcr_interface(interface: SpcrInterfaceType) -> bool {
    matches!(
        interface,
        SpcrInterfaceType::Full16550
            | SpcrInterfaceType::Full16450
            | SpcrInterfaceType::Generic16550
            | SpcrInterfaceType::ArmPL011
            | SpcrInterfaceType::ArmSBSAGeneric32bit
            | SpcrInterfaceType::ArmSBSAGeneric
    )
}

fn spcr_namespace_device_id(system: &System, spcr: &Spcr) -> Option<DeviceId> {
    let namespace = spcr.namespace_string().ok()?.trim_end_matches('\0');
    if namespace.is_empty() || namespace == "." {
        return None;
    }
    system.path_to_device_id(namespace)
}

fn spcr_resource_device_id(system: &System, spcr: &Spcr) -> Option<DeviceId> {
    let address = spcr.base_address()?.ok()?;
    let address = AcpiResourceAddress::from_generic_address(address)?;
    system.resource_address_to_device_id(address)
}

fn read_pci_ecam_regions(
    tables: &AcpiTables<AcpiHandler>,
) -> Result<Vec<AcpiPciEcam>, DriverError> {
    let regions = PciConfigRegions::new(tables).map_err(acpi_error)?;
    Ok(regions
        .regions
        .iter()
        .map(|region| AcpiPciEcam {
            segment_group: region.pci_segment_group,
            bus_start: region.bus_number_start,
            bus_end: region.bus_number_end,
            base_address: region.base_address,
        })
        .collect())
}

fn read_interrupt_routing(tables: &AcpiTables<AcpiHandler>) -> Result<AcpiRouting, DriverError> {
    let (model, _) = InterruptModel::new(tables).map_err(acpi_error)?;
    let mut routing = AcpiRouting::new();
    if let InterruptModel::Apic(apic) = model {
        for io_apic in &apic.io_apics {
            routing.add_io_apic(AcpiIoApic {
                id: io_apic.id,
                address: io_apic.address,
                gsi_base: io_apic.global_system_interrupt_base,
                redirection_entries: 24,
            });
        }
        for irq_override in &apic.interrupt_source_overrides {
            routing.add_isa_irq_override(AcpiIsaIrqOverride {
                source: irq_override.isa_source,
                gsi: irq_override.global_system_interrupt,
                trigger: match irq_override.trigger_mode {
                    TriggerMode::Edge => AcpiIrqTrigger::Edge,
                    TriggerMode::Level => AcpiIrqTrigger::Level,
                    _ => AcpiIrqTrigger::Edge,
                },
                polarity: match irq_override.polarity {
                    Polarity::ActiveHigh => AcpiIrqPolarity::ActiveHigh,
                    Polarity::ActiveLow => AcpiIrqPolarity::ActiveLow,
                    _ => AcpiIrqPolarity::ActiveHigh,
                },
            });
        }
    }
    read_loongarch_pch_pic_routing(tables, &mut routing);
    Ok(routing)
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RawMadtEntryHeader {
    entry_type: u8,
    length: u8,
}

#[repr(C, packed)]
#[derive(Clone, Copy)]
struct RawMadtBioPic {
    header: RawMadtEntryHeader,
    version: u8,
    address: u64,
    size: u16,
    id: u16,
    gsi_base: u16,
}

const ACPI_MADT_TYPE_BIO_PIC: u8 = 22;
const RAW_MADT_HEADER_LEN: usize = core::mem::size_of::<acpi::sdt::SdtHeader>() + 8;

fn read_loongarch_pch_pic_routing(tables: &AcpiTables<AcpiHandler>, routing: &mut AcpiRouting) {
    let Some(madt) = tables.find_table::<acpi::sdt::madt::Madt>() else {
        return;
    };

    let base = madt.virtual_start.as_ptr() as *const u8;
    let length = madt.region_length;
    if length < RAW_MADT_HEADER_LEN {
        return;
    }

    let mut offset = RAW_MADT_HEADER_LEN;
    while offset + core::mem::size_of::<RawMadtEntryHeader>() <= length {
        let header = unsafe { (base.add(offset) as *const RawMadtEntryHeader).read_unaligned() };
        let entry_len = usize::from(header.length);
        if entry_len < core::mem::size_of::<RawMadtEntryHeader>() || offset + entry_len > length {
            break;
        }

        if header.entry_type == ACPI_MADT_TYPE_BIO_PIC {
            read_loongarch_bio_pic_entry(base, offset, entry_len, routing);
        }

        offset += entry_len;
    }
}

fn read_loongarch_bio_pic_entry(
    base: *const u8,
    offset: usize,
    entry_len: usize,
    routing: &mut AcpiRouting,
) {
    if entry_len < core::mem::size_of::<RawMadtBioPic>() {
        warn!("ignore short LoongArch BIO_PIC MADT entry: {entry_len} bytes");
        return;
    }

    let entry = unsafe { (base.add(offset) as *const RawMadtBioPic).read_unaligned() };

    routing.add_pch_pic(AcpiPchPic {
        id: entry.id,
        address: entry.address,
        mmio_size: entry.size,
        gsi_count: LOONGARCH_PCH_PIC_GSI_COUNT,
        gsi_base: u32::from(entry.gsi_base),
    });
}

fn spcr_uart_register_size(access_size: u8) -> u64 {
    let access_bytes = match access_size {
        1 => 1,
        2 => 2,
        3 => 4,
        4 => 8,
        _ => 1,
    };
    access_bytes * 8
}

#[derive(Default)]
struct AcpiDeviceResources {
    memory_ranges: Vec<AcpiResourceRange>,
    io_ranges: Vec<AcpiResourceRange>,
    irq_routes: Vec<AcpiGsiRoute>,
}

fn is_root_acpi_id_list(ids: &[AcpiId]) -> bool {
    ids.iter().any(|id| id.hid == "ACPIIOAP")
}

fn acpi_ids_match(hid: &str, cids: &[String], ids: &[AcpiId]) -> bool {
    ids.iter().any(|id| {
        id.hid == hid
            || id.cids.contains(&hid)
            || cids
                .iter()
                .any(|cid| id.hid == cid || id.cids.contains(&cid.as_str()))
    })
}

fn acpi_device_ids(
    interpreter: &Interpreter<AcpiHandler>,
    path: &AmlName,
) -> Result<Option<(String, Vec<String>)>, AmlError> {
    let Some(hid_object) = eval_child(interpreter, path, "_HID")? else {
        return Ok(None);
    };
    let Some(hid) = acpi_id_from_object(&hid_object) else {
        return Ok(None);
    };
    let cids = match eval_child(interpreter, path, "_CID")? {
        Some(value) => acpi_ids_from_object(&value),
        None => Vec::new(),
    };
    Ok(Some((hid, cids)))
}

fn acpi_id_from_object(value: &Object) -> Option<String> {
    match value {
        Object::String(id) => Some(id.clone()),
        Object::Integer(id) => decode_eisa_id(*id as u32),
        _ => None,
    }
}

fn acpi_ids_from_object(value: &Object) -> Vec<String> {
    match value {
        Object::Package(values) => values
            .iter()
            .filter_map(|value| acpi_id_from_object(value))
            .collect(),
        _ => acpi_id_from_object(value).into_iter().collect(),
    }
}

fn read_device_resources(
    interpreter: &Interpreter<AcpiHandler>,
    path: &AmlName,
    routing: &AcpiRouting,
) -> Result<AcpiDeviceResources, AmlError> {
    let Some(value) = eval_wrapped_child(interpreter, path, "_CRS")? else {
        return Ok(AcpiDeviceResources::default());
    };
    let resources = resource_descriptor_list(value)?;
    let mut out = AcpiDeviceResources::default();
    for resource in resources {
        match resource {
            Resource::MemoryRange(memory) => match memory {
                acpi::aml::resource::MemoryRangeDescriptor::FixedLocation {
                    base_address,
                    range_length,
                    ..
                } => out.memory_ranges.push(AcpiResourceRange {
                    base: u64::from(base_address),
                    size: u64::from(range_length),
                }),
            },
            Resource::AddressSpace(address) => {
                let range = AcpiResourceRange {
                    base: address.address_range.0,
                    size: address.length,
                };
                match address.resource_type {
                    AddressSpaceResourceType::MemoryRange => out.memory_ranges.push(range),
                    AddressSpaceResourceType::IORange => out.io_ranges.push(range),
                    AddressSpaceResourceType::BusNumberRange => {}
                }
            }
            Resource::IOPort(io) => out.io_ranges.push(AcpiResourceRange {
                base: u64::from(io.memory_range.0),
                size: u64::from(io.range_length),
            }),
            Resource::Irq(irq) => {
                if let Some(gsi) = irq_descriptor_gsi(&irq)
                    && let Some(route) = routing.resolve_gsi(gsi)
                {
                    out.irq_routes
                        .push(route_with_irq_descriptor_flags(route, &irq));
                }
            }
            Resource::Dma(_) => {}
        }
    }
    Ok(out)
}

fn read_pci_namespace(
    interpreter: &Interpreter<AcpiHandler>,
) -> Result<AcpiPciNamespace, AcpiError> {
    let mut roots = Vec::new();
    {
        let mut namespace = interpreter.namespace.lock().clone();
        namespace
            .traverse(|path, level| {
                if level.kind == NamespaceLevelKind::Device && is_pci_root(interpreter, path) {
                    let segment =
                        eval_integer_child(interpreter, path, "_SEG")?.unwrap_or(0) as u16;
                    let bus = eval_integer_child(interpreter, path, "_BBN")?.unwrap_or(0) as u8;
                    roots.push(AcpiPciRoot {
                        segment,
                        bus,
                        path: path.as_string(),
                        prt: None,
                        link_prt: None,
                    });
                }
                Ok(true)
            })
            .map_err(AcpiError::Aml)?;
    }

    for root in &mut roots {
        root.prt = read_pci_routing_table(interpreter, &root.path)?;
        root.link_prt = read_pci_link_routing_table(interpreter, &root.path)?;
    }
    for path in PCI_ROOT_FALLBACK_PATHS {
        if roots.iter().any(|root| root.path == *path) {
            continue;
        }
        let Some(prt) = read_pci_routing_table(interpreter, path)? else {
            continue;
        };
        let link_prt = read_pci_link_routing_table(interpreter, path)?;
        roots.push(AcpiPciRoot {
            segment: 0,
            bus: 0,
            path: path.to_string(),
            prt: Some(prt),
            link_prt,
        });
    }

    Ok(AcpiPciNamespace {
        link_allocator: Mutex::new(PciLinkAllocator::default()),
        roots,
    })
}

fn read_pci_routing_table(
    interpreter: &Interpreter<AcpiHandler>,
    root_path: &str,
) -> Result<Option<PciRoutingTable>, AcpiError> {
    let prt_path = AmlName::from_str(&format!("{root_path}._PRT")).map_err(AcpiError::Aml)?;
    match PciRoutingTable::from_prt_path(prt_path, interpreter) {
        Ok(prt) => Ok(Some(prt)),
        Err(AmlError::ObjectDoesNotExist(_)) | Err(AmlError::LevelDoesNotExist(_)) => Ok(None),
        Err(err) => Err(AcpiError::Aml(err)),
    }
}

fn read_pci_link_routing_table(
    interpreter: &Interpreter<AcpiHandler>,
    root_path: &str,
) -> Result<Option<PciLinkRoutingTable>, AcpiError> {
    let prt_path = AmlName::from_str(&format!("{root_path}._PRT")).map_err(AcpiError::Aml)?;
    match PciLinkRoutingTable::from_prt_path(prt_path, interpreter) {
        Ok(prt) => Ok(Some(prt)),
        Err(AmlError::ObjectDoesNotExist(_)) | Err(AmlError::LevelDoesNotExist(_)) => Ok(None),
        Err(err) => Err(AcpiError::Aml(err)),
    }
}

fn is_pci_root(interpreter: &Interpreter<AcpiHandler>, path: &AmlName) -> bool {
    has_pci_root_id(interpreter, path, "_HID") || has_pci_root_id(interpreter, path, "_CID")
}

fn has_pci_root_id(interpreter: &Interpreter<AcpiHandler>, path: &AmlName, name: &str) -> bool {
    let Ok(Some(value)) = eval_child(interpreter, path, name) else {
        return false;
    };
    object_matches_pci_root_id(&value)
}

fn object_matches_pci_root_id(value: &Object) -> bool {
    match value {
        Object::String(id) => matches!(id.as_str(), "PNP0A03" | "PNP0A08"),
        Object::Integer(id) => {
            let id = decode_eisa_id(*id as u32);
            matches!(id.as_deref(), Some("PNP0A03" | "PNP0A08"))
        }
        Object::Package(values) => values.iter().any(|value| object_matches_pci_root_id(value)),
        _ => false,
    }
}

fn eval_integer_child(
    interpreter: &Interpreter<AcpiHandler>,
    path: &AmlName,
    name: &str,
) -> Result<Option<u64>, AmlError> {
    eval_child(interpreter, path, name)?
        .map(|value| value.as_integer())
        .transpose()
}

fn eval_child(
    interpreter: &Interpreter<AcpiHandler>,
    path: &AmlName,
    name: &str,
) -> Result<Option<Rc<Object>>, AmlError> {
    match eval_wrapped_child(interpreter, path, name)? {
        Some(value) => Ok(Some(Rc::new((*value).clone()))),
        None => Ok(None),
    }
}

fn eval_wrapped_child(
    interpreter: &Interpreter<AcpiHandler>,
    path: &AmlName,
    name: &str,
) -> Result<Option<acpi::aml::object::WrappedObject>, AmlError> {
    let child = AmlName::from_str(name)?.resolve(path)?;
    interpreter.evaluate_if_present(child, Vec::new())
}

fn decode_eisa_id(raw: u32) -> Option<String> {
    if raw == 0 {
        return None;
    }
    let bytes = raw.to_le_bytes();
    let chars = [
        ((bytes[0] >> 2) & 0x1f).wrapping_add(b'@'),
        (((bytes[0] & 0x03) << 3) | (bytes[1] >> 5)).wrapping_add(b'@'),
        (bytes[1] & 0x1f).wrapping_add(b'@'),
    ];
    if !chars.iter().all(u8::is_ascii_uppercase) {
        return None;
    }
    Some(format!(
        "{}{}{}{:02X}{:02X}",
        chars[0] as char, chars[1] as char, chars[2] as char, bytes[2], bytes[3]
    ))
}

#[derive(Debug)]
struct PciLinkRoutingTable {
    entries: Vec<PciLinkRoute>,
}

#[derive(Debug)]
struct PciLinkRoute {
    device: u16,
    function: u16,
    pin: Pin,
    link: AmlName,
    source_index: usize,
}

impl PciLinkRoutingTable {
    fn from_prt_path(
        prt_path: AmlName,
        interpreter: &Interpreter<AcpiHandler>,
    ) -> Result<Self, AmlError> {
        let prt = interpreter.evaluate(prt_path.clone(), Vec::new())?;
        let Object::Package(ref entries) = *prt else {
            return Err(AmlError::InvalidOperationOnObject {
                op: acpi::aml::Operation::DecodePrt,
                typ: prt.typ(),
            });
        };

        let mut routes = Vec::new();
        for entry in entries {
            let Object::Package(ref package) = **entry else {
                return Err(AmlError::InvalidOperationOnObject {
                    op: acpi::aml::Operation::DecodePrt,
                    typ: entry.typ(),
                });
            };
            if package.len() != 4 {
                return Err(AmlError::UnexpectedResourceType);
            }

            let Object::Integer(address) = *package[0] else {
                return Err(AmlError::PrtInvalidAddress);
            };
            let entry_device =
                u16::try_from((address >> 16) & 0xffff).map_err(|_| AmlError::PrtInvalidAddress)?;
            let entry_function =
                u16::try_from(address & 0xffff).map_err(|_| AmlError::PrtInvalidAddress)?;
            let entry_pin = match *package[1] {
                Object::Integer(0) => Pin::IntA,
                Object::Integer(1) => Pin::IntB,
                Object::Integer(2) => Pin::IntC,
                Object::Integer(3) => Pin::IntD,
                _ => return Err(AmlError::PrtInvalidPin),
            };
            let Object::String(ref source) = *package[2] else {
                continue;
            };
            let Object::Integer(source_index) = *package[3] else {
                return Err(AmlError::PrtInvalidSource);
            };
            let source_index =
                usize::try_from(source_index).map_err(|_| AmlError::PrtInvalidSource)?;
            let link = interpreter
                .namespace
                .lock()
                .search_for_level(&AmlName::from_str(source)?, &prt_path)?;
            routes.push(PciLinkRoute {
                device: entry_device,
                function: entry_function,
                pin: entry_pin,
                link,
                source_index,
            });
        }

        Ok(Self { entries: routes })
    }

    fn route(
        &self,
        device: u16,
        function: u16,
        pin: Pin,
        interpreter: &Interpreter<AcpiHandler>,
        handler: &AcpiHandler,
        allocator: &mut PciLinkAllocator,
    ) -> Result<Option<IrqDescriptor>, AmlError> {
        let Some(route) = self.entries.iter().find(|entry| {
            entry.device == device
                && (entry.function == 0xffff || entry.function == function)
                && entry.pin == pin
        }) else {
            return Ok(None);
        };

        resolve_pci_link_irq(
            interpreter,
            handler,
            allocator,
            &route.link,
            route.source_index,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinkIrqResourceKind {
    SmallIrq,
    ExtendedIrq,
}

#[derive(Clone, Debug)]
struct LinkIrqResource {
    kind: LinkIrqResourceKind,
    descriptor: IrqDescriptor,
    irqs: Vec<u32>,
}

struct LinkIrqSelection<'a> {
    resource: &'a LinkIrqResource,
    irq: u32,
    needs_programming: bool,
}

#[derive(Default)]
struct PciLinkAllocator {
    assigned: BTreeMap<String, u32>,
    irq_use_count: BTreeMap<u32, usize>,
}

impl PciLinkAllocator {
    fn assigned_irq(&self, link: &AmlName) -> Option<u32> {
        self.assigned.get(&link.as_string()).copied()
    }

    fn commit(&mut self, link: &AmlName, irq: u32) {
        let key = link.as_string();
        match self.assigned.insert(key, irq) {
            Some(old_irq) if old_irq == irq => return,
            Some(old_irq) => {
                if let Some(count) = self.irq_use_count.get_mut(&old_irq) {
                    *count = count.saturating_sub(1);
                }
            }
            None => {}
        }
        *self.irq_use_count.entry(irq).or_default() += 1;
    }

    fn penalty(&self, irq: u32) -> usize {
        let isa_penalty = match irq {
            0..=2 => usize::MAX / 2,
            3..=8 => 16 * 16 * 16 * 16,
            9..=11 => 0,
            12..=15 => 16 * 16 * 16 * 16 * 16,
            _ => 0,
        };
        isa_penalty + self.irq_use_count.get(&irq).copied().unwrap_or(0) * 16 * 16 * 16
    }
}

fn resolve_pci_link_irq(
    interpreter: &Interpreter<AcpiHandler>,
    handler: &AcpiHandler,
    allocator: &mut PciLinkAllocator,
    link: &AmlName,
    source_index: usize,
) -> Result<Option<IrqDescriptor>, AmlError> {
    let current = evaluate_link_irq_resource(interpreter, link, "_CRS", source_index)?;
    let possible = evaluate_link_irq_resource(interpreter, link, "_PRS", source_index)?;
    let Some(selection) = select_pci_link_irq(link, current.as_ref(), possible.as_ref(), allocator)
    else {
        return Ok(None);
    };

    if selection.needs_programming {
        let srs = build_link_srs_buffer(selection.resource, selection.irq)?;
        let srs_path = AmlName::from_str("_SRS")?.resolve(link)?;
        if let Err(err) = interpreter.evaluate(srs_path.clone(), vec![Object::Buffer(srs).wrap()]) {
            if is_buffer_field_to_field_unit_store_gap(&err) {
                let _ = interpreter.namespace.lock().remove_level(srs_path);
                // Current acpi's AML Store path cannot coerce a BufferField into a FieldUnit.
                // SeaBIOS PIRQ link _SRS methods use that exact pattern; program the already
                // decoded link field directly so PCI link allocation still follows _PRS/_SRS.
                program_known_pci_link_field(interpreter, handler, link, selection.irq)?;
            } else {
                let _ = interpreter.namespace.lock().remove_level(srs_path);
                return Err(err);
            }
        }
    }

    allocator.commit(link, selection.irq);
    Ok(Some(selection.resource.descriptor_for_irq(selection.irq)))
}

fn evaluate_link_irq_resource(
    interpreter: &Interpreter<AcpiHandler>,
    link: &AmlName,
    method: &str,
    source_index: usize,
) -> Result<Option<LinkIrqResource>, AmlError> {
    let path = AmlName::from_str(method)?.resolve(link)?;
    let value = match interpreter.evaluate_if_present(path.clone(), Vec::new()) {
        Ok(Some(value)) => value,
        Ok(None) => return Ok(None),
        Err(err) if method == "_CRS" && is_buffer_field_to_field_unit_store_gap(&err) => {
            let _ = interpreter.namespace.lock().remove_level(path);
            return Ok(None);
        }
        Err(err) => {
            let _ = interpreter.namespace.lock().remove_level(path);
            return Err(err);
        }
    };
    let resources = parse_link_irq_resources(&value)?;
    Ok(resources.into_iter().nth(source_index))
}

fn is_buffer_field_to_field_unit_store_gap(err: &AmlError) -> bool {
    matches!(
        err,
        AmlError::ObjectNotOfExpectedType {
            expected: ObjectType::Integer,
            got: ObjectType::BufferField,
        }
    )
}

fn program_known_pci_link_field(
    interpreter: &Interpreter<AcpiHandler>,
    handler: &AcpiHandler,
    link: &AmlName,
    irq: u32,
) -> Result<(), AmlError> {
    let Some(field_path) = known_pci_link_irq_field(interpreter, link) else {
        return Err(AmlError::ObjectNotOfExpectedType {
            expected: ObjectType::Integer,
            got: ObjectType::BufferField,
        });
    };
    let field = interpreter.namespace.lock().get(field_path)?.clone();
    let Object::FieldUnit(ref field) = *field else {
        return Err(AmlError::ObjectNotOfExpectedType {
            expected: ObjectType::FieldUnit,
            got: field.typ(),
        });
    };
    write_field_unit(interpreter, handler, field, irq as u64)
}

fn known_pci_link_irq_field(
    interpreter: &Interpreter<AcpiHandler>,
    link: &AmlName,
) -> Option<AmlName> {
    let mut namespace = interpreter.namespace.lock();
    for field in pci_link_irq_field_candidates(link) {
        let path = AmlName::from_str(field).ok()?;
        if namespace.get(path.clone()).is_ok() {
            return Some(path);
        }
    }
    None
}

fn pci_link_irq_field_candidates(link: &AmlName) -> &'static [&'static str] {
    match link.as_string().rsplit('.').next().unwrap_or_default() {
        "LNKA" => &["\\_SB.PRQA", "\\_SB.PRQ0"],
        "LNKB" => &["\\_SB.PRQB", "\\_SB.PRQ1"],
        "LNKC" => &["\\_SB.PRQC", "\\_SB.PRQ2"],
        "LNKD" => &["\\_SB.PRQD", "\\_SB.PRQ3"],
        "LNKE" => &["\\_SB.PRQE"],
        "LNKF" => &["\\_SB.PRQF"],
        "LNKG" => &["\\_SB.PRQG"],
        "LNKH" => &["\\_SB.PRQH"],
        _ => &[],
    }
}

fn write_field_unit(
    interpreter: &Interpreter<AcpiHandler>,
    handler: &AcpiHandler,
    field: &FieldUnit,
    value: u64,
) -> Result<(), AmlError> {
    let access_width_bits = field.flags.access_type_bytes()? * 8;
    let FieldUnitKind::Normal { ref region } = field.kind else {
        return Err(AmlError::LibUnimplemented);
    };
    let Object::OpRegion(ref region) = **region else {
        return Err(AmlError::ObjectNotOfExpectedType {
            expected: ObjectType::OpRegion,
            got: region.typ(),
        });
    };

    let value_bytes = value.to_le_bytes();
    let native_accesses =
        (field.bit_length + (field.bit_index % access_width_bits)).div_ceil(access_width_bits);
    let mut written_so_far = 0;

    for i in 0..native_accesses {
        let aligned_bit = align_down(field.bit_index + i * access_width_bits, access_width_bits);
        let dst_index = if i == 0 {
            field.bit_index % access_width_bits
        } else {
            0
        };
        let remaining = field.bit_length - written_so_far;
        let length = if i == 0 {
            usize::min(
                remaining,
                access_width_bits - (field.bit_index % access_width_bits),
            )
        } else {
            usize::min(remaining, access_width_bits)
        };

        let mut bytes = if dst_index > 0 || remaining < access_width_bits {
            match field.flags.update_rule() {
                FieldUpdateRule::Preserve => read_native_region(
                    interpreter,
                    handler,
                    region,
                    aligned_bit / 8,
                    access_width_bits / 8,
                )?
                .to_le_bytes(),
                FieldUpdateRule::WriteAsOnes => [0xff; 8],
                FieldUpdateRule::WriteAsZeros => [0; 8],
            }
        } else {
            [0; 8]
        };

        copy_bits(&value_bytes, written_so_far, &mut bytes, dst_index, length);
        write_native_region(
            interpreter,
            handler,
            region,
            aligned_bit / 8,
            access_width_bits / 8,
            u64::from_le_bytes(bytes),
        )?;
        written_so_far += length;
    }

    Ok(())
}

fn read_native_region(
    interpreter: &Interpreter<AcpiHandler>,
    handler: &AcpiHandler,
    region: &OpRegion,
    offset: usize,
    length: usize,
) -> Result<u64, AmlError> {
    match region.space {
        RegionSpace::SystemMemory => {
            let address = region.base as usize + offset;
            match length {
                1 => Ok(u64::from(handler.read_u8(address))),
                2 => Ok(u64::from(handler.read_u16(address))),
                4 => Ok(u64::from(handler.read_u32(address))),
                8 => Ok(handler.read_u64(address)),
                _ => Err(AmlError::InvalidFieldFlags),
            }
        }
        RegionSpace::SystemIO => {
            let address = region.base as u16 + offset as u16;
            match length {
                1 => Ok(u64::from(handler.read_io_u8(address))),
                2 => Ok(u64::from(handler.read_io_u16(address))),
                4 => Ok(u64::from(handler.read_io_u32(address))),
                _ => Err(AmlError::InvalidFieldFlags),
            }
        }
        RegionSpace::PciConfig => {
            let address = pci_address_for_region(interpreter, region)?;
            let offset = region.base as u16 + offset as u16;
            match length {
                1 => Ok(u64::from(handler.read_pci_u8(address, offset))),
                2 => Ok(u64::from(handler.read_pci_u16(address, offset))),
                4 => Ok(u64::from(handler.read_pci_u32(address, offset))),
                _ => Err(AmlError::InvalidFieldFlags),
            }
        }
        _ => Err(AmlError::NoHandlerForRegionAccess(region.space)),
    }
}

fn write_native_region(
    interpreter: &Interpreter<AcpiHandler>,
    handler: &AcpiHandler,
    region: &OpRegion,
    offset: usize,
    length: usize,
    value: u64,
) -> Result<(), AmlError> {
    match region.space {
        RegionSpace::SystemMemory => {
            let address = region.base as usize + offset;
            match length {
                1 => handler.write_u8(address, value as u8),
                2 => handler.write_u16(address, value as u16),
                4 => handler.write_u32(address, value as u32),
                8 => handler.write_u64(address, value),
                _ => return Err(AmlError::InvalidFieldFlags),
            }
            Ok(())
        }
        RegionSpace::SystemIO => {
            let address = region.base as u16 + offset as u16;
            match length {
                1 => handler.write_io_u8(address, value as u8),
                2 => handler.write_io_u16(address, value as u16),
                4 => handler.write_io_u32(address, value as u32),
                _ => return Err(AmlError::InvalidFieldFlags),
            }
            Ok(())
        }
        RegionSpace::PciConfig => {
            let address = pci_address_for_region(interpreter, region)?;
            let offset = region.base as u16 + offset as u16;
            match length {
                1 => handler.write_pci_u8(address, offset, value as u8),
                2 => handler.write_pci_u16(address, offset, value as u16),
                4 => handler.write_pci_u32(address, offset, value as u32),
                _ => return Err(AmlError::InvalidFieldFlags),
            }
            Ok(())
        }
        _ => Err(AmlError::NoHandlerForRegionAccess(region.space)),
    }
}

fn pci_address_for_region(
    interpreter: &Interpreter<AcpiHandler>,
    region: &OpRegion,
) -> Result<acpi::PciAddress, AmlError> {
    let path = &region.parent_device_path;
    let segment = eval_integer_child(interpreter, path, "_SEG")?.unwrap_or(0) as u16;
    let bus = eval_integer_child(interpreter, path, "_BBN")?.unwrap_or(0) as u8;
    let address = eval_integer_child(interpreter, path, "_ADR")?.unwrap_or(0);
    Ok(acpi::PciAddress::new(
        segment,
        bus,
        ((address >> 16) & 0xff) as u8,
        (address & 0xff) as u8,
    ))
}

fn copy_bits(src: &[u8], src_offset: usize, dst: &mut [u8], dst_offset: usize, length: usize) {
    for bit in 0..length {
        let src_bit = src_offset + bit;
        let dst_bit = dst_offset + bit;
        let is_set = src[src_bit / 8] & (1 << (src_bit % 8)) != 0;
        if is_set {
            dst[dst_bit / 8] |= 1 << (dst_bit % 8);
        } else {
            dst[dst_bit / 8] &= !(1 << (dst_bit % 8));
        }
    }
}

fn align_down(value: usize, align: usize) -> usize {
    assert!(align.is_power_of_two());
    value & !(align - 1)
}

fn parse_link_irq_resources(
    value: &acpi::aml::object::WrappedObject,
) -> Result<Vec<LinkIrqResource>, AmlError> {
    let Object::Buffer(ref bytes) = **value else {
        return Err(AmlError::InvalidOperationOnObject {
            op: acpi::aml::Operation::ParseResource,
            typ: value.typ(),
        });
    };

    let mut resources = Vec::new();
    let mut rest = bytes.as_slice();
    while !rest.is_empty() {
        if rest[0] & 0x80 != 0 {
            if rest.len() < 3 {
                return Err(AmlError::InvalidResourceDescriptor);
            }
            let length = usize::from(u16::from_le_bytes([rest[1], rest[2]]));
            if rest.len() < length + 3 {
                return Err(AmlError::InvalidResourceDescriptor);
            }
            let descriptor = &rest[..length + 3];
            if rest[0] & 0x7f == 0x09 {
                resources.push(parse_extended_irq_resource(descriptor)?);
            }
            rest = &rest[length + 3..];
        } else {
            let length = usize::from(rest[0] & 0b111);
            if rest.len() < length + 1 {
                return Err(AmlError::InvalidResourceDescriptor);
            }
            let descriptor = &rest[..length + 1];
            match (rest[0] >> 3) & 0x0f {
                0x04 => resources.push(parse_small_irq_resource(descriptor)?),
                0x0f => break,
                _ => {}
            }
            rest = &rest[length + 1..];
        }
    }
    Ok(resources)
}

fn parse_extended_irq_resource(bytes: &[u8]) -> Result<LinkIrqResource, AmlError> {
    if bytes.len() < 5 {
        return Err(AmlError::InvalidResourceDescriptor);
    }
    let count = usize::from(bytes[4]);
    if bytes.len() < 5 + count * 4 {
        return Err(AmlError::InvalidResourceDescriptor);
    }

    let mut irqs = Vec::new();
    for idx in 0..count {
        let start = 5 + idx * 4;
        irqs.push(u32::from_le_bytes([
            bytes[start],
            bytes[start + 1],
            bytes[start + 2],
            bytes[start + 3],
        ]));
    }

    Ok(LinkIrqResource {
        kind: LinkIrqResourceKind::ExtendedIrq,
        descriptor: IrqDescriptor {
            is_consumer: bytes[3] & 0b0000_0001 != 0,
            trigger: if bytes[3] & 0b0000_0010 != 0 {
                InterruptTrigger::Edge
            } else {
                InterruptTrigger::Level
            },
            polarity: if bytes[3] & 0b0000_0100 != 0 {
                InterruptPolarity::ActiveLow
            } else {
                InterruptPolarity::ActiveHigh
            },
            is_shared: bytes[3] & 0b0000_1000 != 0,
            is_wake_capable: bytes[3] & 0b0001_0000 != 0,
            irq: irqs.first().copied().unwrap_or(0),
        },
        irqs,
    })
}

fn parse_small_irq_resource(bytes: &[u8]) -> Result<LinkIrqResource, AmlError> {
    if bytes.len() != 3 && bytes.len() != 4 {
        return Err(AmlError::InvalidResourceDescriptor);
    }

    let mask = u16::from_le_bytes([bytes[1], bytes[2]]);
    let mut irqs = Vec::new();
    for irq in 0..16 {
        if mask & (1 << irq) != 0 {
            irqs.push(irq);
        }
    }

    let (trigger, polarity, is_shared, is_wake_capable) = if bytes.len() == 4 {
        (
            if bytes[3] & 0b0000_0001 != 0 {
                InterruptTrigger::Edge
            } else {
                InterruptTrigger::Level
            },
            if bytes[3] & 0b0000_1000 != 0 {
                InterruptPolarity::ActiveLow
            } else {
                InterruptPolarity::ActiveHigh
            },
            bytes[3] & 0b0001_0000 != 0,
            bytes[3] & 0b0010_0000 != 0,
        )
    } else {
        (
            InterruptTrigger::Edge,
            InterruptPolarity::ActiveHigh,
            false,
            false,
        )
    };

    Ok(LinkIrqResource {
        kind: LinkIrqResourceKind::SmallIrq,
        descriptor: IrqDescriptor {
            is_consumer: false,
            trigger,
            polarity,
            is_shared,
            is_wake_capable,
            irq: u32::from(mask),
        },
        irqs,
    })
}

fn select_pci_link_irq<'a>(
    link: &AmlName,
    current: Option<&'a LinkIrqResource>,
    possible: Option<&'a LinkIrqResource>,
    allocator: &PciLinkAllocator,
) -> Option<LinkIrqSelection<'a>> {
    if let Some(irq) = allocator.assigned_irq(link) {
        let resource = possible
            .filter(|possible| possible.irqs.contains(&irq))
            .or(current.filter(|current| current.irqs.contains(&irq)))?;
        return Some(LinkIrqSelection {
            resource,
            irq,
            needs_programming: current.is_none_or(|current| current.irqs.first() != Some(&irq)),
        });
    }

    if let Some(current) = current
        && let Some(irq) = current.irqs.first().copied().filter(|irq| *irq != 0)
        && possible.is_none_or(|possible| possible.irqs.contains(&irq))
    {
        return Some(LinkIrqSelection {
            resource: possible.unwrap_or(current),
            irq,
            needs_programming: false,
        });
    }

    let possible = possible?;
    let irq = preferred_pci_link_irq(&possible.irqs, allocator)?;
    Some(LinkIrqSelection {
        resource: possible,
        irq,
        needs_programming: true,
    })
}

fn preferred_pci_link_irq(irqs: &[u32], allocator: &PciLinkAllocator) -> Option<u32> {
    irqs.iter()
        .copied()
        .filter(|irq| *irq != 0)
        .min_by_key(|irq| (allocator.penalty(*irq), link_irq_tiebreaker(*irq)))
}

fn link_irq_tiebreaker(irq: u32) -> usize {
    match irq {
        10 => 0,
        11 => 1,
        9 => 2,
        16.. => 3 + irq as usize,
        _ => 1024 + irq as usize,
    }
}

fn build_link_srs_buffer(resource: &LinkIrqResource, irq: u32) -> Result<Vec<u8>, AmlError> {
    match resource.kind {
        LinkIrqResourceKind::ExtendedIrq => {
            let mut buffer = Vec::new();
            buffer.extend_from_slice(&[
                0x89,
                0x06,
                0x00,
                extended_irq_flags(&resource.descriptor),
                1,
            ]);
            buffer.extend_from_slice(&irq.to_le_bytes());
            buffer.extend_from_slice(&[0x79, 0x00]);
            Ok(buffer)
        }
        LinkIrqResourceKind::SmallIrq => {
            if irq >= 16 {
                return Err(AmlError::InvalidResourceDescriptor);
            }
            let mask = 1u16 << irq;
            let mut buffer = Vec::new();
            buffer.push((0x04 << 3) | 3);
            buffer.extend_from_slice(&mask.to_le_bytes());
            buffer.push(small_irq_flags(&resource.descriptor));
            buffer.extend_from_slice(&[0x79, 0x00]);
            Ok(buffer)
        }
    }
}

impl LinkIrqResource {
    fn descriptor_for_irq(&self, irq: u32) -> IrqDescriptor {
        let mut descriptor = self.descriptor.clone();
        descriptor.irq = irq;
        descriptor
    }
}

fn extended_irq_flags(descriptor: &IrqDescriptor) -> u8 {
    let mut flags = 0;
    if descriptor.is_consumer {
        flags |= 1 << 0;
    }
    if descriptor.trigger == InterruptTrigger::Edge {
        flags |= 1 << 1;
    }
    if descriptor.polarity == InterruptPolarity::ActiveLow {
        flags |= 1 << 2;
    }
    if descriptor.is_shared {
        flags |= 1 << 3;
    }
    if descriptor.is_wake_capable {
        flags |= 1 << 4;
    }
    flags
}

fn small_irq_flags(descriptor: &IrqDescriptor) -> u8 {
    let mut flags = 0;
    if descriptor.trigger == InterruptTrigger::Edge {
        flags |= 1 << 0;
    }
    if descriptor.polarity == InterruptPolarity::ActiveLow {
        flags |= 1 << 3;
    }
    if descriptor.is_shared {
        flags |= 1 << 4;
    }
    if descriptor.is_wake_capable {
        flags |= 1 << 5;
    }
    flags
}

fn acpi_pin(interrupt_pin: u8) -> Result<Pin, OnProbeError> {
    match interrupt_pin {
        1 => Ok(Pin::IntA),
        2 => Ok(Pin::IntB),
        3 => Ok(Pin::IntC),
        4 => Ok(Pin::IntD),
        _ => Err(OnProbeError::other(format!(
            "invalid PCI interrupt pin {interrupt_pin}"
        ))),
    }
}

fn irq_descriptor_gsi(descriptor: &IrqDescriptor) -> Option<u32> {
    let irq = descriptor.irq;
    if !descriptor.is_consumer && irq.count_ones() == 1 && irq <= u16::MAX as u32 {
        Some(irq.trailing_zeros())
    } else {
        Some(irq)
    }
}

fn pci_irq_descriptor_gsi(descriptor: &IrqDescriptor) -> Option<u32> {
    Some(descriptor.irq)
}

fn route_with_irq_descriptor_flags(
    route: AcpiGsiRoute,
    descriptor: &IrqDescriptor,
) -> AcpiGsiRoute {
    AcpiGsiRoute {
        trigger: irq_trigger(descriptor.trigger),
        polarity: irq_polarity(descriptor.polarity),
        ..route
    }
}

fn irq_trigger(trigger: acpi::aml::resource::InterruptTrigger) -> AcpiIrqTrigger {
    match trigger {
        acpi::aml::resource::InterruptTrigger::Edge => AcpiIrqTrigger::Edge,
        acpi::aml::resource::InterruptTrigger::Level => AcpiIrqTrigger::Level,
    }
}

fn irq_polarity(polarity: acpi::aml::resource::InterruptPolarity) -> AcpiIrqPolarity {
    match polarity {
        acpi::aml::resource::InterruptPolarity::ActiveHigh => AcpiIrqPolarity::ActiveHigh,
        acpi::aml::resource::InterruptPolarity::ActiveLow => AcpiIrqPolarity::ActiveLow,
    }
}

#[cfg(target_arch = "x86_64")]
fn pci_legacy_read_u8(address: acpi::PciAddress, offset: u16) -> Option<u8> {
    let value = pci_legacy_read_aligned_u32(address, offset)?;
    let shift = u32::from(offset & 0b11) * 8;
    Some((value >> shift) as u8)
}

#[cfg(not(target_arch = "x86_64"))]
fn pci_legacy_read_u8(_address: acpi::PciAddress, _offset: u16) -> Option<u8> {
    None
}

#[cfg(target_arch = "x86_64")]
fn pci_legacy_write_u8(address: acpi::PciAddress, offset: u16, value: u8) {
    let Some(old) = pci_legacy_read_aligned_u32(address, offset) else {
        return;
    };
    let shift = u32::from(offset & 0b11) * 8;
    let mask = 0xff_u32 << shift;
    let new = (old & !mask) | (u32::from(value) << shift);
    pci_legacy_write_aligned_u32(address, offset, new);
}

#[cfg(not(target_arch = "x86_64"))]
fn pci_legacy_write_u8(_address: acpi::PciAddress, _offset: u16, _value: u8) {}

#[cfg(target_arch = "x86_64")]
fn pci_legacy_config_address(address: acpi::PciAddress, offset: u16) -> Option<u32> {
    if address.segment() != 0 || offset >= 256 {
        return None;
    }

    Some(
        0x8000_0000
            | (u32::from(address.bus()) << 16)
            | (u32::from(address.device()) << 11)
            | (u32::from(address.function()) << 8)
            | u32::from(offset & !0b11),
    )
}

#[cfg(target_arch = "x86_64")]
fn pci_legacy_read_aligned_u32(address: acpi::PciAddress, offset: u16) -> Option<u32> {
    let config_address = pci_legacy_config_address(address, offset)?;
    unsafe {
        x86::io::outl(0xcf8, config_address);
        Some(x86::io::inl(0xcfc))
    }
}

#[cfg(target_arch = "x86_64")]
fn pci_legacy_write_aligned_u32(address: acpi::PciAddress, offset: u16, value: u32) {
    if let Some(config_address) = pci_legacy_config_address(address, offset) {
        unsafe {
            x86::io::outl(0xcf8, config_address);
            x86::io::outl(0xcfc, value);
        }
    }
}

pub fn acpi_trigger(trigger: TriggerMode) -> AcpiIrqTrigger {
    match trigger {
        TriggerMode::Edge => AcpiIrqTrigger::Edge,
        TriggerMode::Level => AcpiIrqTrigger::Level,
        _ => AcpiIrqTrigger::Level,
    }
}

pub fn acpi_polarity(polarity: Polarity) -> AcpiIrqPolarity {
    match polarity {
        Polarity::ActiveHigh => AcpiIrqPolarity::ActiveHigh,
        Polarity::ActiveLow => AcpiIrqPolarity::ActiveLow,
        _ => AcpiIrqPolarity::ActiveLow,
    }
}

#[cfg(target_arch = "x86_64")]
fn read_io_u8(port: u16) -> u8 {
    unsafe { x86::io::inb(port) }
}

#[cfg(not(target_arch = "x86_64"))]
fn read_io_u8(_port: u16) -> u8 {
    0
}

#[cfg(target_arch = "x86_64")]
fn read_io_u16(port: u16) -> u16 {
    unsafe { x86::io::inw(port) }
}

#[cfg(not(target_arch = "x86_64"))]
fn read_io_u16(_port: u16) -> u16 {
    0
}

#[cfg(target_arch = "x86_64")]
fn read_io_u32(port: u16) -> u32 {
    unsafe { x86::io::inl(port) }
}

#[cfg(not(target_arch = "x86_64"))]
fn read_io_u32(_port: u16) -> u32 {
    0
}

#[cfg(target_arch = "x86_64")]
fn write_io_u8(port: u16, value: u8) {
    unsafe { x86::io::outb(port, value) }
}

#[cfg(not(target_arch = "x86_64"))]
fn write_io_u8(_port: u16, _value: u8) {}

#[cfg(target_arch = "x86_64")]
fn write_io_u16(port: u16, value: u16) {
    unsafe { x86::io::outw(port, value) }
}

#[cfg(not(target_arch = "x86_64"))]
fn write_io_u16(_port: u16, _value: u16) {}

#[cfg(target_arch = "x86_64")]
fn write_io_u32(port: u16, value: u32) {
    unsafe { x86::io::outl(port, value) }
}

#[cfg(not(target_arch = "x86_64"))]
fn write_io_u32(_port: u16, _value: u32) {}
