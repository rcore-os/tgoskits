use core::ptr::NonNull;

use rdif_serial::{
    Config, ConfigError, DataBits, IRQ_RX_BATCH_CAPACITY, IrqRxBatch, Parity, RxErrorFlags, RxFlag,
    RxSample, SerialEventSet, SerialIrqEvent, SerialIrqReport, SerialParts, SplitUart, StopBits,
    UartEmergencyTx, UartInfo, UartIrq, UartPort,
};
use tock_registers::{
    LocalRegisterCopy, interfaces::*, register_bitfields, register_structs, registers::*,
};

use crate::{PollingUart, SerialDirection, SerialEvent, TransBytesError, TransferError};

const BUSY_POLL_BUDGET: usize = 1 << 20;
const EMERGENCY_TX_BUDGET: usize = 16;
const ALL_IRQ_BITS: u32 = (1 << 11) - 1;

mod control;
mod emergency;
mod event;
mod irq;
mod registers;
mod runtime;
mod rx;

pub use control::Pl011;
pub use emergency::Pl011EmergencyTx;
use event::*;
pub use irq::Pl011Irq;
pub use registers::Pl011Registers;
use registers::*;
use rx::*;

#[cfg(test)]
mod tests;
