use alloc::{boxed::Box, collections::BTreeMap, sync::Arc};

use crate::{
    irq::{
        InterruptControllerOps, IrqMessage, IrqOutcome, IrqRoutingTable, IrqSink, KickTarget,
        TriggerMode, VcpuKicker,
    },
    r#trait::*,
};

/// Lock-free interrupt dispatch runtime.
///
/// Constructed once at VM freeze time. After construction, all fields are
/// immutable — no locks needed on the inject hot path.
///
/// ```text
/// IrqRoutingTable lookup (BTreeMap, O(log n))
///   → pre-resolved Arc<dyn InterruptControllerOps> (direct, no indirection)
///     → .inject_irq() → IrqOutcome
///       → kicker.kick(target)
/// ```
pub struct IrqRuntime {
    routing: IrqRoutingTable,
    default_intc: Option<Arc<dyn InterruptControllerOps>>,
    resolved_controllers: BTreeMap<u64, Arc<dyn InterruptControllerOps>>,
    kicker: Box<dyn VcpuKicker>,
}

impl IrqRuntime {
    pub fn new(
        routing: IrqRoutingTable,
        default_intc: Option<Arc<dyn InterruptControllerOps>>,
        resolved_controllers: BTreeMap<u64, Arc<dyn InterruptControllerOps>>,
        kicker: Box<dyn VcpuKicker>,
    ) -> Self {
        Self {
            routing,
            default_intc,
            resolved_controllers,
            kicker,
        }
    }

    pub fn inject(&self, msg: IrqMessage) -> Result<()> {
        let outcome = match &msg {
            IrqMessage::Legacy { line } => {
                if let Some((_ctrl_id, entry)) = self.routing.lookup_legacy(*line) {
                    let ctrl = self
                        .resolved_controllers
                        .get(&entry.controller.0)
                        .ok_or(DeviceError::NotFound)?;
                    ctrl.inject_irq(entry.controller_pin, entry.trigger, entry.target)?
                } else if let Some(ref intc) = self.default_intc {
                    intc.inject_irq(line.0, TriggerMode::Edge, None)?
                } else {
                    return Err(DeviceError::NotFound);
                }
            }
            IrqMessage::Msi { addr, data } => {
                let ctrl_id = self
                    .routing
                    .lookup_msi(*addr)
                    .ok_or(DeviceError::NotFound)?;
                let ctrl = self
                    .resolved_controllers
                    .get(&ctrl_id.0)
                    .ok_or(DeviceError::NotFound)?;
                ctrl.handle_msi(*addr, *data)?
            }
        };

        self.apply_outcome(outcome);
        Ok(())
    }

    pub fn deactivate(&self, line: IrqLine) -> Result<()> {
        let outcome = if let Some((_ctrl_id, entry)) = self.routing.lookup_legacy(line) {
            let ctrl = self
                .resolved_controllers
                .get(&entry.controller.0)
                .ok_or(DeviceError::NotFound)?;
            ctrl.deactivate_irq(entry.controller_pin)?
        } else if let Some(ref intc) = self.default_intc {
            intc.deactivate_irq(line.0)?
        } else {
            return Err(DeviceError::NotFound);
        };

        self.apply_outcome(outcome);
        Ok(())
    }

    fn apply_outcome(&self, outcome: IrqOutcome) {
        match outcome {
            IrqOutcome::Kick(KickTarget::One(id)) => self.kicker.kick(id),
            IrqOutcome::Kick(KickTarget::Set(set)) => {
                for id in set.iter() {
                    self.kicker.kick(id);
                }
            }
            IrqOutcome::Kick(KickTarget::All) => self.kicker.kick_all(),
            IrqOutcome::Delivered | IrqOutcome::Queued => {}
        }
    }

