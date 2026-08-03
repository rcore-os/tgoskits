use super::*;

#[derive(Debug)]
pub(crate) struct OwnerDispatchState {
    pub(crate) current: Option<ThreadId>,
    pub(crate) current_core: Option<Arc<ThreadCore>>,
    pub(crate) current_dispatch: Option<CurrentDispatch>,
    pub(crate) idle: Option<ThreadId>,
    pub(crate) idle_core: Option<Arc<ThreadCore>>,
    pub(crate) rt_bandwidth: RtBandwidth,
    pub(crate) fair_balance_interval_ns: u64,
    pub(crate) switch_handoff: Option<SwitchHandoff>,
}

impl OwnerDispatchState {
    pub(crate) fn new(config: TaskSystemConfig) -> Self {
        Self {
            current: None,
            current_core: None,
            current_dispatch: None,
            idle: None,
            idle_core: None,
            rt_bandwidth: RtBandwidth::new(config.rt_period_ns(), config.rt_runtime_ns()),
            fair_balance_interval_ns: config.balance_interval_ns().max(1),
            switch_handoff: None,
        }
    }
}
