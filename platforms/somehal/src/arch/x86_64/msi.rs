use alloc::{collections::BTreeMap, vec::Vec};

use irq_framework::{HwIrq, IrqAffinity, IrqError, IrqId};
use rdif_msi::{
    Interface, Msi, MsiAllocation, MsiEventId, MsiMessage, MsiProviderId, MsiRequest, MsiVector,
    MsiVectorIndex,
};
use rdrive::DeviceId;

use super::IOAPIC_CPU_IF;

const MSI_ADDRESS_BASE: u64 = 0xfee0_0000;
// Keep MSI vectors separate from QEMU's legacy I/O APIC range and from the
// runtime-reserved local APIC vectors at the top of the vector table.
const MSI_VECTOR_START: u8 = 0x80;
const MSI_VECTOR_END: u8 = 0xf2;

#[derive(Clone, Copy)]
struct MsiRoute {
    leaf_irq: IrqId,
    destination: u8,
}

pub(super) struct X86MsiProvider {
    owner: DeviceId,
    parent_domain: irq_framework::IrqDomainId,
    msix_domain: irq_framework::IrqDomainId,
    next_leaf: u32,
    routes: BTreeMap<u8, MsiRoute>,
}

impl X86MsiProvider {
    pub(super) fn new(owner: DeviceId) -> Result<Self, IrqError> {
        let parent_domain = crate::irq::alloc_irq_domain(owner, crate::irq::IrqDomainKind::X86Msi)?;
        let msix_domain = crate::irq::alloc_child_irq_domain(
            owner,
            parent_domain,
            crate::irq::IrqDomainKind::PciMsix,
        )?;
        Ok(Self {
            owner,
            parent_domain,
            msix_domain,
            next_leaf: 0,
            routes: BTreeMap::new(),
        })
    }

    pub(super) fn provider_id(&self) -> MsiProviderId {
        MsiProviderId(u64::from(self.owner))
    }

    fn destination(affinity: IrqAffinity) -> Result<u8, IrqError> {
        let cpu_id = match affinity {
            IrqAffinity::Any => 0,
            IrqAffinity::Fixed(cpu_id) => cpu_id.0,
        };
        let apic_id = someboot::smp::cpu_idx_to_id(cpu_id).ok_or(IrqError::InvalidCpu)?;
        u8::try_from(apic_id).map_err(|_| IrqError::InvalidCpu)
    }

    fn reserve_vector(&self) -> Result<(u8, IrqId), IrqError> {
        for vector in available_vectors(&self.routes) {
            let parent_irq = IrqId::new(self.parent_domain, HwIrq(u32::from(vector)));
            match IOAPIC_CPU_IF.remember_vector_route(usize::from(vector), parent_irq) {
                Ok(_) => return Ok((vector, parent_irq)),
                Err(IrqError::Busy) => continue,
                Err(error) => return Err(error),
            }
        }
        Err(IrqError::NoMemory)
    }

    fn route_for(&self, vector: &MsiVector) -> Result<(u8, MsiRoute), IrqError> {
        if vector.parent_irq.domain != self.parent_domain {
            return Err(IrqError::InvalidIrq);
        }
        let raw = u8::try_from(vector.parent_irq.hwirq.0).map_err(|_| IrqError::InvalidIrq)?;
        let route = *self.routes.get(&raw).ok_or(IrqError::InvalidIrq)?;
        if route.leaf_irq != vector.irq {
            return Err(IrqError::InvalidIrq);
        }
        Ok((raw, route))
    }

    fn set_parent_affinity(
        &mut self,
        parent_irq: IrqId,
        affinity: crate::irq::IrqAffinity,
    ) -> Result<(), IrqError> {
        if parent_irq.domain != self.parent_domain {
            return Err(IrqError::InvalidIrq);
        }
        let vector = u8::try_from(parent_irq.hwirq.0).map_err(|_| IrqError::InvalidIrq)?;
        let destination = Self::destination(match affinity {
            crate::irq::IrqAffinity::Any => IrqAffinity::Any,
            crate::irq::IrqAffinity::Fixed { cpu_id } => {
                IrqAffinity::Fixed(irq_framework::CpuId(cpu_id))
            }
        })?;
        self.routes
            .get_mut(&vector)
            .ok_or(IrqError::InvalidIrq)?
            .destination = destination;
        Ok(())
    }

    fn release_vector(&mut self, vector: &MsiVector) -> Result<(), IrqError> {
        let (raw, _) = self.route_for(vector)?;
        crate::irq::unmap_irq_route(vector.parent_irq, vector.irq)?;
        IOAPIC_CPU_IF.forget_vector_route(usize::from(raw), vector.parent_irq)?;
        self.routes.remove(&raw);
        Ok(())
    }
}

fn available_vectors(routes: &BTreeMap<u8, MsiRoute>) -> impl Iterator<Item = u8> + '_ {
    (MSI_VECTOR_START..=MSI_VECTOR_END).filter(|vector| !routes.contains_key(vector))
}

