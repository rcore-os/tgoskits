//! Typed conversion between Linux scheduling attributes and scheduler policies.

use ax_errno::{AxError, AxResult};
use ax_std::os::arceos::task::{
    DeadlineFlags, DeadlinePolicy, FairMode, Nice, RtPriority, SchedulePolicy,
};
use bytemuck::{Pod, Zeroable};
use linux_raw_sys::general::{
    SCHED_BATCH, SCHED_DEADLINE, SCHED_FIFO, SCHED_FLAG_DL_OVERRUN, SCHED_FLAG_KEEP_PARAMS,
    SCHED_FLAG_KEEP_POLICY, SCHED_FLAG_RECLAIM, SCHED_FLAG_RESET_ON_FORK,
    SCHED_FLAG_UTIL_CLAMP_MAX, SCHED_FLAG_UTIL_CLAMP_MIN, SCHED_IDLE, SCHED_NORMAL,
    SCHED_RESET_ON_FORK, SCHED_RR,
};

/// Linux's extensible scheduling attribute ABI through utilization clamp v1.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
pub(crate) struct SchedAttr {
    pub(crate) size: u32,
    pub(crate) sched_policy: u32,
    pub(crate) sched_flags: u64,
    pub(crate) sched_nice: i32,
    pub(crate) sched_priority: u32,
    pub(crate) sched_runtime: u64,
    pub(crate) sched_deadline: u64,
    pub(crate) sched_period: u64,
    pub(crate) sched_util_min: u32,
    pub(crate) sched_util_max: u32,
}

/// Validated scheduler update resolved against the target's current policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScheduleUpdate {
    /// Policy and parameters that are committed to the target thread.
    pub(crate) policy: SchedulePolicy,
    /// Requested policy combined with the parameters Linux validates.
    pub(crate) permission_policy: SchedulePolicy,
    pub(crate) reset_on_fork: bool,
}

/// Linux credentials and resource limits relevant to a policy update.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SchedulerPermission {
    pub(crate) owns_target: bool,
    pub(crate) has_cap_sys_nice: bool,
    pub(crate) rlimit_rtprio: u64,
    pub(crate) rlimit_nice: u64,
    pub(crate) stored_nice: Nice,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinuxScheduleClass {
    Fair(FairMode),
    Fifo,
    RoundRobin,
    Deadline,
}

/// Converts a Linux `sched_attr` into a fully validated core policy.
pub(crate) fn parse_sched_attr(
    attr: SchedAttr,
    current_policy: SchedulePolicy,
) -> AxResult<ScheduleUpdate> {
    validate_sched_attr_size(attr.size)?;
    validate_sched_attr_flags(attr.sched_flags)?;

    let keep_policy = attr.sched_flags & SCHED_FLAG_KEEP_POLICY as u64 != 0;
    let keep_params = attr.sched_flags & SCHED_FLAG_KEEP_PARAMS as u64 != 0;
    let effective_class = if keep_policy {
        schedule_class(current_policy)
    } else {
        linux_schedule_class(attr.sched_policy)?
    };
    let reset_on_fork = attr.sched_flags & SCHED_FLAG_RESET_ON_FORK as u64 != 0;
    let (policy, permission_policy) = if keep_params {
        (
            current_policy,
            policy_from_kept_params(effective_class, current_policy, attr)?,
        )
    } else {
        let policy = policy_from_sched_attr(effective_class, attr)?;
        (policy, policy)
    };

    Ok(ScheduleUpdate {
        policy,
        permission_policy,
        reset_on_fork,
    })
}

fn policy_from_kept_params(
    requested_class: LinuxScheduleClass,
    current_policy: SchedulePolicy,
    mut attr: SchedAttr,
) -> AxResult<SchedulePolicy> {
    let deadline_flag_mask = (SCHED_FLAG_RECLAIM | SCHED_FLAG_DL_OVERRUN) as u64;
    match current_policy {
        SchedulePolicy::Fair { nice, .. } => {
            attr.sched_nice = i32::from(nice.get());
            // Linux get_params() also replaces sched_runtime with the current
            // fair slice. ax-task keeps that request in its entity rather than
            // SchedulePolicy; zero still makes a Fair-to-Deadline validation
            // fail instead of trusting user-supplied parameters that will not
            // be committed.
            attr.sched_runtime = 0;
        }
        SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => {
            attr.sched_priority = u32::from(priority.get());
        }
        SchedulePolicy::Deadline(deadline) => {
            attr.sched_priority = 0;
            attr.sched_runtime = deadline.runtime_ns();
            attr.sched_deadline = deadline.deadline_ns();
            attr.sched_period = deadline.period_ns();
            attr.sched_flags =
                (attr.sched_flags & !deadline_flag_mask) | linux_deadline_flags(deadline.flags());
        }
    }

    match requested_class {
        LinuxScheduleClass::Fair(mode) => {
            if attr.sched_priority != 0 {
                return Err(AxError::InvalidInput);
            }
            let nice = Nice::new(attr.sched_nice.clamp(-20, 19) as i8)
                .map_err(|_| AxError::InvalidInput)?;
            Ok(SchedulePolicy::fair(nice, mode))
        }
        LinuxScheduleClass::Fifo => Ok(SchedulePolicy::fifo(parse_rt_priority(attr)?)),
        LinuxScheduleClass::RoundRobin => Ok(SchedulePolicy::round_robin(parse_rt_priority(attr)?)),
        LinuxScheduleClass::Deadline => {
            if attr.sched_priority != 0 {
                return Err(AxError::InvalidInput);
            }
            let deadline = DeadlinePolicy::new(
                attr.sched_runtime,
                attr.sched_deadline,
                attr.sched_period,
                deadline_flags(attr.sched_flags),
            )
            .map_err(|_| AxError::InvalidInput)?;
            Ok(SchedulePolicy::deadline(deadline))
        }
    }
}