    /// Inject an IPI (inter-processor interrupt) to a specific vCPU.
    ///
    /// This is a convenience wrapper around the default interrupt controller:
    /// it calls `inject_irq(vector, Edge, Cpu(target))` on `self.default_intc`,
    /// then applies the resulting `IrqOutcome` (vCPU kick).
    ///
    /// Returns an error if no default interrupt controller is configured.
    pub fn inject_ipi(&self, target_vcpu: usize, vector: u8) -> Result<()> {
        let intc = self.default_intc.as_ref().ok_or(DeviceError::NotFound)?;
        let outcome = intc.inject_irq(
            vector as u32,
            TriggerMode::Edge,
            Some(IrqTarget::Cpu(target_vcpu)),
        )?;
        self.apply_outcome(outcome);
        Ok(())
    }

    pub fn kicker(&self) -> &dyn VcpuKicker {
        self.kicker.as_ref()
    }

    /// Create an [`IrqSink`] backed by this runtime.
    ///
    /// Unlike [`BusRouter::create_irq_sink`], the sink produced here routes
    /// through [`IrqRuntime::inject`], which properly dispatches [`IrqOutcome`]
    /// kick targets to the [`VcpuKicker`].
    pub fn create_irq_sink(self: &Arc<Self>, line: IrqLine, trigger: TriggerMode) -> IrqSink {
        let rt_inject = Arc::clone(self);
        let rt_deact = Arc::clone(self);
        IrqSink::new(
            line,
            trigger,
            Arc::new(move |msg| rt_inject.inject(msg)),
            Arc::new(move |line| rt_deact.deactivate(line)),
        )
    }
}

#[cfg(test)]
mod tests {
    use core::{
        any::Any,
        sync::atomic::{AtomicU32, Ordering},
    };

    use super::*;

    struct NoopKicker(usize);
    impl VcpuKicker for NoopKicker {
        fn kick(&self, _vcpu_id: usize) {}
        fn vcpu_count(&self) -> usize {
            self.0
        }
    }

    struct CountingKicker {
        kicks: alloc::vec::Vec<AtomicU32>,
    }
    impl CountingKicker {
        fn new(n: usize) -> Self {
            let mut kicks = alloc::vec::Vec::with_capacity(n);
            for _ in 0..n {
                kicks.push(AtomicU32::new(0));
            }
            Self { kicks }
        }
    }
    impl VcpuKicker for CountingKicker {
        fn kick(&self, vcpu_id: usize) {
            self.kicks[vcpu_id].fetch_add(1, Ordering::Relaxed);
        }
        fn vcpu_count(&self) -> usize {
            self.kicks.len()
        }
    }

    #[derive(Debug)]
    struct MockCtrl {
        inject_count: AtomicU32,
        outcome: IrqOutcome,
    }
    impl MockCtrl {
        fn new(outcome: IrqOutcome) -> Self {
            Self {
                inject_count: AtomicU32::new(0),
                outcome,
            }
        }
    }
    impl crate::irq::InterruptControllerOps for MockCtrl {
        fn inject_irq(
            &self,
            _pin: u32,
            _trigger: TriggerMode,
            _target: Option<IrqTarget>,
        ) -> Result<IrqOutcome> {
            self.inject_count.fetch_add(1, Ordering::Relaxed);
            Ok(self.outcome.clone())
        }
        fn deactivate_irq(&self, _pin: u32) -> Result<IrqOutcome> {
            Ok(IrqOutcome::Delivered)
        }
    }

