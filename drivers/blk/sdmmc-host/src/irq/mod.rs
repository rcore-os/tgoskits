/// Move-only ownership split produced once for one physical host.
///
/// `bus` remains in task/owner context, `irq` moves into hard-IRQ
/// registration, and `card_irq` controls a nested SDIO `CARD_INT` source when
/// the controller provides one. The concrete endpoint capability traits live
/// with the protocol/runtime that consumes those endpoints.
/// Native memory-card consumers may drop `card_irq` only because their card
/// protocol never enables an SDIO Function; IO-card consumers must transfer it
/// to the same owner domain as `bus`.
pub struct HostParts<B, I, C> {
    pub bus: B,
    pub irq: I,
    pub card_irq: Option<C>,
}