/// Serializes a core policy into Linux's current `sched_attr` layout.
pub(crate) fn sched_attr_from_policy(policy: SchedulePolicy, reset_on_fork: bool) -> SchedAttr {
    let mut attr = match policy {
        SchedulePolicy::Fair { nice, mode } => {
            SchedAttr::fair(linux_fair_policy(mode), i32::from(nice.get()))
        }
        SchedulePolicy::Fifo { priority } => {
            SchedAttr::realtime(SCHED_FIFO, u32::from(priority.get()))
        }
        SchedulePolicy::RoundRobin { priority, .. } => {
            SchedAttr::realtime(SCHED_RR, u32::from(priority.get()))
        }
        SchedulePolicy::Deadline(deadline) => SchedAttr {
            size: core::mem::size_of::<SchedAttr>() as u32,
            sched_policy: SCHED_DEADLINE,
            sched_flags: linux_deadline_flags(deadline.flags()),
            sched_runtime: deadline.runtime_ns(),
            sched_deadline: deadline.deadline_ns(),
            sched_period: deadline.period_ns(),
            ..SchedAttr::zeroed()
        },
    };
    if reset_on_fork {
        attr.sched_flags |= SCHED_FLAG_RESET_ON_FORK as u64;
    }
    attr
}

/// Returns the Linux policy number represented by a core policy.
pub(crate) const fn linux_policy_number(policy: SchedulePolicy) -> u32 {
    match policy {
        SchedulePolicy::Fair { mode, .. } => linux_fair_policy(mode),
        SchedulePolicy::Fifo { .. } => SCHED_FIFO,
        SchedulePolicy::RoundRobin { .. } => SCHED_RR,
        SchedulePolicy::Deadline(_) => SCHED_DEADLINE,
    }
}

/// Returns the `sched_param` priority represented by a core policy.
pub(crate) const fn linux_sched_priority(policy: SchedulePolicy) -> i32 {
    match policy {
        SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => {
            priority.get() as i32
        }
        SchedulePolicy::Fair { .. } | SchedulePolicy::Deadline(_) => 0,
    }
}

/// Resolves the child's policy and flag according to Linux reset-on-fork rules.
pub(crate) fn fork_schedule_policy(
    parent: SchedulePolicy,
    reset_on_fork: bool,
) -> AxResult<(SchedulePolicy, bool)> {
    if !reset_on_fork {
        if matches!(parent, SchedulePolicy::Deadline(_)) {
            return Err(AxError::WouldBlock);
        }
        return Ok((parent, false));
    }

    let child = match parent {
        SchedulePolicy::Fifo { .. }
        | SchedulePolicy::RoundRobin { .. }
        | SchedulePolicy::Deadline(_) => SchedulePolicy::fair(Nice::ZERO, FairMode::Normal),
        SchedulePolicy::Fair { nice, mode } if nice.get() < 0 => {
            SchedulePolicy::fair(Nice::ZERO, mode)
        }
        SchedulePolicy::Fair { .. } => parent,
    };
    Ok((child, false))
}

/// Converts the legacy `sched_setscheduler` arguments into a core policy.
pub(crate) fn parse_setscheduler(
    raw_policy: i32,
    priority: i32,
    current_policy: SchedulePolicy,
    stored_nice: Nice,
) -> AxResult<ScheduleUpdate> {
    let raw_policy = u32::try_from(raw_policy).map_err(|_| AxError::InvalidInput)?;
    let reset_on_fork = raw_policy & SCHED_RESET_ON_FORK != 0;
    let policy = raw_policy & !SCHED_RESET_ON_FORK;
    if policy == SCHED_DEADLINE {
        return Err(AxError::InvalidInput);
    }

    let attr = match linux_schedule_class(policy)? {
        LinuxScheduleClass::Fair(_) => {
            if priority != 0 {
                return Err(AxError::InvalidInput);
            }
            SchedAttr::fair(policy, i32::from(stored_nice.get()))
        }
        LinuxScheduleClass::Fifo | LinuxScheduleClass::RoundRobin => {
            let priority = u32::try_from(priority).map_err(|_| AxError::InvalidInput)?;
            SchedAttr::realtime(policy, priority)
        }
        LinuxScheduleClass::Deadline => return Err(AxError::InvalidInput),
    };
    let mut attr = attr;
    if reset_on_fork {
        attr.sched_flags |= SCHED_FLAG_RESET_ON_FORK as u64;
    }
    parse_sched_attr(attr, current_policy)
}

