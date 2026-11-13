//! Physical resources that have no device-level DMA retirement proof.

use crate::{
    AxVmError,
    config::AxVMConfig,
    layout::{VmAddressLayout, VmRegionKind},
};

/// A resource whose ownership must survive guest execution shutdown.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HostDmaBlocker {
    UnpreparedAddressSpace,
    HostMapping {
        guest_base: crate::GuestPhysAddr,
        host_base: crate::HostPhysAddr,
        length: usize,
    },
    Port {
        base: u16,
        length: u16,
    },
}

impl HostDmaBlocker {
    pub(crate) fn into_error(self, operation: &'static str) -> AxVmError {
        let detail = match self {
            Self::UnpreparedAddressSpace => {
                "guest address layout is unavailable; DMA retirement cannot be proven".into()
            }
            Self::HostMapping {
                guest_base,
                host_base,
                length,
            } => std::format!(
                "direct host mapping GPA={:#x} HPA={:#x} length={length:#x} has no DMA retirement \
                 capability",
                guest_base.as_usize(),
                host_base.as_usize(),
            ),
            Self::Port { base, length } => std::format!(
                "host I/O ports base={base:#x} length={length:#x} have no DMA retirement \
                 capability"
            ),
        };
        AxVmError::unsupported(operation, detail)
    }
}

pub(super) fn host_dma_blocker(
    config: &AxVMConfig,
    layout: Option<&VmAddressLayout>,
) -> Option<HostDmaBlocker> {
    let Some(layout) = layout else {
        return Some(HostDmaBlocker::UnpreparedAddressSpace);
    };
    // The planner removes modeled replacements from physical mappings. Raw
    // configuration alone cannot distinguish those from direct host access.
    if let Some(mapping) = layout
        .mappings()
        .iter()
        .find(|mapping| mapping.kind == VmRegionKind::Passthrough)
    {
        return Some(HostDmaBlocker::HostMapping {
            guest_base: mapping.gpa,
            host_base: mapping.hpa,
            length: mapping.size,
        });
    }
    // Every configured host port range is built as a physical forwarding
    // device. Its graph node has no device-level DMA retirement capability.
    config
        .pass_through_ports()
        .first()
        .map(|port| HostDmaBlocker::Port {
            base: port.base,
            length: port.length,
        })
}
