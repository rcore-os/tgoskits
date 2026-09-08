//! Root-domain Fair NOHZ idle-load-balancer ownership.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;

#[path = "fair_nohz_state.rs"]
mod state;

use state::{FairNoHzClaim, FairNoHzPhase, FairNoHzState, FairNoHzTransition};

type RootDomainFairNoHzPhase = FairNoHzPhase<CpuId>;
type RootDomainFairNoHzState = FairNoHzState<CpuId>;
type RootDomainFairNoHzTransition = FairNoHzTransition<CpuId>;

/// Linux `nohz.idle_cpus_mask` and the Fair sources which can feed it.
///
/// Source and target publication use sequential consistency deliberately. A
/// source becoming pushable and a CPU selecting idle form a two-sided wakeup
/// handshake: whichever publication is second must observe the first and
/// either kick the idle owner or run new-idle balance locally. Acquire/Release
/// on two independent atomics would permit both sides to observe the old state.
#[derive(Debug)]
pub(super) struct RootDomainFairNoHz {
    pushable_sources: Vec<AtomicBool>,
    idle_targets: Vec<AtomicBool>,
    state: PreemptTicketLock<RootDomainFairNoHzState>,
    /// Zero means no published ILB; otherwise this is `CpuId + 1`.
    published_balancer: AtomicUsize,
    #[cfg(test)]
    source_publication_writes: Vec<AtomicUsize>,
}

#[derive(Debug)]
pub(in crate::system::task_system) struct RootDomainFairNoHzClaim(FairNoHzClaim<CpuId>);

impl RootDomainFairNoHz {
    pub(super) fn new(cpu_count: usize) -> Self {
        Self {
            pushable_sources: (0..cpu_count).map(|_| AtomicBool::new(false)).collect(),
            idle_targets: (0..cpu_count).map(|_| AtomicBool::new(false)).collect(),
            state: PreemptTicketLock::new(RootDomainFairNoHzState {
                requested_generation: 0,
                scan_generation: 0,
                source: CpuId::new(0),
                cursor: None,
                phase: RootDomainFairNoHzPhase::Idle,
            }),
            published_balancer: AtomicUsize::new(0),
            #[cfg(test)]
            source_publication_writes: (0..cpu_count).map(|_| AtomicUsize::new(0)).collect(),
        }
    }

    pub(super) fn publish_source(&self, cpu: CpuId, pushable: bool) -> bool {
        let source = &self.pushable_sources[cpu.as_usize()];
        // The rq owner is the sole online writer of its source bit. CPU
        // offline publication runs only after that owner is quiescent, so an
        // unordered load can filter unchanged commits without weakening the
        // SeqCst source/idle-target handshake on a real edge.
        if source.load(Ordering::Relaxed) == pushable {
            return false;
        }
        #[cfg(test)]
        self.source_publication_writes[cpu.as_usize()].fetch_add(1, Ordering::Relaxed);
        source.store(pushable, Ordering::SeqCst);
        pushable
    }

    #[cfg(test)]
    fn source_publication_writes(&self, cpu: CpuId) -> usize {
        self.source_publication_writes[cpu.as_usize()].load(Ordering::Relaxed)
    }

    pub(super) fn publish_idle_target(
        &self,
        cpu: CpuId,
        idle: bool,
        mut accepts: impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        // The owning scheduler is the sole writer of its own idle-target bit,
        // so an unordered load filters unchanged publications. The store keeps
        // the SeqCst NOHZ handshake: a racing Fair source must observe an
        // idle-entry publication together with the armed one-shot pull.
        if self.idle_targets[cpu.as_usize()].load(Ordering::Relaxed) == idle {
            return None;
        }
        self.idle_targets[cpu.as_usize()].store(idle, Ordering::SeqCst);
        if idle {
            return None;
        }

        {
            let mut state = self.state.lock();
            let transition = state.withdraw_balancer(cpu, |cursor, source| {
                self.find_next_balancer(cursor, source, &mut accepts)
            });
            self.publish_transition(transition)
        }
    }