/// Returns Linux's minimum priority for a supported policy number.
pub(crate) fn scheduler_priority_min(policy: u32) -> AxResult<i32> {
    match linux_schedule_class(policy)? {
        LinuxScheduleClass::Fifo | LinuxScheduleClass::RoundRobin => Ok(1),
        LinuxScheduleClass::Fair(_) | LinuxScheduleClass::Deadline => Ok(0),
    }
}

/// Returns Linux's maximum priority for a supported policy number.
pub(crate) fn scheduler_priority_max(policy: u32) -> AxResult<i32> {
    match linux_schedule_class(policy)? {
        LinuxScheduleClass::Fifo | LinuxScheduleClass::RoundRobin => Ok(99),
        LinuxScheduleClass::Fair(_) | LinuxScheduleClass::Deadline => Ok(0),
    }
}

/// Enforces ownership, capability, `RLIMIT_RTPRIO`, and `RLIMIT_NICE`.
pub(crate) fn check_policy_permission(
    permission: SchedulerPermission,
    current: SchedulePolicy,
    requested: SchedulePolicy,
) -> AxResult<()> {
    if !permission.owns_target && !permission.has_cap_sys_nice {
        return Err(AxError::OperationNotPermitted);
    }
    if permission.has_cap_sys_nice {
        return Ok(());
    }

    match requested {
        SchedulePolicy::Deadline(_) => Err(AxError::OperationNotPermitted),
        SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => {
            let same_rt_class = matches!(
                (current, requested),
                (SchedulePolicy::Fifo { .. }, SchedulePolicy::Fifo { .. })
                    | (
                        SchedulePolicy::RoundRobin { .. },
                        SchedulePolicy::RoundRobin { .. }
                    )
            );
            if !same_rt_class && permission.rlimit_rtprio == 0 {
                return Err(AxError::OperationNotPermitted);
            }
            let current_priority = match current {
                SchedulePolicy::Fifo { priority } | SchedulePolicy::RoundRobin { priority, .. } => {
                    priority.get()
                }
                _ => 0,
            };
            let ceiling = permission
                .rlimit_rtprio
                .min(99)
                .max(u64::from(current_priority));
            if u64::from(priority.get()) <= ceiling {
                Ok(())
            } else {
                Err(AxError::OperationNotPermitted)
            }
        }
        SchedulePolicy::Fair {
            nice,
            mode: requested_mode,
        } => {
            let current_nice = match current {
                SchedulePolicy::Fair { nice, .. } => nice,
                _ => permission.stored_nice,
            }
            .get();
            let lowest_allowed = 20_i64 - permission.rlimit_nice.min(40) as i64;
            if matches!(
                current,
                SchedulePolicy::Fair {
                    mode: FairMode::Idle,
                    ..
                }
            ) && requested_mode != FairMode::Idle
                && i64::from(current_nice) < lowest_allowed
            {
                return Err(AxError::OperationNotPermitted);
            }
            if nice.get() >= current_nice || i64::from(nice.get()) >= lowest_allowed {
                Ok(())
            } else {
                Err(AxError::OperationNotPermitted)
            }
        }
    }
}

/// Prevents an unprivileged caller from clearing reset-on-fork.
pub(crate) fn check_reset_on_fork_permission(
    has_cap_sys_nice: bool,
    current: bool,
    requested: bool,
) -> AxResult<()> {
    if current && !requested && !has_cap_sys_nice {
        Err(AxError::OperationNotPermitted)
    } else {
        Ok(())
    }
}

fn validate_sched_attr_size(size: u32) -> AxResult<()> {
    const SCHED_ATTR_V0_SIZE: u32 = 48;
    const SCHED_ATTR_V1_SIZE: u32 = core::mem::size_of::<SchedAttr>() as u32;

    if size == 0 || (SCHED_ATTR_V0_SIZE..=SCHED_ATTR_V1_SIZE).contains(&size) {
        Ok(())
    } else {
        Err(AxError::ArgumentListTooLong)
    }
}