impl rdif_msi::DriverGeneric for X86MsiProvider {
    fn name(&self) -> &str {
        "x86-lapic-msi"
    }

    fn raw_any(&self) -> Option<&dyn core::any::Any> {
        Some(self)
    }

    fn raw_any_mut(&mut self) -> Option<&mut dyn core::any::Any> {
        Some(self)
    }
}

impl Interface for X86MsiProvider {
    fn allocate_vectors(&mut self, request: &MsiRequest) -> Result<Vec<MsiVector>, IrqError> {
        if request.vector_count == 0 {
            return Err(IrqError::InvalidIrq);
        }
        let destination = Self::destination(request.affinity)?;
        let mut vectors = Vec::with_capacity(usize::from(request.vector_count));
        for index in 0..request.vector_count {
            let (raw, parent_irq) = match self.reserve_vector() {
                Ok(vector) => vector,
                Err(error) => {
                    for vector in vectors.iter().rev() {
                        let _ = self.release_vector(vector);
                    }
                    return Err(error);
                }
            };
            let leaf_hwirq = self.next_leaf;
            self.next_leaf = match self.next_leaf.checked_add(1) {
                Some(next) => next,
                None => {
                    let _ = IOAPIC_CPU_IF.forget_vector_route(usize::from(raw), parent_irq);
                    for vector in vectors.iter().rev() {
                        let _ = self.release_vector(vector);
                    }
                    return Err(IrqError::NoMemory);
                }
            };
            let leaf_irq = IrqId::new(self.msix_domain, HwIrq(leaf_hwirq));
            if let Err(error) = crate::irq::map_irq_route(parent_irq, leaf_irq) {
                let _ = IOAPIC_CPU_IF.forget_vector_route(usize::from(raw), parent_irq);
                for vector in vectors.iter().rev() {
                    let _ = self.release_vector(vector);
                }
                return Err(error);
            }
            self.routes.insert(
                raw,
                MsiRoute {
                    leaf_irq,
                    destination,
                },
            );
            vectors.push(MsiVector::with_parent(
                MsiVectorIndex(index),
                MsiEventId(u32::from(raw)),
                leaf_irq,
                parent_irq,
            ));
        }
        Ok(vectors)
    }

    fn compose_message(&self, vector: &MsiVector) -> Result<MsiMessage, IrqError> {
        let (raw, route) = self.route_for(vector)?;
        Ok(msi_message(raw, route.destination))
    }

    fn set_vector_enabled(&mut self, vector: &MsiVector, _enabled: bool) -> Result<(), IrqError> {
        self.route_for(vector).map(|_| ())
    }

    fn set_vector_affinity(
        &mut self,
        vector: &MsiVector,
        affinity: IrqAffinity,
    ) -> Result<(), IrqError> {
        let (raw, _) = self.route_for(vector)?;
        self.routes
            .get_mut(&raw)
            .ok_or(IrqError::InvalidIrq)?
            .destination = Self::destination(affinity)?;
        Ok(())
    }

    fn free_vectors(&mut self, allocation: MsiAllocation) -> Result<(), IrqError> {
        for vector in allocation.vectors() {
            self.route_for(vector)?;
        }
        for vector in allocation.vectors() {
            self.release_vector(vector)?;
        }
        Ok(())
    }
}

fn msi_message(vector: u8, destination: u8) -> MsiMessage {
    MsiMessage::new(
        MSI_ADDRESS_BASE | (u64::from(destination) << 12),
        u32::from(vector),
    )
}

pub(super) fn set_irq_affinity(
    irq: IrqId,
    affinity: crate::irq::IrqAffinity,
) -> Result<(), IrqError> {
    let domain = crate::irq::domain_by_id(irq.domain).ok_or(IrqError::InvalidIrq)?;
    if domain.kind != crate::irq::IrqDomainKind::X86Msi {
        return Err(IrqError::InvalidIrq);
    }
    let provider = rdrive::get::<Msi>(domain.owner).map_err(|_| IrqError::Unsupported)?;
    let mut provider = provider.try_lock().map_err(|_| IrqError::Busy)?;
    provider
        .typed_mut::<X86MsiProvider>()
        .ok_or(IrqError::Unsupported)?
        .set_parent_affinity(irq, affinity)
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;

    use irq_framework::{HwIrq, IrqDomainId, IrqId};

    use super::{MSI_ADDRESS_BASE, MsiRoute, available_vectors, msi_message};

    #[test]
    fn message_encodes_fixed_apic_destination_and_vector() {
        let message = msi_message(0x81, 7);

        assert_eq!(message.address, MSI_ADDRESS_BASE | (7 << 12));
        assert_eq!(message.data, 0x81);
    }

    #[test]
    fn vector_allocator_does_not_reuse_a_route_owned_by_the_provider() {
        let mut routes = BTreeMap::new();
        routes.insert(
            0x80,
            MsiRoute {
                leaf_irq: IrqId::new(IrqDomainId(9), HwIrq(0)),
                destination: 0,
            },
        );

        assert_eq!(available_vectors(&routes).next(), Some(0x81));
    }
}
