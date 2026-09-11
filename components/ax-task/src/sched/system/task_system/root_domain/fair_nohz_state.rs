//! Pure Fair NOHZ ILB owner and generation transitions.

use core::fmt::Debug;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FairNoHzPhase<Cpu> {
    Idle,
    Published(Cpu),
    Claimed(Cpu),
}

#[derive(Debug)]
pub(crate) struct FairNoHzState<Cpu> {
    pub(crate) requested_generation: u64,
    pub(crate) scan_generation: u64,
    pub(crate) source: Cpu,
    pub(crate) cursor: Option<Cpu>,
    pub(crate) phase: FairNoHzPhase<Cpu>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FairNoHzTransition<Cpu> {
    pub(crate) changed: bool,
    pub(crate) target: Option<Cpu>,
}

impl<Cpu> FairNoHzTransition<Cpu> {
    pub(crate) const UNCHANGED: Self = Self {
        changed: false,
        target: None,
    };

    pub(crate) fn published(target: Option<Cpu>) -> Self {
        Self {
            changed: true,
            target,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FairNoHzClaim<Cpu> {
    pub(crate) balancer: Cpu,
    pub(crate) generation: u64,
}

impl<Cpu: Copy + Debug + Eq> FairNoHzState<Cpu> {
    pub(crate) fn publish_next(
        &mut self,
        mut select: impl FnMut(Option<Cpu>, Cpu) -> Option<Cpu>,
    ) -> FairNoHzTransition<Cpu> {
        let target = select(self.cursor, self.source);
        self.phase = target.map_or(FairNoHzPhase::Idle, FairNoHzPhase::Published);
        FairNoHzTransition::published(target)
    }

    pub(crate) fn withdraw_balancer(
        &mut self,
        cpu: Cpu,
        select: impl FnMut(Option<Cpu>, Cpu) -> Option<Cpu>,
    ) -> FairNoHzTransition<Cpu> {
        if !matches!(
            self.phase,
            FairNoHzPhase::Published(owner) | FairNoHzPhase::Claimed(owner) if owner == cpu
        ) {
            return FairNoHzTransition::UNCHANGED;
        }
        self.cursor = Some(cpu);
        self.publish_next(select)
    }

    pub(crate) fn claim_balancer(&mut self, cpu: Cpu) -> Option<FairNoHzClaim<Cpu>> {
        if self.phase != FairNoHzPhase::Published(cpu) {
            return None;
        }
        self.phase = FairNoHzPhase::Claimed(cpu);
        Some(FairNoHzClaim {
            balancer: cpu,
            generation: self.scan_generation,
        })
    }

    pub(crate) fn finish_balancer(
        &mut self,
        claim: FairNoHzClaim<Cpu>,
        serviced: bool,
        has_source: bool,
        mut select: impl FnMut(Option<Cpu>, Cpu) -> Option<Cpu>,
    ) -> FairNoHzTransition<Cpu> {
        // Owner withdrawal may retarget this generation while the old owner
        // is leaving idle or being offlined. Like Linux's NOHZ kick flags, an
        // already consumed owner snapshot is advisory: its late completion
        // must not clear a newer published owner, including reuse of the same
        // CPU for a later generation.
        if self.phase != FairNoHzPhase::Claimed(claim.balancer)
            || self.scan_generation != claim.generation
        {
            return FairNoHzTransition::UNCHANGED;
        }

        if !serviced {
            self.cursor = Some(claim.balancer);
            let transition = self.publish_next(&mut select);
            if transition.target.is_some() {
                return transition;
            }
        }
        if has_source && self.scan_generation != self.requested_generation {
            self.scan_generation = self.requested_generation;
            self.cursor = None;
            return self.publish_next(select);
        }
        self.phase = FairNoHzPhase::Idle;
        FairNoHzTransition::published(None)
    }

    pub(crate) fn retarget_failed_delivery(
        &mut self,
        failed: Cpu,
        select: impl FnMut(Option<Cpu>, Cpu) -> Option<Cpu>,
    ) -> FairNoHzTransition<Cpu> {
        if self.phase != FairNoHzPhase::Published(failed) {
            return FairNoHzTransition::UNCHANGED;
        }
        self.cursor = Some(failed);
        self.publish_next(select)
    }
}