fn validate_sched_attr_flags(flags: u64) -> AxResult<()> {
    const SUPPORTED: u64 = (SCHED_FLAG_RESET_ON_FORK
        | SCHED_FLAG_RECLAIM
        | SCHED_FLAG_DL_OVERRUN
        | SCHED_FLAG_KEEP_POLICY
        | SCHED_FLAG_KEEP_PARAMS) as u64;
    const UTIL_CLAMP: u64 = (SCHED_FLAG_UTIL_CLAMP_MIN | SCHED_FLAG_UTIL_CLAMP_MAX) as u64;

    if flags & !(SUPPORTED | UTIL_CLAMP) != 0 {
        Err(AxError::InvalidInput)
    } else if flags & UTIL_CLAMP != 0 {
        Err(AxError::OperationNotSupported)
    } else {
        Ok(())
    }
}

fn policy_from_sched_attr(class: LinuxScheduleClass, attr: SchedAttr) -> AxResult<SchedulePolicy> {
    match class {
        LinuxScheduleClass::Fair(mode) => {
            ensure_zero_realtime_and_deadline_fields(attr)?;
            // Linux clamps this field in sched_copy_attr() before validating
            // the requested policy.
            let nice = attr.sched_nice.clamp(-20, 19) as i8;
            Ok(SchedulePolicy::fair(
                Nice::new(nice).map_err(|_| AxError::InvalidInput)?,
                mode,
            ))
        }
        LinuxScheduleClass::Fifo => {
            ensure_zero_deadline_fields(attr)?;
            Ok(SchedulePolicy::fifo(parse_rt_priority(attr)?))
        }
        LinuxScheduleClass::RoundRobin => {
            ensure_zero_deadline_fields(attr)?;
            Ok(SchedulePolicy::round_robin(parse_rt_priority(attr)?))
        }
        LinuxScheduleClass::Deadline => {
            if attr.sched_priority != 0 {
                return Err(AxError::InvalidInput);
            }
            let flags = deadline_flags(attr.sched_flags);
            let deadline = DeadlinePolicy::new(
                attr.sched_runtime,
                attr.sched_deadline,
                attr.sched_period,
                flags,
            )
            .map_err(|_| AxError::InvalidInput)?;
            Ok(SchedulePolicy::deadline(deadline))
        }
    }
}

fn ensure_zero_realtime_and_deadline_fields(attr: SchedAttr) -> AxResult<()> {
    if attr.sched_priority == 0 {
        ensure_zero_deadline_fields(attr)
    } else {
        Err(AxError::InvalidInput)
    }
}

fn ensure_zero_deadline_fields(attr: SchedAttr) -> AxResult<()> {
    let deadline_flags = (SCHED_FLAG_RECLAIM | SCHED_FLAG_DL_OVERRUN) as u64;
    if attr.sched_runtime != 0
        || attr.sched_deadline != 0
        || attr.sched_period != 0
        || attr.sched_flags & deadline_flags != 0
    {
        Err(AxError::InvalidInput)
    } else {
        Ok(())
    }
}

fn parse_rt_priority(attr: SchedAttr) -> AxResult<RtPriority> {
    let priority = u8::try_from(attr.sched_priority).map_err(|_| AxError::InvalidInput)?;
    RtPriority::new(priority).map_err(|_| AxError::InvalidInput)
}

fn deadline_flags(raw: u64) -> DeadlineFlags {
    let mut flags = DeadlineFlags::NONE;
    if raw & SCHED_FLAG_RECLAIM as u64 != 0 {
        flags = flags | DeadlineFlags::RECLAIM;
    }
    if raw & SCHED_FLAG_DL_OVERRUN as u64 != 0 {
        flags = flags | DeadlineFlags::DL_OVERRUN;
    }
    flags
}

fn linux_deadline_flags(flags: DeadlineFlags) -> u64 {
    let mut raw = 0;
    if flags.contains(DeadlineFlags::RECLAIM) {
        raw |= SCHED_FLAG_RECLAIM as u64;
    }
    if flags.contains(DeadlineFlags::DL_OVERRUN) {
        raw |= SCHED_FLAG_DL_OVERRUN as u64;
    }
    raw
}

fn linux_schedule_class(policy: u32) -> AxResult<LinuxScheduleClass> {
    match policy {
        SCHED_NORMAL => Ok(LinuxScheduleClass::Fair(FairMode::Normal)),
        SCHED_BATCH => Ok(LinuxScheduleClass::Fair(FairMode::Batch)),
        SCHED_IDLE => Ok(LinuxScheduleClass::Fair(FairMode::Idle)),
        SCHED_FIFO => Ok(LinuxScheduleClass::Fifo),
        SCHED_RR => Ok(LinuxScheduleClass::RoundRobin),
        SCHED_DEADLINE => Ok(LinuxScheduleClass::Deadline),
        _ => Err(AxError::InvalidInput),
    }
}

