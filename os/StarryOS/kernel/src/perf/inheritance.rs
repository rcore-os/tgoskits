//! Linux-style ownership for one task-bound perf event and its descendants.
//!
//! Linux links every inherited event to the original parent event's
//! `child_list`. Control operations snapshot that relationship under the event
//! lock, then perform CPU-affine work after releasing it. This module provides
//! the same boundary: [`FamilyState`] is the sole relationship/output owner,
//! while each [`PerTaskCounter`] independently owns one scheduler-visible PMU
//! lease.

use alloc::sync::{Arc, Weak};
use core::sync::atomic::Ordering;

use ax_errno::{AxError, AxResult};
use ax_sync::PiMutex;

use super::{
    hw,
    hw_owner::Counter,
    inheritance_lifecycle::PerfInheritanceLifecycle,
    output::{PerfRingOutput, PerfRingWeak},
    task::{
        PERF_TASK_ACTIVE, PerTaskCounter, SamplingAnchors, attach, detach_unpublished, free_hw,
    },
};
use crate::task::Thread;

const MAX_FAMILY_MEMBERS: usize = 32;

#[derive(Clone)]
struct FamilyOwnOutput {
    ring: PerfRingWeak,
    anchors: SamplingAnchors,
}

#[derive(Clone, Copy, Debug, Default)]
struct RetiredTotals {
    value: u64,
    time_enabled: u64,
    time_running: u64,
}

impl RetiredTotals {
    fn add(&mut self, values: (u64, u64, u64)) {
        self.value = self.value.saturating_add(values.0);
        self.time_enabled = self.time_enabled.saturating_add(values.1);
        self.time_running = self.time_running.saturating_add(values.2);
    }
}

struct FamilyState {
    lifecycle: PerfInheritanceLifecycle,
    members: heapless::Vec<Arc<PerTaskCounter>, MAX_FAMILY_MEMBERS>,
    retired: RetiredTotals,
    own_output: Option<FamilyOwnOutput>,
    redirect: Option<PerfRingOutput>,
}

impl FamilyState {
    fn effective_output(&self) -> Option<(PerfRingOutput, Option<SamplingAnchors>)> {
        if let Some(output) = &self.redirect {
            return Some((output.clone(), None));
        }
        let own = self.own_output.as_ref()?;
        Some((own.ring.upgrade()?, Some(own.anchors.clone())))
    }
}

/// One fd-owned perf event family.
///
/// `control` serializes userspace control transactions. `state` is held only
/// while publishing intent or snapshotting bounded member references; owner-CPU
/// worker waits always happen after `state` is released.
pub(crate) struct PerfInheritanceFamily {
    control: PiMutex<()>,
    state: PiMutex<FamilyState>,
}

impl core::fmt::Debug for PerfInheritanceFamily {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let state = self.state.lock();
        formatter
            .debug_struct("PerfInheritanceFamily")
            .field("lifecycle", &state.lifecycle)
            .field("members", &state.members.len())
            .finish_non_exhaustive()
    }
}

impl PerfInheritanceFamily {
    /// Creates the family and binds its root member before scheduler visibility.
    pub(crate) fn new(root: Arc<PerTaskCounter>, enabled: bool) -> Arc<Self> {
        let mut members = heapless::Vec::new();
        members
            .push(Arc::clone(&root))
            .expect("a new perf family always has room for its root");
        let family = Arc::new(Self {
            control: PiMutex::new(()),
            state: PiMutex::new(FamilyState {
                lifecycle: PerfInheritanceLifecycle::new(enabled),
                members,
                retired: RetiredTotals::default(),
                own_output: None,
                redirect: None,
            }),
        });
        root.bind_family(Arc::downgrade(&family), true);
        family
    }

    /// Returns the original event member.
    pub(crate) fn root(&self) -> Arc<PerTaskCounter> {
        Arc::clone(
            self.state
                .lock()
                .members
                .first()
                .expect("a perf family retains its root"),
        )
    }

    /// Returns whether descendants may still join this family.
    pub(crate) fn is_open(&self) -> bool {
        !self.state.lock().lifecycle.is_closed()
    }

