//! Local interrupt operations.

pub use super::asm::{
    disable_irqs, enable_irqs, halt, irqs_enabled, wait_for_irqs, wait_for_irqs_disabled,
};
