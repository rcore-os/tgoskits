mod futex;
mod membarrier;
mod rseq;

pub use self::{futex::*, membarrier::*, rseq::*};

#[cfg(axtest)]
pub(crate) fn membarrier_validation_rules_hold_for_test() -> bool {
    membarrier::membarrier_query_and_global_rules_hold_for_test()
}

#[cfg(axtest)]
pub(crate) fn futex_op_and_compare_rules_hold_for_test() -> bool {
    futex::futex_op_and_compare_rules_hold_for_test()
}

#[cfg(axtest)]
pub(crate) use self::rseq::rseq_validation_rules_hold_for_test;
