use tock_registers::interfaces::{Readable, Writeable};

use crate::backend::{
    kmod::dwc2::{
        channel::Dwc2ChannelCompletions,
        reg::{
            DWC2_MAX_CHANNELS, DWC2_RUNTIME_GINTMSK, Dwc2Registers, GINTSTS_DISCONNINT,
            GINTSTS_HCHINT, GINTSTS_PRTINT, HCINT_CHHLTD, HPRT_CONN_DET, HPRT_ENA_CHG,
            HPRT_OVRCUR_CHG, HPRT_W1C_MASK,
        },
        stats::Dwc2Stats,
    },
    ty::{Event, EventHandlerOp},
};

pub(crate) struct Dwc2EventHandler {
    regs: Dwc2Registers,
    channel_completions: Dwc2ChannelCompletions,
    stats: Dwc2Stats,
}

unsafe impl Send for Dwc2EventHandler {}
unsafe impl Sync for Dwc2EventHandler {}

impl Dwc2EventHandler {
    pub(crate) fn new(
        regs: Dwc2Registers,
        channel_completions: Dwc2ChannelCompletions,
        stats: Dwc2Stats,
    ) -> Self {
        Self {
            regs,
            channel_completions,
            stats,
        }
    }
}

impl EventHandlerOp for Dwc2EventHandler {
    fn handle_event(&self) -> Event {
        let pending =
            self.regs.regs().gintsts.get() & self.regs.regs().gintmsk.get() & DWC2_RUNTIME_GINTMSK;
        if pending == 0 {
            return Event::Nothing;
        }

        if pending & GINTSTS_DISCONNINT != 0 {
            self.channel_completions.disconnect_all_with(|| {
                self.regs.regs().haintmsk.set(0);
                let mask = self.regs.regs().gintmsk.get();
                self.regs.regs().gintmsk.set(mask & !GINTSTS_HCHINT);
            });
            self.regs.regs().gintsts.set(GINTSTS_DISCONNINT);
            return Event::Stopped;
        }

        if pending & GINTSTS_PRTINT != 0 {
            let hprt = self.regs.hprt().raw();
            let changes = hprt & (HPRT_CONN_DET | HPRT_ENA_CHG | HPRT_OVRCUR_CHG);
            if changes != 0 {
                // PRTINT is a read-only summary. Linux clears its source by
                // acknowledging the HPRT0 W1C change bits while writing zero
                // to HPRT0.ENA so the acknowledgement cannot disable the port.
                self.regs.hprt().write((hprt & !HPRT_W1C_MASK) | changes);
            }
            return Event::PortChange { port: 1 };
        }
        if pending & GINTSTS_HCHINT != 0 {
            self.stats.record_irq_event();
            let count = self.handle_channel_interrupts();
            self.regs.regs().gintsts.set(GINTSTS_HCHINT);
            return Event::TransferActivity {
                count: count.max(1),
            };
        }
        self.regs.regs().gintsts.set(pending);
        Event::Stopped
    }
}

