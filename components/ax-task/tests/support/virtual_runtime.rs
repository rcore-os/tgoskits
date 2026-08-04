//! Allocation-free deterministic physical runtime state.

use ax_task::runtime::RuntimeStatus;

use super::MAX_TEST_CPUS;

const MAX_EVENTS: usize = 256;

/// Observable transitions made by the deterministic runtime transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VirtualRuntimeEventKind {
    SchedulerWorkPublished,
    IpiEdgePublished,
    IpiEdgeCoalesced,
    IpiClaimed,
    IdleCommitAborted,
    IdleCommitted,
    SchedulerFrameEntered,
    ContextSwitched,
    SwitchTailCompleted,
    SchedulerFrameExited,
}

/// One totally ordered virtual hardware/runtime event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualRuntimeEvent {
    pub sequence: u64,
    pub cpu: u32,
    pub kind: VirtualRuntimeEventKind,
    pub generation: u64,
    pub previous_context: usize,
    pub next_context: usize,
}

impl VirtualRuntimeEvent {
    const EMPTY: Self = Self {
        sequence: 0,
        cpu: 0,
        kind: VirtualRuntimeEventKind::SchedulerWorkPublished,
        generation: 0,
        previous_context: 0,
        next_context: 0,
    };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VirtualIdleState {
    Running,
    Sleeping,
}

#[derive(Clone, Copy)]
pub(super) struct VirtualCpuState {
    pub(super) online: bool,
    pub(super) ipi_published_epoch: u64,
    pub(super) ipi_claimed_epoch: u64,
    pub(super) ipi_edge_pending: bool,
    pub(super) ipi_send_count: usize,
    pub(super) scheduler_work_generation: u64,
    pub(super) scheduler_work_pending: bool,
    pub(super) idle_state: VirtualIdleState,
    pub(super) scheduler_frame_depth: usize,
    pub(super) current_context: usize,
    pub(super) outgoing_context: usize,
    pub(super) switch_tail_pending: bool,
}

impl VirtualCpuState {
    const fn new() -> Self {
        Self {
            online: false,
            ipi_published_epoch: 0,
            ipi_claimed_epoch: 0,
            ipi_edge_pending: false,
            ipi_send_count: 0,
            scheduler_work_generation: 0,
            scheduler_work_pending: false,
            idle_state: VirtualIdleState::Running,
            scheduler_frame_depth: 0,
            current_context: 0,
            outgoing_context: 0,
            switch_tail_pending: false,
        }
    }
}

pub(super) struct VirtualRuntimeState {
    pub(super) cpus: [VirtualCpuState; MAX_TEST_CPUS],
    next_event_sequence: u64,
    events: [VirtualRuntimeEvent; MAX_EVENTS],
    event_start: usize,
    event_count: usize,
}

impl VirtualRuntimeState {
    pub(super) const fn new() -> Self {
        Self {
            cpus: [VirtualCpuState::new(); MAX_TEST_CPUS],
            next_event_sequence: 1,
            events: [VirtualRuntimeEvent::EMPTY; MAX_EVENTS],
            event_start: 0,
            event_count: 0,
        }
    }

    pub(super) fn cpu(&self, cpu: u32) -> Option<&VirtualCpuState> {
        self.cpus.get(cpu as usize)
    }

    pub(super) fn cpu_mut(&mut self, cpu: u32) -> Option<&mut VirtualCpuState> {
        self.cpus.get_mut(cpu as usize)
    }

    pub(super) fn record(
        &mut self,
        cpu: u32,
        kind: VirtualRuntimeEventKind,
        generation: u64,
        previous_context: usize,
        next_context: usize,
    ) {
        let sequence = self.next_event_sequence;
        self.next_event_sequence = sequence
            .checked_add(1)
            .expect("virtual runtime event sequence exhausted");
        let index = if self.event_count < MAX_EVENTS {
            let index = (self.event_start + self.event_count) % MAX_EVENTS;
            self.event_count += 1;
            index
        } else {
            let index = self.event_start;
            self.event_start = (self.event_start + 1) % MAX_EVENTS;
            index
        };
        self.events[index] = VirtualRuntimeEvent {
            sequence,
            cpu,
            kind,
            generation,
            previous_context,
            next_context,
        };
    }

    pub(super) fn publish_scheduler_work(&mut self, cpu: u32) -> Result<u64, RuntimeStatus> {
        let state = self.cpu_mut(cpu).ok_or(RuntimeStatus::InvalidArgument)?;
        state.scheduler_work_generation = state
            .scheduler_work_generation
            .checked_add(1)
            .expect("virtual scheduler-work generation exhausted");
        state.scheduler_work_pending = true;
        let generation = state.scheduler_work_generation;
        self.record(
            cpu,
            VirtualRuntimeEventKind::SchedulerWorkPublished,
            generation,
            0,
            0,
        );
        Ok(generation)
    }

    pub(super) fn publish_ipi(&mut self, cpu: u32) -> RuntimeStatus {
        let Some(state) = self.cpu_mut(cpu) else {
            return RuntimeStatus::InvalidArgument;
        };
        state.ipi_published_epoch = state
            .ipi_published_epoch
            .checked_add(1)
            .expect("virtual scheduler IPI epoch exhausted");
        let epoch = state.ipi_published_epoch;
        let kind = if state.ipi_edge_pending {
            VirtualRuntimeEventKind::IpiEdgeCoalesced
        } else {
            state.ipi_edge_pending = true;
            state.ipi_send_count = state
                .ipi_send_count
                .checked_add(1)
                .expect("virtual scheduler IPI count exhausted");
            VirtualRuntimeEventKind::IpiEdgePublished
        };
        self.record(cpu, kind, epoch, 0, 0);
        RuntimeStatus::Success
    }

    pub(super) fn claim_ipi(&mut self, cpu: u32) -> Option<u64> {
        let state = self.cpu_mut(cpu)?;
        if !core::mem::replace(&mut state.ipi_edge_pending, false) {
            return None;
        }
        let epoch = state.ipi_published_epoch;
        state.ipi_claimed_epoch = epoch;
        state.idle_state = VirtualIdleState::Running;
        self.record(cpu, VirtualRuntimeEventKind::IpiClaimed, epoch, 0, 0);
        Some(epoch)
    }

    pub(super) fn events(&self) -> Vec<VirtualRuntimeEvent> {
        (0..self.event_count)
            .map(|offset| self.events[(self.event_start + offset) % MAX_EVENTS])
            .collect()
    }

    pub(super) fn clear_events(&mut self) {
        self.event_start = 0;
        self.event_count = 0;
    }
}
