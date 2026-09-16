//! Backend-independent x86 event queue state.

use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PendingEvent {
    pub(crate) vector: u8,
    pub(crate) err_code: Option<u32>,
    pub(crate) level_triggered: bool,
    /// Whether this event originated from the legacy PIC delivery path.
    pub(crate) legacy_pic: bool,
}

pub(crate) fn queue_pending_event(queue: &mut VecDeque<PendingEvent>, event: PendingEvent) {
    if event.vector >= 32
        && queue
            .iter()
            .any(|pending| pending.vector == event.vector && pending.legacy_pic == event.legacy_pic)
    {
        // Coalesce repeated events from the same delivery source. A fixed
        // interrupt and a legacy PIC interrupt may share a vector while still
        // representing independent pending sources.
        return;
    }
    queue.push_back(event);
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::{PendingEvent, queue_pending_event};

    fn external_event(vector: u8) -> PendingEvent {
        PendingEvent {
            vector,
            err_code: None,
            level_triggered: false,
            legacy_pic: false,
        }
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
        let exception = PendingEvent {
            vector: 14,
            err_code: Some(1),
            level_triggered: false,
            legacy_pic: false,
        };

        queue_pending_event(&mut queue, exception);
        queue_pending_event(&mut queue, exception);

        assert_eq!(queue.len(), 2);
    }

    #[test]
    fn fixed_and_legacy_sources_keep_independent_pending_owners() {
        let mut queue = VecDeque::new();
        queue_pending_event(&mut queue, external_event(0x68));
        queue_pending_event(
            &mut queue,
            PendingEvent {
                vector: 0x68,
                err_code: None,
                level_triggered: false,
                legacy_pic: true,
            },
        );

        assert_eq!(queue.len(), 2);
        assert!(!queue[0].legacy_pic);
        assert!(queue[1].legacy_pic);
    }
}