impl Dwc2EventHandler {
    fn handle_channel_interrupts(&self) -> usize {
        let pending = self.regs.regs().haint.get() & self.regs.regs().haintmsk.get();
        let mut count = 0usize;
        for channel in 0..DWC2_MAX_CHANNELS {
            if pending & (1u32 << channel) == 0 {
                continue;
            }
            let channel_regs = self.regs.channel(channel);
            let Some(hcint) = channel_regs.take_irqs() else {
                continue;
            };
            if hcint & HCINT_CHHLTD == 0 && channel_regs.is_enabled() {
                if self.channel_completions.is_iso(channel) {
                    // ISO 常驻通道：XFERCOMPL（IOC）时保持通道使能并继续周期
                    // 会话，直接发布完成位，由任务侧结算本请求。
                    self.channel_completions.publish(channel, hcint);
                    self.stats.record_channel_completion();
                    count += 1;
                } else {
                    self.channel_completions.defer(channel, hcint);
                    channel_regs.disable();
                }
                continue;
            }
            self.channel_completions.publish(channel, hcint);
            self.stats.record_channel_completion();
            count += 1;
        }
        count
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::vec::Vec;

    use super::*;
    use crate::backend::kmod::dwc2::{
        reg::{
            DWC2_COMPLETION_DISCONNECTED, HCINT_CHHLTD, HCINT_STALL, HCINT_XFERCOMPL,
            HPRT_CONN_DET, HPRT_ENA, HPRT_ENA_CHG, HPRT_OVRCUR_CHG, HPRT_PWR, HPRT_W1C_MASK,
        },
        testutil as tu,
    };

    fn handler_fixture() -> (
        Vec<u32>,
        Dwc2Registers,
        Dwc2ChannelCompletions,
        Dwc2Stats,
        Dwc2EventHandler,
    ) {
        let (backing, regs) = tu::test_regs();
        let completions = Dwc2ChannelCompletions::new();
        let stats = Dwc2Stats::new();
        let handler = Dwc2EventHandler::new(regs, completions.clone(), stats.clone());
        (backing, regs, completions, stats, handler)
    }

    /// 预置一道路 channel 中断（GINTMSK/GINTSTS/HAINTMSK/HAINT 全开通道 0）。
    fn arm_channel_interrupt(regs: &Dwc2Registers) {
        regs.regs().gintmsk.set(GINTSTS_HCHINT);
        regs.regs().gintsts.set(GINTSTS_HCHINT);
        regs.regs().haintmsk.set(1);
        regs.regs().haint.set(1);
    }

    #[test]
    fn hchint_event_publishes_channel_completion() {
        let (_backing, regs, completions, stats, handler) = handler_fixture();

        arm_channel_interrupt(&regs);
        regs.regs().hc[0].hcintmsk.set(HCINT_CHHLTD);
        regs.regs().hc[0].hcint.set(HCINT_CHHLTD | HCINT_XFERCOMPL);

        match handler.handle_event() {
            Event::TransferActivity { count } => assert_eq!(count, 1),
            event => panic!("expected transfer activity, got {event:?}"),
        }
        assert_eq!(completions.take(0), Some(HCINT_CHHLTD | HCINT_XFERCOMPL));

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.irq_events, 1);
        assert_eq!(snapshot.channel_completions, 1);
    }

    #[test]
    fn channel_interrupt_requires_gintmsk_ack() {
        let (_backing, regs, completions, _stats, handler) = handler_fixture();

        // 状态位置位但 GINTMSK 未使能：不消费通道中断寄存器。
        regs.regs().gintsts.set(GINTSTS_HCHINT);
        regs.regs().gintmsk.set(0);
        regs.regs().haintmsk.set(1);
        regs.regs().haint.set(1);
        regs.regs().hc[0].hcintmsk.set(HCINT_CHHLTD);
        regs.regs().hc[0].hcint.set(HCINT_CHHLTD | HCINT_XFERCOMPL);

        assert!(matches!(handler.handle_event(), Event::Nothing));
        assert_eq!(completions.take(0), None);
        assert_eq!(
            regs.regs().hc[0].hcint.get(),
            HCINT_CHHLTD | HCINT_XFERCOMPL
        );
    }

    #[test]
    fn port_interrupt_acknowledges_hprt_change_bits() {
        let (_backing, regs, completions, _stats, handler) = handler_fixture();
        let hprt = (1 << 0) | HPRT_ENA | HPRT_CONN_DET | HPRT_ENA_CHG | HPRT_PWR | HPRT_OVRCUR_CHG;

        regs.regs().gintmsk.set(GINTSTS_PRTINT);
        regs.regs().gintsts.set(GINTSTS_PRTINT);
        regs.regs().hprt.set(hprt);

        assert!(matches!(
            handler.handle_event(),
            Event::PortChange { port: 1 }
        ));
        assert_eq!(
            regs.regs().hprt.get(),
            (hprt & !HPRT_W1C_MASK) | HPRT_CONN_DET | HPRT_ENA_CHG | HPRT_OVRCUR_CHG
        );
        assert_eq!(completions.take(0), None);
    }