    /// Registers a pre-scheduler child under the current family intent.
    ///
    /// The child is fully configured before it is appended. A concurrent close
    /// either observes the member or rejects it; no half-published descendant is
    /// reachable from the family.
    pub(crate) fn register_child(self: &Arc<Self>, child: &Arc<PerTaskCounter>) -> AxResult<()> {
        // Match Linux's child_mutex boundary: inheritance cannot enter midway
        // through a family ENABLE/DISABLE/RESET/output transaction.
        let _control = self.control.lock();
        child.bind_family(Arc::downgrade(self), false);
        let mut state = self.state.lock();
        let join = state
            .lifecycle
            .register_member(MAX_FAMILY_MEMBERS)
            .ok_or_else(|| {
                if state.lifecycle.is_closed() {
                    AxError::BadState
                } else {
                    AxError::NoMemory
                }
            })?;
        child.set_enabled_state(join.enabled);
        if let Some((output, anchors)) = state.effective_output() {
            child.install_family_output(output, anchors);
        }
        state
            .members
            .push(Arc::clone(child))
            .expect("lifecycle capacity and member storage must agree");
        Ok(())
    }

    /// Folds one quiescent descendant into the root aggregate and releases its
    /// live relationship slot.
    ///
    /// Linux removes an exited child event from `child_list` after syncing its
    /// count into the parent. Keeping every historical child strongly owned
    /// would make the fixed live-member capacity a lifetime fork limit.
    pub(crate) fn retire_child(&self, child: &Arc<PerTaskCounter>) -> bool {
        let values = child.retired_values();
        let mut state = self.state.lock();
        let Some(index) = state
            .members
            .iter()
            .position(|member| Arc::ptr_eq(member, child))
        else {
            return false;
        };
        if index == 0 {
            return false;
        }
        state.members.swap_remove(index);
        assert!(
            state.lifecycle.retire_member(),
            "perf family membership count diverged from its live member storage"
        );
        state.retired.add(values);
        true
    }

    /// Publishes the root mmap output to every descendant, including members
    /// inherited before userspace mapped the ring.
    pub(crate) fn publish_root_output(
        &self,
        output: &PerfRingOutput,
        anchors: SamplingAnchors,
    ) -> AxResult<()> {
        let _control = self.control.lock();
        let mut state = self.state.lock();
        if state.lifecycle.publish_output().is_none() {
            return Err(AxError::BadState);
        }
        let root = Arc::clone(
            state
                .members
                .first()
                .expect("a perf family retains its root"),
        );
        root.install_root_output(output, anchors.clone());
        state.own_output = Some(FamilyOwnOutput {
            ring: output.downgrade(),
            anchors: anchors.clone(),
        });
        let effective = state.effective_output();
        for child in state.members.iter().skip(1) {
            match &effective {
                Some((ring, child_anchors)) => {
                    child.install_family_output(ring.clone(), child_anchors.clone())
                }
                None => child.clear_family_output(),
            }
        }
        Ok(())
    }

