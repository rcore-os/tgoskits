//! Guest hypercall decoding, separated from lifecycle and mapping ownership.

use std::string::ToString;

use axhvc::{HyperCallCode, HyperCallError, HyperCallResult};

use crate::{AxVmResult, GuestPhysAddr, MappingFlags, runtime::vcpus, services::RunServices};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HyperCallAbi {
    Generic,
    AArch64,
}

impl HyperCallAbi {
    pub(crate) const fn native() -> Self {
        if cfg!(target_arch = "aarch64") {
            Self::AArch64
        } else {
            Self::Generic
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GuestRequest {
    CpuOn {
        target_vcpu_id: usize,
        entry_point: GuestPhysAddr,
        context_id: usize,
        abi: HyperCallAbi,
    },
    CpuOff {
        abi: HyperCallAbi,
    },
    Reset,
    Hypercall {
        code: HyperCallCode,
        args: [u64; 6],
    },
    NestedFault {
        addr: GuestPhysAddr,
        access_flags: MappingFlags,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HyperCallOutcome {
    Return(usize),
    Deferred(GuestRequest),
    CpuSuspendStandby { return_value: usize },
    CpuOff,
    SystemOff,
    SystemReset,
    Unrecognized,
}

fn is_psci_code(code: HyperCallCode) -> bool {
    matches!(
        code,
        HyperCallCode::PSCIVersion
            | HyperCallCode::PSCIFeatures
            | HyperCallCode::PSCICpuSuspend
            | HyperCallCode::PSCICpuSuspend64
            | HyperCallCode::PSCICpuOff
            | HyperCallCode::PSCICpuOn
            | HyperCallCode::PSCICpuOn64
            | HyperCallCode::PSCIAffinityInfo
            | HyperCallCode::PSCIAffinityInfo64
            | HyperCallCode::PSCIMigrate
            | HyperCallCode::PSCIMigrate64
            | HyperCallCode::PSCIMigrateInfoType
            | HyperCallCode::PSCIMigrateInfoUpCpu
            | HyperCallCode::PSCIMigrateInfoUpCpu64
            | HyperCallCode::PSCISystemOff
            | HyperCallCode::PSCISystemReset
    )
}
pub(crate) const PSCI_RET_SUCCESS: usize = 0;
const PSCI_VERSION_0_2: usize = 0x0000_0002;
const ARM_SMCCC_VERSION_FUNC_ID: u64 = 0x8000_0000;
pub(crate) const PSCI_RET_NOT_SUPPORTED: usize = usize::MAX;
pub(crate) const PSCI_RET_INVALID_PARAMETERS: usize = (-2isize) as usize;
pub(crate) const PSCI_RET_DENIED: usize = (-3isize) as usize;
pub(crate) const PSCI_RET_ALREADY_ON: usize = (-4isize) as usize;
pub(crate) const PSCI_RET_ON_PENDING: usize = (-5isize) as usize;
pub(crate) const PSCI_RET_INTERNAL_FAILURE: usize = (-6isize) as usize;
const PSCI_AFFINITY_LEVEL_ON: usize = 0;
const PSCI_AFFINITY_LEVEL_OFF: usize = 1;
const PSCI_AFFINITY_LEVEL_ON_PENDING: usize = 2;
const PSCI_MIGRATE_TYPE_TOS_NOT_PRESENT: usize = 2;
const PSCI_POWER_STATE_TYPE_SHIFT: u64 = 16;
const PSCI_POWER_STATE_TYPE_MASK: u64 = 0x1;
const PSCI_POWER_STATE_TYPE_STANDBY: u64 = 0;
#[cfg(test)]
const PSCI_POWER_STATE_TYPE_POWERDOWN: u64 = 1;

fn psci_power_state_type(power_state: u64) -> u64 {
    (power_state >> PSCI_POWER_STATE_TYPE_SHIFT) & PSCI_POWER_STATE_TYPE_MASK
}

fn psci_affinity_info_result(state: crate::VmVcpuState) -> usize {
    match state {
        crate::VmVcpuState::Ready | crate::VmVcpuState::Running => PSCI_AFFINITY_LEVEL_ON,
        crate::VmVcpuState::Starting => PSCI_AFFINITY_LEVEL_ON_PENDING,
        _ => PSCI_AFFINITY_LEVEL_OFF,
    }
}

pub(crate) fn psci_find_vcpu_by_mpidr<I>(target_cpu: u64, vcpus: I) -> Option<usize>
where
    I: IntoIterator<Item = (usize, u64)>,
{
    vcpus.into_iter().find_map(|(vcpu_id, mpidr)| {
        psci_mpidr_matches_affinity_level(mpidr, target_cpu, 0).then_some(vcpu_id)
    })
}

fn psci_mpidr_affinity_mask(affinity_level: u64) -> Option<u64> {
    match affinity_level {
        0 => Some(0x0000_00ff_00ff_ffff),
        1 => Some(0x0000_00ff_00ff_ff00),
        2 => Some(0x0000_00ff_00ff_0000),
        3 => Some(0x0000_00ff_0000_0000),
        _ => None,
    }
}

fn psci_mpidr_matches_affinity_level(
    vcpu_mpidr: u64,
    target_affinity: u64,
    affinity_level: u64,
) -> bool {
    psci_mpidr_affinity_mask(affinity_level)
        .is_some_and(|mask| (vcpu_mpidr & mask) == (target_affinity & mask))
}

pub(crate) fn psci_affinity_info_result_for_domain<I>(
    target_affinity: u64,
    affinity_level: u64,
    vcpus: I,
) -> usize
where
    I: IntoIterator<Item = (u64, crate::VmVcpuState)>,
{
    if psci_mpidr_affinity_mask(affinity_level).is_none() {
        return PSCI_RET_INVALID_PARAMETERS;
    }

    let mut has_match = false;
    let mut has_on_pending = false;

    for (mpidr, state) in vcpus {
        if !psci_mpidr_matches_affinity_level(mpidr, target_affinity, affinity_level) {
            continue;
        }

        has_match = true;
        match psci_affinity_info_result(state) {
            PSCI_AFFINITY_LEVEL_ON => return PSCI_AFFINITY_LEVEL_ON,
            PSCI_AFFINITY_LEVEL_ON_PENDING => has_on_pending = true,
            _ => {}
        }
    }

    if !has_match {
        PSCI_RET_INVALID_PARAMETERS
    } else if has_on_pending {
        PSCI_AFFINITY_LEVEL_ON_PENDING
    } else {
        PSCI_AFFINITY_LEVEL_OFF
    }
}

fn decode_hypercall_code(raw_code: u64, abi: HyperCallAbi) -> HyperCallResult<HyperCallCode> {
    let code = HyperCallCode::try_from(raw_code as u32)?;
    if abi != HyperCallAbi::AArch64 && is_psci_code(code) {
        return Err(HyperCallError::Unsupported {
            code,
            detail: "PSCI hypercalls are only available on AArch64".to_string(),
        });
    }
    Ok(code)
}

fn psci_feature_result(function_id: u64) -> usize {
    if function_id == ARM_SMCCC_VERSION_FUNC_ID {
        return PSCI_RET_SUCCESS;
    }

    match decode_hypercall_code(function_id, HyperCallAbi::AArch64) {
        Ok(
            HyperCallCode::PSCIVersion
            | HyperCallCode::PSCIFeatures
            | HyperCallCode::PSCICpuSuspend
            | HyperCallCode::PSCICpuSuspend64
            | HyperCallCode::PSCICpuOff
            | HyperCallCode::PSCICpuOn
            | HyperCallCode::PSCICpuOn64
            | HyperCallCode::PSCIAffinityInfo
            | HyperCallCode::PSCIAffinityInfo64
            | HyperCallCode::PSCIMigrateInfoType
            | HyperCallCode::PSCISystemOff
            | HyperCallCode::PSCISystemReset,
        ) => PSCI_RET_SUCCESS,
        _ => PSCI_RET_NOT_SUPPORTED,
    }
}

pub(crate) fn dispatch_psci(code: HyperCallCode, args: [u64; 6]) -> Option<HyperCallResult> {
    match code {
        HyperCallCode::PSCIVersion => Some(Ok(PSCI_VERSION_0_2)),
        HyperCallCode::PSCIFeatures => Some(Ok(psci_feature_result(args[0]))),
        HyperCallCode::PSCIMigrateInfoType => Some(Ok(PSCI_MIGRATE_TYPE_TOS_NOT_PRESENT)),
        HyperCallCode::PSCIMigrate
        | HyperCallCode::PSCIMigrate64
        | HyperCallCode::PSCIMigrateInfoUpCpu
        | HyperCallCode::PSCIMigrateInfoUpCpu64 => Some(Ok(PSCI_RET_NOT_SUPPORTED)),

        HyperCallCode::PSCICpuOn
        | HyperCallCode::PSCICpuOn64
        | HyperCallCode::PSCIAffinityInfo
        | HyperCallCode::PSCIAffinityInfo64
        | HyperCallCode::PSCICpuSuspend
        | HyperCallCode::PSCICpuSuspend64
        | HyperCallCode::PSCICpuOff
        | HyperCallCode::PSCISystemOff
        | HyperCallCode::PSCISystemReset => None,

        _ => None,
    }
}

pub(crate) fn psci_cpu_on_result(result: Result<(), vcpus::VcpuOnError>) -> usize {
    match result {
        Ok(()) => PSCI_RET_SUCCESS,
        Err(vcpus::VcpuOnError::AlreadyOn) => PSCI_RET_ALREADY_ON,
        Err(vcpus::VcpuOnError::OnPending) => PSCI_RET_ON_PENDING,
        Err(vcpus::VcpuOnError::StartFailed) => PSCI_RET_INTERNAL_FAILURE,
    }
}

pub(crate) fn psci_cpu_off_result(allowed: bool) -> usize {
    if allowed {
        PSCI_RET_SUCCESS
    } else {
        PSCI_RET_DENIED
    }
}

pub(crate) fn handle(
    raw_code: u64,
    args: [u64; 6],
    abi: HyperCallAbi,
    _services: &RunServices,
    _vcpu_id: usize,
) -> AxVmResult<HyperCallOutcome> {
    let code = match decode_hypercall_code(raw_code, abi) {
        Ok(code) => code,
        Err(_) => {
            return Ok(if abi == HyperCallAbi::AArch64 {
                HyperCallOutcome::Return(PSCI_RET_NOT_SUPPORTED)
            } else {
                HyperCallOutcome::Unrecognized
            });
        }
    };
    if let Some(result) = dispatch_psci(code, args) {
        return result.map(HyperCallOutcome::Return).map_err(Into::into);
    }
    Ok(match code {
        HyperCallCode::PSCICpuSuspend | HyperCallCode::PSCICpuSuspend64 => {
            match psci_power_state_type(args[0]) {
                PSCI_POWER_STATE_TYPE_STANDBY => HyperCallOutcome::CpuSuspendStandby {
                    return_value: PSCI_RET_SUCCESS,
                },
                _ => HyperCallOutcome::Return(PSCI_RET_NOT_SUPPORTED),
            }
        }
        HyperCallCode::PSCICpuOff => HyperCallOutcome::CpuOff,
        HyperCallCode::PSCISystemOff => HyperCallOutcome::SystemOff,
        HyperCallCode::PSCISystemReset => HyperCallOutcome::SystemReset,
        _ => HyperCallOutcome::Deferred(GuestRequest::Hypercall { code, args }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hvc_psci_features_cpu_on_matches_execute_contract() {
        assert_eq!(
            dispatch_psci(HyperCallCode::PSCIFeatures, [0x8400_0003, 0, 0, 0, 0, 0]),
            Some(Ok(PSCI_RET_SUCCESS))
        );
        assert_ne!(
            psci_cpu_on_result(Err(vcpus::VcpuOnError::StartFailed)),
            PSCI_RET_NOT_SUPPORTED
        );
        assert_ne!(
            psci_cpu_on_result(Err(vcpus::VcpuOnError::AlreadyOn)),
            PSCI_RET_NOT_SUPPORTED
        );
    }

    #[test]
    fn hvc_cpu_on_start_failure_is_not_reported_as_success() {
        assert_eq!(
            psci_cpu_on_result(Err(vcpus::VcpuOnError::StartFailed)),
            PSCI_RET_INTERNAL_FAILURE
        );
        assert_ne!(
            psci_cpu_on_result(Err(vcpus::VcpuOnError::StartFailed)),
            PSCI_RET_SUCCESS
        );
    }

    #[test]
    fn hvc_cpu_on_busy_states_keep_psci_status() {
        assert_eq!(
            psci_cpu_on_result(Err(vcpus::VcpuOnError::AlreadyOn)),
            PSCI_RET_ALREADY_ON
        );
        assert_eq!(
            psci_cpu_on_result(Err(vcpus::VcpuOnError::OnPending)),
            PSCI_RET_ON_PENDING
        );
    }

    #[test]
    fn hvc_cpu_off_last_vcpu_denial_uses_psci_denied() {
        assert_eq!(super::psci_cpu_off_result(false), super::PSCI_RET_DENIED);
        assert_eq!(super::psci_cpu_off_result(true), super::PSCI_RET_SUCCESS);
    }

    #[test]
    fn hvc_decodes_psci_version_and_dispatches_0_2() {
        let code = decode_hypercall_code(0x8400_0000, HyperCallAbi::AArch64).unwrap();

        assert_eq!(code, HyperCallCode::PSCIVersion);
        assert_eq!(dispatch_psci(code, [0; 6]), Some(Ok(0x0000_0002)));
    }

    #[test]
    fn hvc_decodes_psci_calls_and_returns_standard_errors() {
        for raw_code in [0x8400_000a, 0x8400_0006] {
            let code = decode_hypercall_code(raw_code, HyperCallAbi::AArch64).unwrap();

            assert!(dispatch_psci(code, [0; 6]).is_some());
        }

        let cpu_on = decode_hypercall_code(0xc400_0003, HyperCallAbi::AArch64).unwrap();
        assert_eq!(dispatch_psci(cpu_on, [1, 0x80000, 0, 0, 0, 0]), None);

        let cpu_on32 = decode_hypercall_code(0x8400_0003, HyperCallAbi::AArch64).unwrap();
        assert_eq!(dispatch_psci(cpu_on32, [1, 0x80000, 0, 0, 0, 0]), None);

        let cpu_off = decode_hypercall_code(0x8400_0002, HyperCallAbi::AArch64).unwrap();
        assert_eq!(dispatch_psci(cpu_off, [0; 6]), None);

        let affinity_info = decode_hypercall_code(0xc400_0004, HyperCallAbi::AArch64).unwrap();
        assert_eq!(dispatch_psci(affinity_info, [0, 0, 0, 0, 0, 0]), None);

        let features = decode_hypercall_code(0x8400_000a, HyperCallAbi::AArch64).unwrap();
        assert_eq!(
            dispatch_psci(features, [0x8400_0000, 0, 0, 0, 0, 0]),
            Some(Ok(PSCI_RET_SUCCESS))
        );
        assert_eq!(
            dispatch_psci(features, [0x8400_ffff, 0, 0, 0, 0, 0]),
            Some(Ok(PSCI_RET_NOT_SUPPORTED))
        );
    }

    #[test]
    fn generic_hvc_rejects_psci_function_ids() {
        assert!(decode_hypercall_code(0x8400_0009, HyperCallAbi::Generic).is_err());
        assert!(decode_hypercall_code(0x8400_0003, HyperCallAbi::Generic).is_err());
        assert!(decode_hypercall_code(0xc400_0003, HyperCallAbi::Generic).is_err());
    }

    #[test]
    fn hvc_decodes_cpu_suspend_extended_state_type() {
        assert_eq!(psci_power_state_type(0), PSCI_POWER_STATE_TYPE_STANDBY);
        assert_eq!(
            psci_power_state_type(1 << 16),
            PSCI_POWER_STATE_TYPE_POWERDOWN
        );
        assert_eq!(
            psci_power_state_type(1 << 30),
            PSCI_POWER_STATE_TYPE_STANDBY
        );
    }

    #[test]
    fn hvc_affinity_info_reports_cpu_on_pending_before_first_run() {
        assert_eq!(
            psci_affinity_info_result(crate::VmVcpuState::Starting),
            PSCI_AFFINITY_LEVEL_ON_PENDING
        );
        assert_eq!(
            psci_affinity_info_result(crate::VmVcpuState::Ready),
            PSCI_AFFINITY_LEVEL_ON
        );
        assert_eq!(
            psci_affinity_info_result(crate::VmVcpuState::Running),
            PSCI_AFFINITY_LEVEL_ON
        );
        assert_eq!(
            psci_affinity_info_result(crate::VmVcpuState::Free),
            PSCI_AFFINITY_LEVEL_OFF
        );
    }

    #[test]
    fn hvc_cpu_on_matches_guest_mpidr_not_host_placement() {
        let guest_mpidrs = [(0, 0x0), (1, 0x100)];

        assert_eq!(psci_find_vcpu_by_mpidr(0x100, guest_mpidrs), Some(1));
        assert_eq!(psci_find_vcpu_by_mpidr(5, guest_mpidrs), None);
    }

    #[test]
    fn hvc_affinity_info_matches_requested_mpidr_level() {
        let cpu0 = 0x0000_00ab_0002_0100;
        let cpu1 = 0x0000_00ab_0002_0101;
        let other_cluster = 0x0000_00ab_0002_0200;
        let other_aff2 = 0x0000_00ab_0003_0100;

        assert!(psci_mpidr_matches_affinity_level(cpu0, cpu0, 0));
        assert!(!psci_mpidr_matches_affinity_level(cpu1, cpu0, 0));

        assert!(psci_mpidr_matches_affinity_level(cpu0, cpu0, 1));
        assert!(psci_mpidr_matches_affinity_level(cpu1, cpu0, 1));
        assert!(!psci_mpidr_matches_affinity_level(other_cluster, cpu0, 1));

        assert!(psci_mpidr_matches_affinity_level(other_cluster, cpu0, 2));
        assert!(!psci_mpidr_matches_affinity_level(other_aff2, cpu0, 2));

        assert!(!psci_mpidr_matches_affinity_level(cpu0, cpu0, 4));
    }

    #[test]
    fn hvc_affinity_info_aggregates_nonzero_level_domain() {
        let cpu0 = 0x0000_00ab_0002_0100;
        let cpu1 = 0x0000_00ab_0002_0101;
        let other_cluster = 0x0000_00ab_0002_0200;

        assert_eq!(
            psci_affinity_info_result_for_domain(
                cpu0,
                1,
                [
                    (cpu0, crate::VmVcpuState::Free),
                    (cpu1, crate::VmVcpuState::Starting),
                    (other_cluster, crate::VmVcpuState::Ready),
                ],
            ),
            PSCI_AFFINITY_LEVEL_ON_PENDING
        );

        assert_eq!(
            psci_affinity_info_result_for_domain(
                cpu0,
                1,
                [
                    (cpu0, crate::VmVcpuState::Free),
                    (cpu1, crate::VmVcpuState::Ready),
                ],
            ),
            PSCI_AFFINITY_LEVEL_ON
        );

        assert_eq!(
            psci_affinity_info_result_for_domain(
                cpu0,
                1,
                [
                    (cpu0, crate::VmVcpuState::Free),
                    (cpu1, crate::VmVcpuState::Free),
                ],
            ),
            PSCI_AFFINITY_LEVEL_OFF
        );

        assert_eq!(
            psci_affinity_info_result_for_domain(
                cpu0,
                1,
                [(other_cluster, crate::VmVcpuState::Ready)],
            ),
            PSCI_RET_INVALID_PARAMETERS
        );
    }

    #[test]
    fn hvc_decodes_unsupported_psci_migration_calls() {
        for raw_code in [0x8400_0005, 0x8400_0007, 0xc400_0005, 0xc400_0007] {
            let code = decode_hypercall_code(raw_code, HyperCallAbi::AArch64).unwrap();

            assert_eq!(
                dispatch_psci(code, [0; 6]),
                Some(Ok(PSCI_RET_NOT_SUPPORTED))
            );
            assert_eq!(psci_feature_result(raw_code), PSCI_RET_NOT_SUPPORTED);
        }
    }

    #[test]
    fn hvc_advertises_system_reset_as_required_psci_0_2_call() {
        assert_eq!(psci_feature_result(0x8400_0009), PSCI_RET_SUCCESS);
    }

    #[test]
    fn hvc_psci_features_cover_implemented_0_2_surface() {
        let features = HyperCallCode::PSCIFeatures;
        let supported = [
            0x8400_0000, // PSCI_VERSION
            0x8400_0001, // PSCI_CPU_SUSPEND
            0x8400_0002, // PSCI_CPU_OFF
            0x8400_0003, // PSCI_CPU_ON
            0x8400_0004, // PSCI_AFFINITY_INFO
            0x8400_0006, // PSCI_MIGRATE_INFO_TYPE
            0x8400_0008, // PSCI_SYSTEM_OFF
            0x8400_0009, // PSCI_SYSTEM_RESET
            0x8400_000a, // PSCI_FEATURES
            0xc400_0001, // PSCI_CPU_SUSPEND64
            0xc400_0003, // PSCI_CPU_ON64
            0xc400_0004, // PSCI_AFFINITY_INFO64
        ];

        for raw_code in supported {
            assert_eq!(psci_feature_result(raw_code), PSCI_RET_SUCCESS);
            assert_eq!(
                dispatch_psci(features, [raw_code, 0, 0, 0, 0, 0]),
                Some(Ok(PSCI_RET_SUCCESS))
            );
        }

        let unsupported = [
            0x8400_0005, // PSCI_MIGRATE
            0x8400_0007, // PSCI_MIGRATE_INFO_UP_CPU
            0xc400_0005, // PSCI_MIGRATE64
            0xc400_0007, // PSCI_MIGRATE_INFO_UP_CPU64
        ];

        for raw_code in unsupported {
            assert_eq!(psci_feature_result(raw_code), PSCI_RET_NOT_SUPPORTED);
            assert_eq!(
                dispatch_psci(features, [raw_code, 0, 0, 0, 0, 0]),
                Some(Ok(PSCI_RET_NOT_SUPPORTED))
            );
        }
    }

    #[test]
    fn hvc_psci_features_advertises_smccc_version_query() {
        assert_eq!(
            psci_feature_result(ARM_SMCCC_VERSION_FUNC_ID),
            PSCI_RET_SUCCESS
        );
    }

    #[test]
    fn hvc_rejects_unknown_psci_function_ids() {
        assert!(decode_hypercall_code(0x8400_ffff, HyperCallAbi::AArch64).is_err());
    }
}
