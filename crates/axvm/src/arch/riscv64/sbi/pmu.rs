use axerrno::AxResult;

#[derive(Clone, Copy, Debug)]
pub enum PmuFunction {
    /// Returns the total of performance counters (hardware and fireware).
    GetNumCounters,
    /// Returns information about hardware counter specified by the inner value.
    GetCounterInfo(u64),
    /// Stops the couters selected by counter_index and counter_mask.
    /// See the sbi_pmu_counter_stop documentation for details.
    StopCounter {
        /// Countert index base.
        counter_index: u64,
        /// Counter index mask.
        counter_mask: u64,
        /// Counter stop flags.
        stop_flags: u64,
    },
}

impl PmuFunction {
    pub(crate) fn from_regs(args: &[usize]) -> AxResult<Self> {
        match args[6] {
            0 => Ok(Self::GetNumCounters),
            1 => Ok(Self::GetCounterInfo(args[0] as u64)),
            4 => Ok(Self::StopCounter {
                counter_index: args[0] as u64,
                counter_mask: args[1] as u64,
                stop_flags: args[2] as u64,
            }),
            _ => panic!("Unsupported yet!"),
        }
    }
}
