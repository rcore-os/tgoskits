use alloc::collections::BTreeMap;

use rdif_vsock::VsockConnId;

#[derive(Default)]
pub(super) struct TxCreditBook {
    connections: BTreeMap<VsockConnId, TxCredit>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct TxCredit {
    peer_buffer_allocation: u32,
    peer_forward_count: u32,
    transmitted_count: u32,
}

impl TxCreditBook {
    pub(super) fn open(&mut self, connection: VsockConnId) {
        self.connections.entry(connection).or_default();
    }

    pub(super) fn close(&mut self, connection: VsockConnId) {
        self.connections.remove(&connection);
    }

    pub(super) fn update_peer(
        &mut self,
        connection: VsockConnId,
        buffer_allocation: u32,
        forward_count: u32,
    ) {
        let Some(credit) = self.connections.get_mut(&connection) else {
            return;
        };
        credit.peer_buffer_allocation = buffer_allocation;
        credit.peer_forward_count = forward_count;
    }

    pub(super) fn record_sent(&mut self, connection: VsockConnId, length: u32) -> bool {
        let Some(credit) = self.connections.get_mut(&connection) else {
            return false;
        };
        credit.transmitted_count = credit.transmitted_count.wrapping_add(length);
        true
    }

    pub(super) fn available(
        &self,
        connection: VsockConnId,
        local_buffer_allocation: u32,
    ) -> Option<usize> {
        let credit = self.connections.get(&connection)?;
        let window = credit.peer_buffer_allocation.min(local_buffer_allocation);
        let in_flight = credit
            .transmitted_count
            .wrapping_sub(credit.peer_forward_count);
        Some(window.saturating_sub(in_flight) as usize)
    }
}

#[cfg(test)]
mod tests {
    use rdif_vsock::VsockAddr;

    use super::*;

    const CONNECTION: VsockConnId = VsockConnId {
        peer_addr: VsockAddr { cid: 2, port: 3 },
        local_port: 4,
    };

    #[test]
    fn peer_credit_bounds_writable_capacity_and_recovers_after_forwarding() {
        let mut credits = TxCreditBook::default();
        credits.open(CONNECTION);
        credits.update_peer(CONNECTION, 8 * 1024, 0);

        assert_eq!(credits.available(CONNECTION, 32 * 1024), Some(8 * 1024));
        assert!(credits.record_sent(CONNECTION, 8 * 1024));
        assert_eq!(credits.available(CONNECTION, 32 * 1024), Some(0));

        credits.update_peer(CONNECTION, 8 * 1024, 4 * 1024);
        assert_eq!(credits.available(CONNECTION, 32 * 1024), Some(4 * 1024));
    }

    #[test]
    fn peer_buffer_shrink_cannot_underflow_into_false_writability() {
        let mut credits = TxCreditBook::default();
        credits.open(CONNECTION);
        credits.update_peer(CONNECTION, 8 * 1024, 0);
        assert!(credits.record_sent(CONNECTION, 6 * 1024));

        credits.update_peer(CONNECTION, 2 * 1024, 0);

        assert_eq!(credits.available(CONNECTION, 32 * 1024), Some(0));
    }

    #[test]
    fn protocol_counters_use_wrapping_in_flight_arithmetic() {
        let mut credits = TxCreditBook::default();
        credits.open(CONNECTION);
        credits.update_peer(CONNECTION, 64, u32::MAX - 3);
        assert!(credits.record_sent(CONNECTION, u32::MAX - 1));
        assert!(credits.record_sent(CONNECTION, 4));

        assert_eq!(credits.available(CONNECTION, 64), Some(58));
    }

    #[test]
    fn late_peer_credit_cannot_reopen_a_locally_closed_connection() {
        let mut credits = TxCreditBook::default();
        credits.open(CONNECTION);
        credits.update_peer(CONNECTION, 64, 0);
        credits.close(CONNECTION);

        credits.update_peer(CONNECTION, 64, 32);

        assert_eq!(
            credits.available(CONNECTION, 64),
            None,
            "credit updates may change only an explicitly opened connection"
        );
    }
}
