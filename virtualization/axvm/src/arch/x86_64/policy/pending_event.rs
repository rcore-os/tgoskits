//! Backend-independent x86 event queue state.

use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingEventSource {
    FixedApic,
    LegacyPic,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingEventKind {
    ExternalInterrupt(PendingEventSource),
    Exception,
}

pub(crate) const fn is_valid_fixed_apic_vector(vector: u8) -> bool {
    // The architecture reserves vectors 0..=31 for exceptions and interrupts.
    vector >= 32
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PendingEvent {
    pub(crate) vector: u8,
    pub(crate) err_code: Option<u32>,
    pub(crate) level_triggered: bool,
    pub(crate) kind: PendingEventKind,
}

impl PendingEvent {
    pub(crate) const fn external_interrupt(
        vector: u8,
        level_triggered: bool,
        source: PendingEventSource,
    ) -> Self {
        Self {
            vector,
            err_code: None,
            level_triggered,
            kind: PendingEventKind::ExternalInterrupt(source),
        }
    }

    pub(crate) const fn exception(vector: u8, err_code: Option<u32>) -> Self {
        Self {
            vector,
            err_code,
            level_triggered: false,
            kind: PendingEventKind::Exception,
        }
    }

    pub(crate) const fn is_external_interrupt(self) -> bool {
        matches!(self.kind, PendingEventKind::ExternalInterrupt(_))
    }

    pub(crate) const fn is_exception(self) -> bool {
        matches!(self.kind, PendingEventKind::Exception)
    }

    pub(crate) fn requires_vlapic_accept(self) -> bool {
        matches!(
            self.kind,
            PendingEventKind::ExternalInterrupt(PendingEventSource::FixedApic)
        )
    }
}

pub(crate) fn queue_pending_event(queue: &mut VecDeque<PendingEvent>, event: PendingEvent) {
    if event.is_external_interrupt()
        && event.vector >= 32
        && queue
            .iter()
            .any(|pending| pending.vector == event.vector && pending.kind == event.kind)
    {
        return;
    }
    queue.push_back(event);
}

/// Selects a deliverable event without violating vLAPIC PPR.
///
/// Legacy PIC interrupts bypass vLAPIC priority arbitration, while fixed APIC
/// interrupts retain the PPR priority-class check. Both remain maskable
/// external interrupts. The earliest eligible PIC request keeps its order
/// relative to the highest-priority eligible fixed interrupt.
pub(crate) fn select_pending_event(
    queue: &VecDeque<PendingEvent>,
    interrupts_allowed: bool,
    mut can_accept_fixed_apic: impl FnMut(u8) -> bool,
) -> Option<(usize, PendingEvent)> {
    if let Some((index, exception)) = queue
        .iter()
        .copied()
        .enumerate()
        .find(|(_, event)| event.is_exception())
    {
        return Some((index, exception));
    }
    if !interrupts_allowed {
        return None;
    }

    let mut first_pic = None;
    let mut highest_fixed_apic: Option<(usize, PendingEvent)> = None;
    for (index, event) in queue.iter().copied().enumerate() {
        match event.kind {
            PendingEventKind::ExternalInterrupt(PendingEventSource::LegacyPic) => {
                first_pic.get_or_insert((index, event));
            }
            PendingEventKind::ExternalInterrupt(PendingEventSource::FixedApic)
                if can_accept_fixed_apic(event.vector) =>
            {
                if highest_fixed_apic.is_none_or(|(_, highest)| event.vector > highest.vector) {
                    highest_fixed_apic = Some((index, event));
                }
            }
            PendingEventKind::ExternalInterrupt(PendingEventSource::FixedApic)
            | PendingEventKind::Exception => {}
        }
    }

    match (first_pic, highest_fixed_apic) {
        (Some(pic), Some(fixed)) => Some(if pic.0 < fixed.0 { pic } else { fixed }),
        (Some(pic), None) => Some(pic),
        (None, Some(fixed)) => Some(fixed),
        (None, None) => None,
    }
}

pub(crate) fn needs_interrupt_window(
    queue: &VecDeque<PendingEvent>,
    interrupts_allowed: bool,
) -> bool {
    !interrupts_allowed && queue.iter().any(|event| event.is_external_interrupt())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::{
        PendingEvent, PendingEventKind, PendingEventSource, needs_interrupt_window,
        queue_pending_event, select_pending_event,
    };

    fn external_event(vector: u8) -> PendingEvent {
        PendingEvent::external_interrupt(vector, false, PendingEventSource::FixedApic)
    }

    fn pic_event(vector: u8) -> PendingEvent {
        PendingEvent::external_interrupt(vector, false, PendingEventSource::LegacyPic)
    }

    #[test]
    fn repeated_external_vector_has_one_pending_owner() {
        let mut queue = VecDeque::new();

        queue_pending_event(&mut queue, external_event(0x31));
        queue_pending_event(&mut queue, external_event(0x31));
        queue_pending_event(&mut queue, external_event(0x32));

        assert_eq!(queue.len(), 2);
        assert_eq!(queue[0].vector, 0x31);
        assert_eq!(queue[1].vector, 0x32);
    }

    #[test]
    fn repeated_exceptions_remain_distinct_events() {
        let mut queue = VecDeque::new();
        let exception = PendingEvent::exception(14, Some(1));

        queue_pending_event(&mut queue, exception);
        queue_pending_event(&mut queue, exception);

        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn fixed_interrupt_selection_skips_events_blocked_by_ppr() {
        let mut queue = VecDeque::new();
        queue.push_back(external_event(0x41));
        queue.push_back(external_event(0x61));

        assert_eq!(
            select_pending_event(&queue, true, |vector| vector & 0xf0 > 0x50),
            Some((1, external_event(0x61)))
        );
        assert_eq!(
            select_pending_event(&queue, true, |_| true),
            Some((1, external_event(0x61)))
        );
        assert!(select_pending_event(&queue, true, |_| false).is_none());
        assert!(!needs_interrupt_window(&queue, true));

        let waiting = VecDeque::from([external_event(0x41)]);
        assert!(select_pending_event(&waiting, true, |_| false).is_none());
        assert_eq!(
            select_pending_event(&waiting, true, |vector| vector & 0xf0 > 0x30),
            Some((0, external_event(0x41)))
        );
        assert_eq!(
            waiting.len(),
            1,
            "blocked event remains pending until eligible"
        );

        let low_fixed = VecDeque::from([external_event(0x20)]);
        assert!(!super::is_valid_fixed_apic_vector(0));
        assert!(!super::is_valid_fixed_apic_vector(16));
        assert!(!super::is_valid_fixed_apic_vector(31));
        assert!(super::is_valid_fixed_apic_vector(32));
        assert!(super::is_valid_fixed_apic_vector(u8::MAX));
        assert!(select_pending_event(&low_fixed, true, |_| false).is_none());
        assert_eq!(
            select_pending_event(&low_fixed, true, |_| true),
            Some((0, external_event(0x20)))
        );
    }

    #[test]
    fn legacy_pic_bypasses_ppr_but_not_cpu_interruptibility() {
        let mut queue = VecDeque::new();
        queue.push_back(pic_event(8));

        assert_eq!(
            select_pending_event(&queue, true, |_| false),
            Some((0, pic_event(8)))
        );
        assert!(select_pending_event(&queue, false, |_| true).is_none());
        assert!(needs_interrupt_window(&queue, false));
        assert!(!needs_interrupt_window(&queue, true));
    }

    #[test]
    fn low_legacy_pic_vector_is_selected_when_fixed_apic_is_blocked_by_ppr() {
        let queue = VecDeque::from([external_event(0x41), pic_event(8)]);

        assert_eq!(
            select_pending_event(&queue, true, |_| false),
            Some((1, pic_event(8)))
        );
        assert!(select_pending_event(&queue, false, |_| false).is_none());
        assert!(needs_interrupt_window(&queue, false));
    }

    #[test]
    fn exceptions_bypass_ppr_and_interrupt_flag_checks() {
        let mut queue = VecDeque::new();
        let exception = PendingEvent::exception(8, Some(0));
        queue.push_back(pic_event(8));
        queue.push_back(exception);

        assert_eq!(
            select_pending_event(&queue, false, |_| false),
            Some((1, exception))
        );
    }

    #[test]
    fn equal_vectors_from_pic_and_fixed_apic_keep_separate_ownership() {
        let mut queue = VecDeque::new();

        queue_pending_event(&mut queue, external_event(0x31));
        queue_pending_event(&mut queue, pic_event(0x31));
        queue_pending_event(&mut queue, pic_event(0x31));

        assert_eq!(queue.len(), 2);
        assert_eq!(
            queue[0].kind,
            PendingEventKind::ExternalInterrupt(PendingEventSource::FixedApic)
        );
        assert_eq!(
            queue[1].kind,
            PendingEventKind::ExternalInterrupt(PendingEventSource::LegacyPic)
        );
    }

    #[test]
    fn only_fixed_external_interrupts_enter_the_vlapic_isr() {
        assert!(external_event(0x41).requires_vlapic_accept());
        assert!(!pic_event(0x41).requires_vlapic_accept());
        assert!(!PendingEvent::exception(14, None).requires_vlapic_accept());
    }
}
