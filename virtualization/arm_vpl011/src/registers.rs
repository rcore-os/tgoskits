//! PL011 register offsets and architectural bit definitions.

pub(crate) const MMIO_SIZE: u64 = 0x1000;
pub(crate) const FIFO_CAPACITY: usize = 16;

pub(crate) const UARTDR: u64 = 0x000;
pub(crate) const UARTRSR_ECR: u64 = 0x004;
pub(crate) const UARTFR: u64 = 0x018;
pub(crate) const UARTILPR: u64 = 0x020;
pub(crate) const UARTIBRD: u64 = 0x024;
pub(crate) const UARTFBRD: u64 = 0x028;
pub(crate) const UARTLCR_H: u64 = 0x02c;
pub(crate) const UARTCR: u64 = 0x030;
pub(crate) const UARTIFLS: u64 = 0x034;
pub(crate) const UARTIMSC: u64 = 0x038;
pub(crate) const UARTRIS: u64 = 0x03c;
pub(crate) const UARTMIS: u64 = 0x040;
pub(crate) const UARTICR: u64 = 0x044;
pub(crate) const UARTDMACR: u64 = 0x048;

pub(crate) const UARTFR_BUSY: u32 = 1 << 3;
pub(crate) const UARTFR_RXFE: u32 = 1 << 4;
pub(crate) const UARTFR_TXFF: u32 = 1 << 5;
pub(crate) const UARTFR_RXFF: u32 = 1 << 6;
pub(crate) const UARTFR_TXFE: u32 = 1 << 7;

pub(crate) const UARTLCR_H_FEN: u32 = 1 << 4;
pub(crate) const UARTCR_UARTEN: u32 = 1;
pub(crate) const UARTCR_TXE: u32 = 1 << 8;
pub(crate) const UARTCR_RXE: u32 = 1 << 9;

pub(crate) const UARTINT_RX: u32 = 1 << 4;
pub(crate) const UARTINT_TX: u32 = 1 << 5;
pub(crate) const UARTINT_RT: u32 = 1 << 6;
pub(crate) const UARTINT_FE: u32 = 1 << 7;
pub(crate) const UARTINT_PE: u32 = 1 << 8;
pub(crate) const UARTINT_BE: u32 = 1 << 9;
pub(crate) const UARTINT_OE: u32 = 1 << 10;
pub(crate) const UARTINT_ALL: u32 = 0x7ff;

pub(crate) const UART_PID0: u64 = 0xfe0;
pub(crate) const UART_PID1: u64 = 0xfe4;
pub(crate) const UART_PID2: u64 = 0xfe8;
pub(crate) const UART_PID3: u64 = 0xfec;
pub(crate) const UART_CID0: u64 = 0xff0;
pub(crate) const UART_CID1: u64 = 0xff4;
pub(crate) const UART_CID2: u64 = 0xff8;
pub(crate) const UART_CID3: u64 = 0xffc;