    pub(super) fn is_idle_target(&self, cpu: CpuId) -> bool {
        self.idle_targets[cpu.as_usize()].load(Ordering::SeqCst)
    }

    pub(super) fn is_source(&self, cpu: CpuId) -> bool {
        self.pushable_sources[cpu.as_usize()].load(Ordering::SeqCst)
    }

    pub(super) fn request_idle_balance(
        &self,
        source: CpuId,
        mut accepts: impl FnMut(CpuId) -> bool,
        mut publish: impl FnMut(CpuId),
    ) {
        let target = {
            let mut state = self.state.lock();
            state.requested_generation = state
                .requested_generation
                .checked_add(1)
                .expect("Fair NOHZ balance generation exhausted");
            state.source = source;
            if state.phase != RootDomainFairNoHzPhase::Idle {
                None
            } else {
                state.scan_generation = state.requested_generation;
                state.cursor = None;
                let transition = state.publish_next(|cursor, source| {
                    self.find_next_balancer(cursor, source, &mut accepts)
                });
                self.publish_transition(transition)
            }
        };
        if let Some(target) = target {
            publish(target);
        }
    }

    pub(super) fn balancer_pending(&self, cpu: CpuId) -> bool {
        self.published_balancer.load(Ordering::Acquire) == Self::target_token(cpu)
    }

    pub(super) fn claim_balancer(&self, cpu: CpuId) -> Option<RootDomainFairNoHzClaim> {
        let mut state = self.state.lock();
        let claim = state.claim_balancer(cpu)?;
        self.publish_transition(RootDomainFairNoHzTransition::published(None));
        Some(RootDomainFairNoHzClaim(claim))
    }

    pub(super) fn finish_balancer(
        &self,
        claim: RootDomainFairNoHzClaim,
        serviced: bool,
        has_source: bool,
        mut accepts: impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        {
            let mut state = self.state.lock();
            let transition =
                state.finish_balancer(claim.0, serviced, has_source, |cursor, source| {
                    self.find_next_balancer(cursor, source, &mut accepts)
                });
            self.publish_transition(transition)
        }
    }

    pub(super) fn retarget_failed_delivery(
        &self,
        failed: CpuId,
        mut accepts: impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        {
            let mut state = self.state.lock();
            let transition = state.retarget_failed_delivery(failed, |cursor, source| {
                self.find_next_balancer(cursor, source, &mut accepts)
            });
            self.publish_transition(transition)
        }
    }

    pub(super) fn has_source(&self, runqueues: &[Arc<CpuRemote>]) -> bool {
        self.pushable_sources
            .iter()
            .zip(runqueues)
            .any(|(published, remote)| {
                published.load(Ordering::SeqCst)
                    && remote.accepts_placement()
                    && remote.is_scheduler_ready()
            })
    }

    fn find_next_balancer(
        &self,
        cursor: Option<CpuId>,
        source: CpuId,
        accepts: &mut impl FnMut(CpuId) -> bool,
    ) -> Option<CpuId> {
        let first = cursor.map_or(0, |cpu| cpu.as_usize().saturating_add(1));
        self.idle_targets
            .iter()
            .enumerate()
            .skip(first)
            .find_map(|(index, idle)| {
                let target = CpuId::new(index as u32);
                (target != source && idle.load(Ordering::SeqCst) && accepts(target))
                    .then_some(target)
            })
    }

    fn target_token(target: CpuId) -> usize {
        target
            .as_usize()
            .checked_add(1)
            .expect("a Fair NOHZ ILB target must fit the configured CPU topology")
    }

