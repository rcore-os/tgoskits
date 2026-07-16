//! VM-local Power State Coordination Interface dispatch.

const FUNCTION_32_BASE: u64 = 0x8400_0000;
const FUNCTION_64_BASE: u64 = 0xc400_0000;
const FUNCTION_MAX_OFFSET: u64 = 0x1f;

const VERSION: u64 = 0;
const CPU_SUSPEND: u64 = 1;
const CPU_OFF: u64 = 2;
const CPU_ON: u64 = 3;
const AFFINITY_INFO: u64 = 4;
const MIGRATE: u64 = 5;
const MIGRATE_INFO_TYPE: u64 = 6;
const MIGRATE_INFO_UP_CPU: u64 = 7;
const SYSTEM_OFF: u64 = 8;
const SYSTEM_RESET: u64 = 9;
const FEATURES: u64 = 10;

const PSCI_VERSION_1_0: u64 = 1 << 16;
const TOS_NOT_PRESENT_MP: u64 = 2;
const SUCCESS: u64 = 0;
const NOT_SUPPORTED: u64 = u64::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PsciCall {
    Complete(u64),
    CpuOff {
        state: u64,
    },
    CpuOn {
        target_cpu: u64,
        entry_point: u64,
        context: u64,
    },
    SystemOff,
}

pub(crate) fn decode(function: u64, args: [u64; 3]) -> Option<PsciCall> {
    let descriptor = FunctionDescriptor::decode(function)?;
    Some(match descriptor.offset {
        VERSION if !descriptor.is_64 => PsciCall::Complete(PSCI_VERSION_1_0),
        CPU_OFF if !descriptor.is_64 => PsciCall::CpuOff { state: args[0] },
        CPU_ON => PsciCall::CpuOn {
            target_cpu: args[0],
            entry_point: args[1],
            context: args[2],
        },
        MIGRATE_INFO_TYPE if !descriptor.is_64 => PsciCall::Complete(TOS_NOT_PRESENT_MP),
        SYSTEM_OFF | SYSTEM_RESET if !descriptor.is_64 => PsciCall::SystemOff,
        FEATURES if !descriptor.is_64 => PsciCall::Complete(feature_result(args[0])),
        CPU_SUSPEND | AFFINITY_INFO | MIGRATE | MIGRATE_INFO_UP_CPU => {
            PsciCall::Complete(NOT_SUPPORTED)
        }
        _ => PsciCall::Complete(NOT_SUPPORTED),
    })
}

fn feature_result(function: u64) -> u64 {
    let Some(descriptor) = FunctionDescriptor::decode(function) else {
        return NOT_SUPPORTED;
    };
    match descriptor.offset {
        VERSION | CPU_OFF | MIGRATE_INFO_TYPE | SYSTEM_OFF | SYSTEM_RESET | FEATURES
            if !descriptor.is_64 =>
        {
            SUCCESS
        }
        CPU_ON => SUCCESS,
        _ => NOT_SUPPORTED,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FunctionDescriptor {
    offset: u64,
    is_64: bool,
}

impl FunctionDescriptor {
    fn decode(function: u64) -> Option<Self> {
        let (offset, is_64) = if function <= FUNCTION_32_BASE + FUNCTION_MAX_OFFSET {
            (function.checked_sub(FUNCTION_32_BASE)?, false)
        } else if function <= FUNCTION_64_BASE + FUNCTION_MAX_OFFSET {
            (function.checked_sub(FUNCTION_64_BASE)?, true)
        } else {
            return None;
        };
        Some(Self { offset, is_64 })
    }
}