fn schedule_class(policy: SchedulePolicy) -> LinuxScheduleClass {
    match policy {
        SchedulePolicy::Fair { mode, .. } => LinuxScheduleClass::Fair(mode),
        SchedulePolicy::Fifo { .. } => LinuxScheduleClass::Fifo,
        SchedulePolicy::RoundRobin { .. } => LinuxScheduleClass::RoundRobin,
        SchedulePolicy::Deadline(_) => LinuxScheduleClass::Deadline,
    }
}

const fn linux_fair_policy(mode: FairMode) -> u32 {
    match mode {
        FairMode::Normal => SCHED_NORMAL,
        FairMode::Batch => SCHED_BATCH,
        FairMode::Idle => SCHED_IDLE,
    }
}

impl SchedAttr {
    fn fair(policy: u32, nice: i32) -> Self {
        Self {
            size: core::mem::size_of::<Self>() as u32,
            sched_policy: policy,
            sched_nice: nice,
            ..Self::zeroed()
        }
    }

    fn realtime(policy: u32, priority: u32) -> Self {
        Self {
            size: core::mem::size_of::<Self>() as u32,
            sched_policy: policy,
            sched_priority: priority,
            ..Self::zeroed()
        }
    }

    #[cfg(test)]
    fn deadline(runtime: u64, deadline: u64, period: u64) -> Self {
        Self {
            size: core::mem::size_of::<Self>() as u32,
            sched_policy: SCHED_DEADLINE,
            sched_runtime: runtime,
            sched_deadline: deadline,
            sched_period: period,
            ..Self::zeroed()
        }
    }
}

#[cfg(test)]
mod tests {
    use ax_std::os::arceos::task::{DeadlineFlags, FairMode, Nice, RtPriority, SchedulePolicy};
    use linux_raw_sys::general::{
        SCHED_BATCH, SCHED_DEADLINE, SCHED_FIFO, SCHED_FLAG_DL_OVERRUN, SCHED_FLAG_KEEP_PARAMS,
        SCHED_FLAG_KEEP_POLICY, SCHED_FLAG_RECLAIM, SCHED_FLAG_RESET_ON_FORK,
        SCHED_FLAG_UTIL_CLAMP_MIN, SCHED_IDLE, SCHED_NORMAL, SCHED_RR,
    };

    use super::*;

    #[test]
    fn parses_every_supported_sched_attr_policy() {
        let normal =
            parse_sched_attr(SchedAttr::fair(SCHED_NORMAL, -5), SchedulePolicy::default()).unwrap();
        assert_eq!(
            normal.policy,
            SchedulePolicy::fair(Nice::new(-5).unwrap(), FairMode::Normal)
        );

        let batch =
            parse_sched_attr(SchedAttr::fair(SCHED_BATCH, 4), SchedulePolicy::default()).unwrap();
        assert!(matches!(
            batch.policy,
            SchedulePolicy::Fair {
                mode: FairMode::Batch,
                ..
            }
        ));

        let idle =
            parse_sched_attr(SchedAttr::fair(SCHED_IDLE, 19), SchedulePolicy::default()).unwrap();
        assert!(matches!(
            idle.policy,
            SchedulePolicy::Fair {
                mode: FairMode::Idle,
                ..
            }
        ));

        let fifo = parse_sched_attr(
            SchedAttr::realtime(SCHED_FIFO, 99),
            SchedulePolicy::default(),
        )
        .unwrap();
        assert!(matches!(fifo.policy, SchedulePolicy::Fifo { .. }));

        let rr =
            parse_sched_attr(SchedAttr::realtime(SCHED_RR, 1), SchedulePolicy::default()).unwrap();
        assert!(matches!(rr.policy, SchedulePolicy::RoundRobin { .. }));

        let deadline =
            parse_sched_attr(SchedAttr::deadline(10, 20, 30), SchedulePolicy::default()).unwrap();
        assert!(matches!(deadline.policy, SchedulePolicy::Deadline(_)));
    }

    #[test]
    fn sched_attr_matches_linux_v1_layout() {
        assert_eq!(core::mem::size_of::<SchedAttr>(), 56);
        assert_eq!(core::mem::offset_of!(SchedAttr, sched_flags), 8);
        assert_eq!(core::mem::offset_of!(SchedAttr, sched_runtime), 24);
        assert_eq!(core::mem::offset_of!(SchedAttr, sched_util_min), 48);
    }

    #[test]
    fn rejects_unknown_and_util_clamp_flags_explicitly() {
        let mut attr = SchedAttr::fair(SCHED_NORMAL, 0);
        attr.sched_flags = 1 << 63;
        assert_eq!(
            parse_sched_attr(attr, SchedulePolicy::default()),
            Err(AxError::InvalidInput)
        );

        attr.sched_flags = SCHED_FLAG_UTIL_CLAMP_MIN as u64;
        assert_eq!(
            parse_sched_attr(attr, SchedulePolicy::default()),
            Err(AxError::OperationNotSupported)
        );

        attr.sched_flags = SCHED_FLAG_UTIL_CLAMP_MIN as u64 | 1 << 63;
        assert_eq!(
            parse_sched_attr(attr, SchedulePolicy::default()),
            Err(AxError::InvalidInput)
        );
    }