    #[test]
    fn chhltd_completion_preserves_unmasked_raw_hcint_reason() {
        let (_backing, regs, completions, _stats, handler) = handler_fixture();

        arm_channel_interrupt(&regs);
        regs.regs().hc[0].hcintmsk.set(HCINT_CHHLTD);
        regs.regs().hc[0].hcint.set(HCINT_CHHLTD | HCINT_STALL);

        match handler.handle_event() {
            Event::TransferActivity { count } => assert_eq!(count, 1),
            event => panic!("expected transfer activity, got {event:?}"),
        }
        // 只开 CHHLTD 屏蔽位，故障位仍随原始 hcint 保留。
        assert_eq!(completions.take(0), Some(HCINT_CHHLTD | HCINT_STALL));
    }

    #[test]
    fn xfercomplete_without_channel_halt_is_deferred_until_halt_for_non_iso() {
        let (_backing, regs, completions, stats, handler) = handler_fixture();

        arm_channel_interrupt(&regs);
        regs.regs().hc[0]
            .hcintmsk
            .set(HCINT_CHHLTD | HCINT_XFERCOMPL);
        regs.regs().hc[0].hcchar.set(1 << 31); // CHENA
        regs.regs().hc[0].hcint.set(HCINT_XFERCOMPL);

        match handler.handle_event() {
            Event::TransferActivity { .. } => {}
            event => panic!("expected transfer activity, got {event:?}"),
        }
        // 非 ISO：XFERCOMPL 未伴随 CHHLTD，先请求 halt 后再发布。
        assert_eq!(completions.take(0), None);
        assert_eq!(
            regs.regs().hc[0].hcchar.get() & ((1 << 30) | (1 << 31)),
            1 << 30
        );
        assert_eq!(stats.snapshot().channel_completions, 0);

        // CHHLTD 到达：发布合并 deferred 位的完整完成。
        regs.regs().gintsts.set(GINTSTS_HCHINT);
        regs.regs().hc[0].hcchar.set(0);
        regs.regs().hc[0].hcint.set(HCINT_CHHLTD);
        match handler.handle_event() {
            Event::TransferActivity { count } => assert_eq!(count, 1),
            event => panic!("expected transfer activity, got {event:?}"),
        }
        assert_eq!(completions.take(0), Some(HCINT_CHHLTD | HCINT_XFERCOMPL));
        assert_eq!(stats.snapshot().channel_completions, 1);
    }

    #[test]
    fn iso_completion_publishes_without_halt_or_disable() {
        let (_backing, regs, completions, stats, handler) = handler_fixture();

        arm_channel_interrupt(&regs);
        completions.mark_iso(0, true);
        regs.regs().hc[0]
            .hcintmsk
            .set(HCINT_CHHLTD | HCINT_XFERCOMPL);
        regs.regs().hc[0].hcchar.set(1 << 31); // ISO 常驻通道保持 CHENA
        regs.regs().hc[0].hcint.set(HCINT_XFERCOMPL);

        match handler.handle_event() {
            Event::TransferActivity { count } => assert_eq!(count, 1),
            event => panic!("expected transfer activity, got {event:?}"),
        }
        assert_eq!(completions.take(0), Some(HCINT_XFERCOMPL));
        assert_eq!(regs.regs().hc[0].hcchar.get() & (1 << 31), 1 << 31);
        assert_eq!(stats.snapshot().channel_completions, 1);
    }

    #[test]
    fn disconnect_event_stops_and_publishes_disconnected_completion() {
        let (_backing, regs, completions, _stats, handler) = handler_fixture();
        let channel = completions.try_begin_request(3);
        assert!(channel);

        regs.regs().gintmsk.set(GINTSTS_DISCONNINT);
        regs.regs().gintsts.set(GINTSTS_DISCONNINT);

        assert!(matches!(handler.handle_event(), Event::Stopped));
        assert_eq!(completions.take(3), Some(DWC2_COMPLETION_DISCONNECTED));
        assert_eq!(regs.regs().haintmsk.get(), 0);
        assert_eq!(regs.regs().gintmsk.get() & GINTSTS_HCHINT, 0);
    }
}