    fn publish_transition(&self, transition: RootDomainFairNoHzTransition) -> Option<CpuId> {
        if transition.changed {
            let token = transition.target.map_or(0, Self::target_token);
            self.published_balancer.store(token, Ordering::Release);
        }
        transition.target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn withdrawing_idle_membership_removes_the_cpu_from_ilb_selection() {
        let nohz = RootDomainFairNoHz::new(2);
        let source = CpuId::new(0);
        let balancer = CpuId::new(1);
        nohz.idle_targets[balancer.as_usize()].store(true, Ordering::SeqCst);
        assert_eq!(
            nohz.find_next_balancer(None, source, &mut |_| true),
            Some(balancer)
        );
        nohz.idle_targets[balancer.as_usize()].store(false, Ordering::SeqCst);

        assert!(!nohz.is_idle_target(balancer));
        assert_eq!(nohz.find_next_balancer(None, source, &mut |_| true), None);
    }

    #[test]
    fn unchanged_fair_source_does_not_republish_the_seqcst_edge() {
        let nohz = RootDomainFairNoHz::new(1);
        let owner = CpuId::new(0);

        assert!(nohz.publish_source(owner, true));
        assert_eq!(nohz.source_publication_writes(owner), 1);
        assert!(!nohz.publish_source(owner, true));
        assert_eq!(nohz.source_publication_writes(owner), 1);
    }

    #[test]
    fn one_fair_source_edge_selects_one_idle_balancer() {
        let nohz = RootDomainFairNoHz::new(3);
        let source = CpuId::new(0);
        let first_idle = CpuId::new(1);
        let second_idle = CpuId::new(2);
        nohz.publish_idle_target(first_idle, true, |_| true);
        nohz.publish_idle_target(second_idle, true, |_| true);

        let selected = nohz.find_next_balancer(None, source, &mut |_| true);

        assert_eq!(selected, Some(first_idle));
        assert_ne!(selected, Some(second_idle));
    }

    #[test]
    fn withdrawing_a_published_balancer_retargets_the_same_generation() {
        let source = CpuId::new(0);
        let first_idle = CpuId::new(1);
        let second_idle = CpuId::new(2);
        let mut state = RootDomainFairNoHzState {
            requested_generation: 1,
            scan_generation: 1,
            source,
            cursor: None,
            phase: RootDomainFairNoHzPhase::Published(first_idle),
        };

        let retargeted = state.withdraw_balancer(first_idle, |cursor, selected_source| {
            assert_eq!(cursor, Some(first_idle));
            assert_eq!(selected_source, source);
            Some(second_idle)
        });

        assert_eq!(retargeted.target, Some(second_idle));
        assert_eq!(state.phase, RootDomainFairNoHzPhase::Published(second_idle));
    }

    #[test]
    fn withdrawing_a_claimed_balancer_retargets_without_stale_completion_clobbering_it() {
        let source = CpuId::new(0);
        let first_idle = CpuId::new(1);
        let second_idle = CpuId::new(2);
        let claim = FairNoHzClaim {
            balancer: first_idle,
            generation: 1,
        };
        let mut state = RootDomainFairNoHzState {
            requested_generation: 1,
            scan_generation: 1,
            source,
            cursor: None,
            phase: RootDomainFairNoHzPhase::Claimed(first_idle),
        };

        let retargeted = state.withdraw_balancer(first_idle, |cursor, selected_source| {
            assert_eq!(cursor, Some(first_idle));
            assert_eq!(selected_source, source);
            Some(second_idle)
        });

        assert_eq!(retargeted.target, Some(second_idle));
        assert_eq!(state.phase, RootDomainFairNoHzPhase::Published(second_idle));
        assert_eq!(
            state.finish_balancer(claim, false, true, |_, _| {
                panic!("a stale completion must not select another ILB owner")
            }),
            RootDomainFairNoHzTransition::UNCHANGED,
            "completion from the withdrawn owner must be stale"
        );
        assert_eq!(state.phase, RootDomainFairNoHzPhase::Published(second_idle));
    }
}