    #[test]
    fn maps_deadline_flags_without_accepting_them_for_other_classes() {
        let mut attr = SchedAttr::deadline(10, 20, 30);
        attr.sched_flags =
            (SCHED_FLAG_RECLAIM | SCHED_FLAG_DL_OVERRUN | SCHED_FLAG_RESET_ON_FORK) as u64;
        let update = parse_sched_attr(attr, SchedulePolicy::default()).unwrap();
        let SchedulePolicy::Deadline(deadline) = update.policy else {
            panic!("deadline attr must create a deadline policy");
        };
        assert!(deadline.flags().contains(DeadlineFlags::RECLAIM));
        assert!(deadline.flags().contains(DeadlineFlags::DL_OVERRUN));
        assert!(!deadline.flags().contains(DeadlineFlags::RESET_ON_FORK));
        assert!(update.reset_on_fork);

        let mut normal = SchedAttr::fair(SCHED_NORMAL, 0);
        normal.sched_flags = SCHED_FLAG_RECLAIM as u64;
        assert_eq!(
            parse_sched_attr(normal, SchedulePolicy::default()),
            Err(AxError::InvalidInput)
        );
    }

    #[test]
    fn keep_policy_and_params_validate_the_requested_class() {
        let current = SchedulePolicy::fair(Nice::new(7).unwrap(), FairMode::Batch);
        let mut keep_policy = SchedAttr::fair(SCHED_FIFO, 4);
        keep_policy.sched_flags = SCHED_FLAG_KEEP_POLICY as u64;
        let update = parse_sched_attr(keep_policy, current).unwrap();
        assert_eq!(
            update.policy,
            SchedulePolicy::fair(Nice::new(4).unwrap(), FairMode::Batch)
        );

        let mut keep_params = SchedAttr::fair(SCHED_NORMAL, -20);
        keep_params.sched_flags = (SCHED_FLAG_KEEP_POLICY | SCHED_FLAG_KEEP_PARAMS) as u64;
        let update = parse_sched_attr(keep_params, current).unwrap();
        assert_eq!(update.policy, current);

        keep_params.sched_policy = u32::MAX;
        let update = parse_sched_attr(keep_params, current).unwrap();
        assert_eq!(update.policy, current);

        let mut keep_params_only = SchedAttr::realtime(SCHED_FIFO, 99);
        keep_params_only.sched_flags = SCHED_FLAG_KEEP_PARAMS as u64;
        let update = parse_sched_attr(keep_params_only, current).unwrap();
        assert_eq!(update.policy, current);
        assert!(matches!(
            update.permission_policy,
            SchedulePolicy::Fifo { priority } if priority.get() == 99
        ));

        let current = SchedulePolicy::fifo(RtPriority::new(7).unwrap());
        let mut compatible = SchedAttr::realtime(SCHED_RR, 99);
        compatible.sched_flags = SCHED_FLAG_KEEP_PARAMS as u64;
        let update = parse_sched_attr(compatible, current).unwrap();
        assert_eq!(update.policy, current);
        assert!(matches!(
            update.permission_policy,
            SchedulePolicy::RoundRobin { priority, .. } if priority.get() == 7
        ));
        let unprivileged = SchedulerPermission {
            owns_target: true,
            has_cap_sys_nice: false,
            rlimit_rtprio: 0,
            rlimit_nice: 0,
            stored_nice: Nice::ZERO,
        };
        assert_eq!(
            check_policy_permission(unprivileged, current, update.permission_policy,),
            Err(AxError::OperationNotPermitted),
        );
        assert_eq!(
            check_policy_permission(
                SchedulerPermission {
                    has_cap_sys_nice: true,
                    ..unprivileged
                },
                current,
                update.permission_policy,
            ),
            Ok(()),
        );

        let current =
            SchedulePolicy::deadline(DeadlinePolicy::new(10, 20, 30, DeadlineFlags::NONE).unwrap());
        let mut keep_deadline_params = SchedAttr::fair(SCHED_NORMAL, 0);
        keep_deadline_params.sched_flags = SCHED_FLAG_KEEP_PARAMS as u64;
        let update = parse_sched_attr(keep_deadline_params, current).unwrap();
        assert_eq!(update.policy, current);
        assert!(matches!(
            update.permission_policy,
            SchedulePolicy::Fair {
                mode: FairMode::Normal,
                ..
            }
        ));
    }