    /// Applies root-fd ENABLE to every existing descendant and to future joins.
    pub(crate) fn enable(&self) -> AxResult<()> {
        let _control = self.control.lock();
        let members = {
            let mut state = self.state.lock();
            state.lifecycle.set_enabled(true).ok_or(AxError::BadState)?;
            state.members.clone()
        };
        for member in &members {
            member.set_enabled();
        }
        let mut first_error = None;
        for member in &members {
            if let Err(error) = member.synchronize_context()
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Applies root-fd DISABLE to every existing descendant.
    pub(crate) fn disable(&self) -> AxResult<()> {
        let _control = self.control.lock();
        let members = {
            let mut state = self.state.lock();
            if state.lifecycle.is_closed() {
                return Ok(());
            }
            state
                .lifecycle
                .set_enabled(false)
                .expect("an open family accepts control intent");
            state.members.clone()
        };
        let mut first_error = None;
        for member in &members {
            if let Err(error) = super::task::disable_counter(member)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Resets all descendants that existed at the ioctl linearization point.
    pub(crate) fn reset(&self) -> AxResult<()> {
        let _control = self.control.lock();
        let members = {
            let state = self.state.lock();
            if state.lifecycle.is_closed() {
                return Err(AxError::BadState);
            }
            state.members.clone()
        };
        let mut first_error = None;
        for member in &members {
            if let Err(error) = super::task::reset_counter(member)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Aggregates root and descendant values like Linux
    /// `__perf_event_read_value()`.
    pub(crate) fn read(&self) -> AxResult<(u64, u64, u64)> {
        let _control = self.control.lock();
        let (members, retired) = {
            let state = self.state.lock();
            (state.members.clone(), state.retired)
        };
        let mut value = retired.value;
        let mut time_enabled = retired.time_enabled;
        let mut time_running = retired.time_running;
        for member in &members {
            let (member_value, member_enabled, member_running) = super::task::read_counter(member)?;
            value = value.saturating_add(member_value);
            time_enabled = time_enabled.saturating_add(member_enabled);
            time_running = time_running.saturating_add(member_running);
        }
        Ok((value, time_enabled, time_running))
    }

    /// Publishes the wrapper event id before any child can be inherited.
    pub(crate) fn set_sample_id(&self, id: u64) {
        self.root().set_sample_id(id);
    }

    /// Redirects all current and future family members to another event output.
    pub(crate) fn redirect_output(&self, output: PerfRingOutput) -> AxResult<()> {
        self.replace_redirect(Some(output))
    }

    /// Removes an explicit redirect and restores the root mmap output.
    pub(crate) fn detach_output(&self) -> AxResult<()> {
        self.replace_redirect(None)
    }

    fn replace_redirect(&self, output: Option<PerfRingOutput>) -> AxResult<()> {
        let _control = self.control.lock();
        let (restore_enabled, members) = {
            let mut state = self.state.lock();
            if state.lifecycle.is_closed() {
                return Err(AxError::BadState);
            }
            let restore_enabled = state.lifecycle.enabled();
            state
                .lifecycle
                .set_enabled(false)
                .expect("an open family accepts control intent");
            (restore_enabled, state.members.clone())
        };
        let mut first_error = None;
        for member in &members {
            if let Err(error) = super::task::disable_counter(member)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        {
            let mut state = self.state.lock();
            state.redirect = output;
            let effective = state.effective_output();
            for (index, member) in state.members.iter().enumerate() {
                if index == 0 {
                    match &state.redirect {
                        Some(output) => member.set_redirect_ring(output.clone()),
                        None => member.detach_redirect(),
                    }
                    continue;
                }
                match &effective {
                    Some((ring, anchors)) => {
                        member.install_family_output(ring.clone(), anchors.clone())
                    }
                    None => member.clear_family_output(),
                }
            }
            state
                .lifecycle
                .set_enabled(restore_enabled)
                .expect("control serialization prevents close");
        }
        if restore_enabled {
            for member in &members {
                member.set_enabled();
            }
        }
        Ok(())
    }

    /// Permanently quiesces every member before releasing the shared output.
    pub(crate) fn close(&self) -> AxResult<()> {
        let _control = self.control.lock();
        let members = {
            let mut state = self.state.lock();
            state.lifecycle.close();
            state.members.clone()
        };
        let mut first_error = None;
        for member in &members {
            if let Err(error) = super::task::free_hw(member)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }

        let own_output = {
            let mut state = self.state.lock();
            state.redirect = None;
            state.own_output.take()
        };
        if let Some(output) = own_output {
            output.anchors.stop();
        }
        for member in &members {
            member.clear_family_output();
        }
        Ok(())
    }
}

/// Clones each inheritable parent event into a child before scheduler
/// publication, flattening every generation back onto the original family.
pub fn on_clone_inherit(parent_thr: &Thread, child_thr: &Thread) {
    if PERF_TASK_ACTIVE.load(Ordering::Acquire) == 0 {
        return;
    }
    let Some(parents) = parent_thr.perf_context().snapshot_for_inherit() else {
        return;
    };
    for parent in &parents {
        if !parent.inheritable() {
            continue;
        }
        let Some(family) = parent.family().filter(|family| family.is_open()) else {
            continue;
        };
        let Some(n) = hw::alloc_programmable_counter() else {
            warn!(
                "perf: attr.inherit skipped for child tid {} (no free PMU counter)",
                child_thr.tid()
            );
            continue;
        };
        let Some(scheduler_id) = child_thr.scheduler_id() else {
            warn!(
                "perf: attr.inherit skipped for child tid {} (scheduler identity unavailable)",
                child_thr.tid()
            );
            super::hw_allocation::free_counter(Counter::Programmable(n));
            continue;
        };
        let child = Arc::new(PerTaskCounter::new(
            parent.inherited_config(scheduler_id, Counter::Programmable(n)),
        ));
        child.set_sample_id(parent.sample_id());

        // `do_clone` has not made the child schedulable yet. Publish the local
        // scheduler-list reservation before family close can observe it.
        if let Err(error) = attach(child_thr, Arc::clone(&child)) {
            free_hw(&child).expect("an unpublished inherited event must roll back locally");
            if error != AxError::NoSuchProcess {
                warn!(
                    "perf: attr.inherit skipped for child tid {}: {error}",
                    child_thr.tid()
                );
            }
            continue;
        }
        if let Err(error) = family.register_child(&child) {
            detach_unpublished(child_thr, &child);
            free_hw(&child).expect("unpublished inherited counter must roll back locally");
            if error != AxError::BadState {
                warn!(
                    "perf: attr.inherit skipped for child tid {}: {error}",
                    child_thr.tid()
                );
            }
        }
    }
}

/// Weak family identity retained by scheduler-visible members.
pub(crate) type PerfInheritanceFamilyWeak = Weak<PerfInheritanceFamily>;
