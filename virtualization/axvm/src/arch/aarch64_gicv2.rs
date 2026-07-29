//! Testable GICv2 direct-injection policy for the AArch64 adapter.

use arm_gic_driver::{
    IntId,
    v2::{VirtualInterruptConfig, VirtualInterruptState},
};

pub(super) fn direct_injection_config(virtual_id: IntId) -> VirtualInterruptConfig {
    VirtualInterruptConfig::software(
        virtual_id,
        None,
        0,
        VirtualInterruptState::Pending,
        false,
        true,
    )
}

#[cfg(test)]
mod tests {
    use arm_gic_driver::v2::VirtualInterruptType;

    use super::*;

    #[test]
    fn direct_injection_preserves_legacy_group_and_eoi_policy() {
        // SAFETY: 32 is a valid traditional GIC SPI INTID.
        let config = direct_injection_config(unsafe { IntId::raw(32) });

        assert_eq!(config.virtual_id.to_u32(), 32);
        assert_eq!(config.priority, 0);
        assert!(matches!(config.state, VirtualInterruptState::Pending));
        assert!(!config.group1);
        assert!(matches!(
            config.interrupt_type,
            VirtualInterruptType::Software {
                cpu_id: None,
                eoi_maintenance: true,
            }
        ));
    }
}