    #[test]
    fn validates_deadline_and_realtime_parameters() {
        assert_eq!(
            parse_sched_attr(SchedAttr::deadline(20, 10, 30), SchedulePolicy::default()),
            Err(AxError::InvalidInput)
        );
        assert_eq!(
            parse_sched_attr(
                SchedAttr::realtime(SCHED_FIFO, 0),
                SchedulePolicy::default()
            ),
            Err(AxError::InvalidInput)
        );
        assert_eq!(
            parse_sched_attr(
                SchedAttr::realtime(SCHED_RR, 100),
                SchedulePolicy::default()
            ),
            Err(AxError::InvalidInput)
        );
        assert_eq!(
            parse_sched_attr(
                SchedAttr::realtime(SCHED_DEADLINE, 1),
                SchedulePolicy::default()
            ),
            Err(AxError::InvalidInput)
        );
    }

    #[test]
    fn legacy_setscheduler_rejects_deadline_and_preserves_reset_on_fork() {
        let current = SchedulePolicy::default();
        let update = parse_setscheduler(
            (SCHED_FIFO | SCHED_RESET_ON_FORK) as i32,
            10,
            current,
            Nice::ZERO,
        )
        .unwrap();
        assert!(matches!(update.policy, SchedulePolicy::Fifo { .. }));
        assert!(update.reset_on_fork);
        assert_eq!(
            parse_setscheduler(SCHED_DEADLINE as i32, 0, current, Nice::ZERO),
            Err(AxError::InvalidInput)
        );
    }

    #[test]
    fn legacy_setscheduler_restores_stored_nice_when_leaving_rt() {
        let current = SchedulePolicy::fifo(RtPriority::new(42).unwrap());
        let update =
            parse_setscheduler(SCHED_BATCH as i32, 0, current, Nice::new(-7).unwrap()).unwrap();

        assert_eq!(
            update.policy,
            SchedulePolicy::fair(Nice::new(-7).unwrap(), FairMode::Batch)
        );
    }

