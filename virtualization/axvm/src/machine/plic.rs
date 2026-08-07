//! Host-derived guest PLIC firmware identity.

use std::string::String;

const CONTEXT_CONTROL_OFFSET: usize = 0x20_0000;
const CONTEXT_STRIDE: usize = 0x1000;
const CLAIM_COMPLETE_SIZE: usize = 8;

/// Host firmware resources retained by the virtual PLIC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GuestPlicProfile {
    /// Absolute path of the host PLIC node.
    pub node_path: String,
    /// PLIC node phandle, when supplied by firmware.
    pub node_phandle: Option<u32>,
    /// Guest-visible PLIC register base.
    pub base: usize,
    /// Guest-visible PLIC register span.
    pub length: usize,
}

impl GuestPlicProfile {
    pub(crate) fn validate_for_vcpus(
        &self,
        vcpu_count: usize,
    ) -> Result<(), GuestPlicProfileError> {
        let contexts = vcpu_count
            .max(1)
            .checked_mul(2)
            .ok_or(GuestPlicProfileError::ContextCountOverflow)?;
        let minimum_length = contexts
            .checked_mul(CONTEXT_STRIDE)
            .and_then(|offset| offset.checked_add(CONTEXT_CONTROL_OFFSET))
            .and_then(|offset| offset.checked_add(CLAIM_COMPLETE_SIZE))
            .ok_or(GuestPlicProfileError::WindowSizeOverflow)?;
        if self.length < minimum_length {
            return Err(GuestPlicProfileError::WindowTooSmall {
                length: self.length,
                minimum: minimum_length,
            });
        }
        Ok(())
    }
}

/// Invalid host-derived PLIC firmware geometry.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GuestPlicProfileError {
    /// Two PLIC contexts per vCPU cannot be represented by the host width.
    #[error("RISC-V PLIC context count overflows usize")]
    ContextCountOverflow,
    /// The register window required for all contexts cannot be represented.
    #[error("RISC-V PLIC context window size overflows usize")]
    WindowSizeOverflow,
    /// The host window does not contain all guest-visible contexts.
    #[error("RISC-V PLIC window {length:#x} is smaller than required size {minimum:#x}")]
    WindowTooSmall { length: usize, minimum: usize },
}