    #[test]
    fn inject_via_default_intc() {
        let ctrl = Arc::new(MockCtrl::new(IrqOutcome::Delivered));
        let rt = IrqRuntime::new(
            IrqRoutingTable::new(),
            Some(ctrl.clone()),
            BTreeMap::new(),
            Box::new(NoopKicker(4)),
        );

        rt.inject(IrqMessage::leg(IrqLine(5))).unwrap();
        assert_eq!(ctrl.inject_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn inject_via_routing_table() {
        let ctrl = Arc::new(MockCtrl::new(IrqOutcome::Delivered));
        let ctrl_id = DeviceId(42);

        let mut routing = IrqRoutingTable::new();
        routing.add_legacy(IrqLine(10), ctrl_id, 3, TriggerMode::Edge, None, "dev0");

        let mut controllers = BTreeMap::new();
        controllers.insert(
            ctrl_id.0,
            ctrl.clone() as Arc<dyn crate::irq::InterruptControllerOps>,
        );

        let rt = IrqRuntime::new(routing, None, controllers, Box::new(NoopKicker(4)));

        rt.inject(IrqMessage::leg(IrqLine(10))).unwrap();
        assert_eq!(ctrl.inject_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn inject_no_route_no_default_returns_error() {
        let rt = IrqRuntime::new(
            IrqRoutingTable::new(),
            None,
            BTreeMap::new(),
            Box::new(NoopKicker(1)),
        );

        let result = rt.inject(IrqMessage::leg(IrqLine(1)));
        assert!(matches!(result, Err(DeviceError::NotFound)));
    }

    #[test]
    fn explicit_route_takes_priority_over_default() {
        let default_ctrl = Arc::new(MockCtrl::new(IrqOutcome::Delivered));
        let explicit_ctrl = Arc::new(MockCtrl::new(IrqOutcome::Delivered));
        let explicit_id = DeviceId(99);

        let mut routing = IrqRoutingTable::new();
        routing.add_legacy(
            IrqLine(5),
            explicit_id,
            7,
            TriggerMode::Level { high: true },
            None,
            "explicit",
        );

        let mut controllers = BTreeMap::new();
        controllers.insert(
            explicit_id.0,
            explicit_ctrl.clone() as Arc<dyn crate::irq::InterruptControllerOps>,
        );

        let rt = IrqRuntime::new(
            routing,
            Some(default_ctrl.clone()),
            controllers,
            Box::new(NoopKicker(4)),
        );

        rt.inject(IrqMessage::leg(IrqLine(5))).unwrap();

        assert_eq!(explicit_ctrl.inject_count.load(Ordering::Relaxed), 1);
        assert_eq!(default_ctrl.inject_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn kick_outcome_dispatched_to_kicker() {
        let ctrl = Arc::new(MockCtrl::new(IrqOutcome::Kick(KickTarget::One(2))));
        let kicker = alloc::sync::Arc::new(CountingKicker::new(4));

        let rt = IrqRuntime::new(
            IrqRoutingTable::new(),
            Some(ctrl),
            BTreeMap::new(),
            Box::new(KickerRef(kicker.clone())),
        );

        rt.inject(IrqMessage::leg(IrqLine(1))).unwrap();

        assert_eq!(kicker.kicks[0].load(Ordering::Relaxed), 0);
        assert_eq!(kicker.kicks[1].load(Ordering::Relaxed), 0);
        assert_eq!(kicker.kicks[2].load(Ordering::Relaxed), 1);
        assert_eq!(kicker.kicks[3].load(Ordering::Relaxed), 0);
    }

    struct KickerRef(alloc::sync::Arc<CountingKicker>);
    impl VcpuKicker for KickerRef {
        fn kick(&self, vcpu_id: usize) {
            self.0.kick(vcpu_id);
        }
        fn vcpu_count(&self) -> usize {
            self.0.vcpu_count()
        }
    }

    #[test]
    fn delivered_outcome_no_kick() {
        let ctrl = Arc::new(MockCtrl::new(IrqOutcome::Delivered));
        let kicker = alloc::sync::Arc::new(CountingKicker::new(4));

        let rt = IrqRuntime::new(
            IrqRoutingTable::new(),
            Some(ctrl),
            BTreeMap::new(),
            Box::new(KickerRef(kicker.clone())),
        );

        rt.inject(IrqMessage::leg(IrqLine(1))).unwrap();

        for k in &kicker.kicks {
            assert_eq!(k.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn deactivate_with_default_intc_fallback() {
        let ctrl = Arc::new(MockCtrl::new(IrqOutcome::Delivered));
        let rt = IrqRuntime::new(
            IrqRoutingTable::new(),
            Some(ctrl),
            BTreeMap::new(),
            Box::new(NoopKicker(1)),
        );

        // default_intc fallback works for deactivate (unlike old BusRouter)
        assert!(rt.deactivate(IrqLine(42)).is_ok());
    }

    #[test]
    fn deactivate_no_route_no_default_returns_error() {
        let rt = IrqRuntime::new(
            IrqRoutingTable::new(),
            None,
            BTreeMap::new(),
            Box::new(NoopKicker(1)),
        );

        assert!(matches!(
            rt.deactivate(IrqLine(1)),
            Err(DeviceError::NotFound)
        ));
    }

    #[test]
    fn rapid_inject_all_reach_controller() {
        let ctrl = Arc::new(MockCtrl::new(IrqOutcome::Delivered));
        let rt = IrqRuntime::new(
            IrqRoutingTable::new(),
            Some(ctrl.clone()),
            BTreeMap::new(),
            Box::new(NoopKicker(1)),
        );

        for i in 0..100u32 {
            rt.inject(IrqMessage::leg(IrqLine(i))).unwrap();
        }

        assert_eq!(ctrl.inject_count.load(Ordering::Relaxed), 100);
    }

    #[test]
    fn irq_sink_via_runtime_delivers_kick() {
        let ctrl = Arc::new(MockCtrl::new(IrqOutcome::Kick(KickTarget::One(2))));
        let kicker = alloc::sync::Arc::new(CountingKicker::new(4));

        let rt = Arc::new(IrqRuntime::new(
            IrqRoutingTable::new(),
            Some(ctrl.clone()),
            BTreeMap::new(),
            Box::new(KickerRef(kicker.clone())),
        ));

        let sink = rt.create_irq_sink(IrqLine(5), TriggerMode::Edge);
        assert_eq!(sink.line(), IrqLine(5));

        sink.raise().unwrap();

        assert_eq!(ctrl.inject_count.load(Ordering::Relaxed), 1);
        assert_eq!(kicker.kicks[2].load(Ordering::Relaxed), 1);
        assert_eq!(kicker.kicks[0].load(Ordering::Relaxed), 0);
    }

    #[test]
    fn irq_sink_lower_routes_through_deactivate() {
        let ctrl = Arc::new(MockCtrl::new(IrqOutcome::Delivered));

        let rt = Arc::new(IrqRuntime::new(
            IrqRoutingTable::new(),
            Some(ctrl),
            BTreeMap::new(),
            Box::new(NoopKicker(1)),
        ));

        let sink = rt.create_irq_sink(IrqLine(10), TriggerMode::Level { high: true });
        assert!(sink.lower().is_ok());
    }

    #[test]
    fn populated_routing_table_explicit_and_fallback() {
        let explicit_ctrl = Arc::new(MockCtrl::new(IrqOutcome::Delivered));
        let default_ctrl = Arc::new(MockCtrl::new(IrqOutcome::Delivered));
        let explicit_id = DeviceId(77);

        let mut routing = IrqRoutingTable::new();
        routing.add_legacy(
            IrqLine(33),
            explicit_id,
            33,
            TriggerMode::Edge,
            None,
            "uart",
        );

        let mut controllers = BTreeMap::new();
        controllers.insert(
            explicit_id.0,
            explicit_ctrl.clone() as Arc<dyn crate::irq::InterruptControllerOps>,
        );

        let rt = IrqRuntime::new(
            routing,
            Some(default_ctrl.clone()),
            controllers,
            Box::new(NoopKicker(4)),
        );

        // Line 33 has explicit route → explicit_ctrl
        rt.inject(IrqMessage::leg(IrqLine(33))).unwrap();
        assert_eq!(explicit_ctrl.inject_count.load(Ordering::Relaxed), 1);
        assert_eq!(default_ctrl.inject_count.load(Ordering::Relaxed), 0);

        // Line 99 has no route → falls back to default_ctrl
        rt.inject(IrqMessage::leg(IrqLine(99))).unwrap();
        assert_eq!(explicit_ctrl.inject_count.load(Ordering::Relaxed), 1);
        assert_eq!(default_ctrl.inject_count.load(Ordering::Relaxed), 1);
    }
}