    #[test]
    fn fork_inherits_or_resets_policy_before_child_publication() {
        let fifo = SchedulePolicy::fifo(RtPriority::new(42).unwrap());
        assert_eq!(fork_schedule_policy(fifo, false), Ok((fifo, false)));
        assert_eq!(
            fork_schedule_policy(fifo, true),
            Ok((SchedulePolicy::fair(Nice::ZERO, FairMode::Normal), false,))
        );

        let negative_batch = SchedulePolicy::fair(Nice::new(-7).unwrap(), FairMode::Batch);
        assert_eq!(
            fork_schedule_policy(negative_batch, true),
            Ok((SchedulePolicy::fair(Nice::ZERO, FairMode::Batch), false,))
        );

        let deadline =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap());
        assert_eq!(
            fork_schedule_policy(deadline, false),
            Err(AxError::WouldBlock)
        );
        assert_eq!(
            fork_schedule_policy(deadline, true),
            Ok((SchedulePolicy::fair(Nice::ZERO, FairMode::Normal), false,))
        );
    }

    #[test]
    fn rejects_sched_attr_sizes_outside_supported_versions() {
        let mut attr = SchedAttr::fair(SCHED_NORMAL, 0);
        attr.size = 47;
        assert_eq!(
            parse_sched_attr(attr, SchedulePolicy::default()),
            Err(AxError::ArgumentListTooLong)
        );
        attr.size = core::mem::size_of::<SchedAttr>() as u32 + 1;
        assert_eq!(
            parse_sched_attr(attr, SchedulePolicy::default()),
            Err(AxError::ArgumentListTooLong)
        );
    }

    #[test]
    fn enforces_unprivileged_rt_and_nice_limits() {
        let fair = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        let fifo_20 = SchedulePolicy::fifo(RtPriority::new(20).unwrap());
        let permission = SchedulerPermission {
            owns_target: true,
            has_cap_sys_nice: false,
            rlimit_rtprio: 10,
            rlimit_nice: 25,
            stored_nice: Nice::ZERO,
        };
        assert_eq!(
            check_policy_permission(permission, fair, fifo_20),
            Err(AxError::OperationNotPermitted)
        );

        let allowed_nice = SchedulePolicy::fair(Nice::new(-5).unwrap(), FairMode::Normal);
        assert_eq!(
            check_policy_permission(permission, fair, allowed_nice),
            Ok(())
        );
        let denied_nice = SchedulePolicy::fair(Nice::new(-6).unwrap(), FairMode::Normal);
        assert_eq!(
            check_policy_permission(permission, fair, denied_nice),
            Err(AxError::OperationNotPermitted)
        );

        let fifo_10 = SchedulePolicy::fifo(RtPriority::new(10).unwrap());
        let rr_10 = SchedulePolicy::round_robin(RtPriority::new(10).unwrap());
        let no_rt_limit = SchedulerPermission {
            rlimit_rtprio: 0,
            ..permission
        };
        assert_eq!(
            check_policy_permission(no_rt_limit, fifo_10, rr_10),
            Err(AxError::OperationNotPermitted)
        );

        let idle = SchedulePolicy::fair(Nice::ZERO, FairMode::Idle);
        let normal = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        let no_nice_limit = SchedulerPermission {
            rlimit_nice: 0,
            ..permission
        };
        assert_eq!(
            check_policy_permission(no_nice_limit, idle, normal),
            Err(AxError::OperationNotPermitted)
        );

        let non_owner = SchedulerPermission {
            owns_target: false,
            ..permission
        };
        assert_eq!(
            check_policy_permission(non_owner, fair, fair),
            Err(AxError::OperationNotPermitted)
        );

        let deadline =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 2, 3, DeadlineFlags::NONE).unwrap());
        assert_eq!(
            check_policy_permission(permission, fair, deadline),
            Err(AxError::OperationNotPermitted)
        );
        let privileged = SchedulerPermission {
            has_cap_sys_nice: true,
            ..permission
        };
        assert_eq!(check_policy_permission(privileged, fair, deadline), Ok(()));
    }

    #[test]
    fn rt_thread_may_restore_its_stored_nice_without_extra_rlimit() {
        let current = SchedulePolicy::fifo(RtPriority::new(20).unwrap());
        let requested = SchedulePolicy::fair(Nice::new(-7).unwrap(), FairMode::Normal);
        let permission = SchedulerPermission {
            owns_target: true,
            has_cap_sys_nice: false,
            rlimit_rtprio: 0,
            rlimit_nice: 0,
            stored_nice: Nice::new(-7).unwrap(),
        };

        assert_eq!(
            check_policy_permission(permission, current, requested),
            Ok(())
        );
    }

    #[test]
    fn clamps_fair_nice_like_linux_sched_copy_attr() {
        let low = parse_sched_attr(
            SchedAttr::fair(SCHED_NORMAL, i32::MIN),
            SchedulePolicy::default(),
        )
        .unwrap();
        let high = parse_sched_attr(
            SchedAttr::fair(SCHED_NORMAL, i32::MAX),
            SchedulePolicy::default(),
        )
        .unwrap();

        assert_eq!(
            low.policy,
            SchedulePolicy::fair(Nice::new(-20).unwrap(), FairMode::Normal)
        );
        assert_eq!(
            high.policy,
            SchedulePolicy::fair(Nice::new(19).unwrap(), FairMode::Normal)
        );
    }

    #[test]
    fn only_privileged_callers_may_clear_reset_on_fork() {
        assert_eq!(
            check_reset_on_fork_permission(false, true, false),
            Err(AxError::OperationNotPermitted)
        );
        assert_eq!(check_reset_on_fork_permission(true, true, false), Ok(()));
        assert_eq!(check_reset_on_fork_permission(false, true, true), Ok(()));
        assert_eq!(check_reset_on_fork_permission(false, false, false), Ok(()));
    }

    #[test]
    fn exposes_linux_priority_ranges_and_default_rr_interval() {
        assert_eq!(scheduler_priority_min(SCHED_FIFO), Ok(1));
        assert_eq!(scheduler_priority_max(SCHED_RR), Ok(99));
        assert_eq!(scheduler_priority_min(SCHED_NORMAL), Ok(0));
        assert_eq!(scheduler_priority_max(SCHED_DEADLINE), Ok(0));
        assert_eq!(scheduler_priority_min(42), Err(AxError::InvalidInput));
        let SchedulePolicy::RoundRobin { quantum_ns, .. } =
            SchedulePolicy::round_robin(RtPriority::new(1).unwrap())
        else {
            panic!("round-robin constructor returned another policy class");
        };
        assert_eq!(quantum_ns, 5_000_000);
    }

    #[test]
    fn sched_attr_serialization_round_trips_deadline_flags() {
        let policy = SchedulePolicy::deadline(
            DeadlinePolicy::new(
                10,
                20,
                30,
                DeadlineFlags::RECLAIM | DeadlineFlags::DL_OVERRUN,
            )
            .unwrap(),
        );
        let attr = sched_attr_from_policy(policy, true);
        assert_eq!(attr.sched_policy, SCHED_DEADLINE);
        assert_eq!(linux_policy_number(policy), SCHED_DEADLINE);
        assert_eq!(linux_sched_priority(policy), 0);
        let update = parse_sched_attr(attr, SchedulePolicy::default()).unwrap();
        assert_eq!(update.policy, policy);
        assert!(update.reset_on_fork);
    }

    #[test]
    fn task_reset_metadata_is_the_only_serialized_reset_source() {
        let policy = SchedulePolicy::deadline(
            DeadlinePolicy::new(10, 20, 30, DeadlineFlags::RESET_ON_FORK).unwrap(),
        );

        let without_reset = sched_attr_from_policy(policy, false);
        assert_eq!(
            without_reset.sched_flags & SCHED_FLAG_RESET_ON_FORK as u64,
            0
        );

        let with_reset = sched_attr_from_policy(policy, true);
        assert_ne!(with_reset.sched_flags & SCHED_FLAG_RESET_ON_FORK as u64, 0);
    }
}
