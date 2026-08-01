//! Minimal ARM PrimeCell PL011 console emulation.

use alloc::{boxed::Box, string::String};
use core::{any::Any, marker::PhantomData};

use ax_kspin::SpinNoIrq as Mutex;
use axdevice_base::{
    AccessWidth, BusAccess, BusKind, BusResponse, Device, DeviceError, DeviceResult, Resource,
};
use axvm_types::GuestPhysAddr;

const PL011_REGISTER_WINDOW_SIZE: usize = 0x1000;
// Keep normal terminal records in one host callback while still bounding output
// from a guest that never emits a newline.
const OUTPUT_CHUNK_CAPACITY: usize = 1024;

const UARTDR: usize = 0x000;
const UARTRSR_ECR: usize = 0x004;
const UARTFR: usize = 0x018;
const UARTILPR: usize = 0x020;
const UARTIBRD: usize = 0x024;
const UARTFBRD: usize = 0x028;
const UARTLCR_H: usize = 0x02c;
const UARTCR: usize = 0x030;
const UARTIFLS: usize = 0x034;
const UARTIMSC: usize = 0x038;
const UARTRIS: usize = 0x03c;
const UARTMIS: usize = 0x040;
const UARTICR: usize = 0x044;
const UARTDMACR: usize = 0x048;

const UARTPID0: usize = 0xfe0;
const UARTPID1: usize = 0xfe4;
const UARTPID2: usize = 0xfe8;
const UARTPID3: usize = 0xfec;
const UARTCID0: usize = 0xff0;
const UARTCID1: usize = 0xff4;
const UARTCID2: usize = 0xff8;
const UARTCID3: usize = 0xffc;

const UARTFR_RXFE: u32 = 1 << 4;
const UARTFR_TXFE: u32 = 1 << 7;
const UARTCR_RESET: u32 = (1 << 8) | (1 << 9);
const UARTIFLS_RESET: u32 = 0x12;
const UARTIBRD_MASK: u32 = 0xffff;
const UARTFBRD_MASK: u32 = 0x3f;
const UARTLCR_H_MASK: u32 = 0xff;
const UARTCR_MASK: u32 = 0xffff;
const UARTIFLS_MASK: u32 = 0x3f;
const UARTIMSC_MASK: u32 = 0x7ff;
const UARTDMACR_MASK: u32 = 0x7;

/// Host capability used to publish one complete guest-console output chunk.
pub trait Pl011ConsoleHostOps {
    /// Publishes bytes emitted by `console`.
    ///
    /// Chunks normally end in a newline. A chunk can instead end at the fixed
    /// capacity boundary so a guest cannot force unbounded buffering by never
    /// printing a newline.
    fn write_console_chunk(console: &str, bytes: &[u8]);
}

struct OutputChunk {
    bytes: [u8; OUTPUT_CHUNK_CAPACITY],
    len: usize,
}

impl OutputChunk {
    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

struct Pl011State {
    ilpr: u32,
    ibrd: u32,
    fbrd: u32,
    lcr_h: u32,
    cr: u32,
    ifls: u32,
    imsc: u32,
    dmacr: u32,
    output: [u8; OUTPUT_CHUNK_CAPACITY],
    output_len: usize,
}

impl Pl011State {
    const fn new() -> Self {
        Self {
            ilpr: 0,
            ibrd: 0,
            fbrd: 0,
            lcr_h: 0,
            cr: UARTCR_RESET,
            ifls: UARTIFLS_RESET,
            imsc: 0,
            dmacr: 0,
            output: [0; OUTPUT_CHUNK_CAPACITY],
            output_len: 0,
        }
    }

    fn push_output(&mut self, byte: u8) -> Option<OutputChunk> {
        self.output[self.output_len] = byte;
        self.output_len += 1;
        if byte != b'\n' && self.output_len != self.output.len() {
            return None;
        }

        let chunk = OutputChunk {
            bytes: self.output,
            len: self.output_len,
        };
        self.output_len = 0;
        Some(chunk)
    }
}

/// Output-only PL011 UART used to observe an AArch64 guest without passing a
/// physical UART through to it.
pub struct Pl011ConsoleDevice<H: Pl011ConsoleHostOps> {
    name: String,
    base: GuestPhysAddr,
    length: usize,
    resources: Box<[Resource]>,
    state: Mutex<Pl011State>,
    _host: PhantomData<fn() -> H>,
}

impl<H: Pl011ConsoleHostOps> Pl011ConsoleDevice<H> {
    /// Creates a PL011 console over `base..base + length`.
    pub fn new(name: String, base: GuestPhysAddr, length: usize) -> DeviceResult<Self> {
        if name.is_empty() {
            return Err(DeviceError::InvalidInput {
                operation: "create PL011 console",
                detail: "console name must not be empty".into(),
            });
        }
        if length < PL011_REGISTER_WINDOW_SIZE {
            return Err(DeviceError::InvalidInput {
                operation: "create PL011 console",
                detail: alloc::format!(
                    "MMIO length {length:#x} is smaller than the {PL011_REGISTER_WINDOW_SIZE:#x} \
                     PL011 register window"
                ),
            });
        }
        base.as_usize()
            .checked_add(length)
            .ok_or_else(|| DeviceError::InvalidInput {
                operation: "create PL011 console",
                detail: alloc::format!(
                    "MMIO range {:#x}+{length:#x} overflows the guest address space",
                    base.as_usize()
                ),
            })?;

        let resources = alloc::vec![Resource::MmioRange {
            base: base.as_usize() as u64,
            size: length as u64,
        }]
        .into_boxed_slice();
        Ok(Self {
            name,
            base,
            length,
            resources,
            state: Mutex::new(Pl011State::new()),
            _host: PhantomData,
        })
    }

    fn offset(&self, access: &BusAccess) -> DeviceResult<usize> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        if access.width == AccessWidth::Qword {
            return Err(DeviceError::InvalidWidth {
                expected: AccessWidth::Dword,
                actual: access.width,
            });
        }
        let addr = usize::try_from(access.addr)
            .map_err(|_| DeviceError::OutOfRange { addr: access.addr })?;
        let base = self.base.as_usize();
        let end = base
            .checked_add(self.length)
            .expect("PL011 range was validated during construction");
        let access_end = addr
            .checked_add(AccessWidth::Dword.size())
            .ok_or(DeviceError::OutOfRange { addr: access.addr })?;
        if addr < base || access_end > end {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let offset = addr - base;
        if !offset.is_multiple_of(AccessWidth::Dword.size()) {
            return Err(DeviceError::InvalidInput {
                operation: "access PL011 register",
                detail: alloc::format!("register offset {offset:#x} is not 32-bit aligned"),
            });
        }
        Ok(offset)
    }

    fn read_register(&self, offset: usize) -> u32 {
        let state = self.state.lock();
        match offset {
            UARTDR | UARTRSR_ECR => 0,
            UARTFR => UARTFR_RXFE | UARTFR_TXFE,
            UARTILPR => state.ilpr,
            UARTIBRD => state.ibrd,
            UARTFBRD => state.fbrd,
            UARTLCR_H => state.lcr_h,
            UARTCR => state.cr,
            UARTIFLS => state.ifls,
            UARTIMSC => state.imsc,
            UARTRIS | UARTMIS | UARTICR => 0,
            UARTDMACR => state.dmacr,
            UARTPID0 => 0x11,
            UARTPID1 => 0x10,
            UARTPID2 => 0x14,
            UARTPID3 => 0x00,
            UARTCID0 => 0x0d,
            UARTCID1 => 0xf0,
            UARTCID2 => 0x05,
            UARTCID3 => 0xb1,
            _ => 0,
        }
    }

    fn write_register(&self, offset: usize, value: u32) {
        let chunk = {
            let mut state = self.state.lock();
            match offset {
                UARTDR => state.push_output(value as u8),
                UARTRSR_ECR | UARTFR | UARTRIS | UARTMIS | UARTICR | UARTPID0 | UARTPID1
                | UARTPID2 | UARTPID3 | UARTCID0 | UARTCID1 | UARTCID2 | UARTCID3 => None,
                UARTILPR => {
                    state.ilpr = value;
                    None
                }
                UARTIBRD => {
                    state.ibrd = value & UARTIBRD_MASK;
                    None
                }
                UARTFBRD => {
                    state.fbrd = value & UARTFBRD_MASK;
                    None
                }
                UARTLCR_H => {
                    state.lcr_h = value & UARTLCR_H_MASK;
                    None
                }
                UARTCR => {
                    state.cr = value & UARTCR_MASK;
                    None
                }
                UARTIFLS => {
                    state.ifls = value & UARTIFLS_MASK;
                    None
                }
                UARTIMSC => {
                    state.imsc = value & UARTIMSC_MASK;
                    None
                }
                UARTDMACR => {
                    state.dmacr = value & UARTDMACR_MASK;
                    None
                }
                _ => None,
            }
        };
        if let Some(chunk) = chunk {
            H::write_console_chunk(&self.name, chunk.as_bytes());
        }
    }
}

impl<H: Pl011ConsoleHostOps + 'static> Device for Pl011ConsoleDevice<H> {
    fn name(&self) -> &str {
        &self.name
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn handle(&self, access: &BusAccess) -> DeviceResult<BusResponse> {
        let offset = self.offset(access)?;
        if access.is_read {
            let access_mask = match access.width {
                AccessWidth::Byte => u32::from(u8::MAX),
                AccessWidth::Word => u32::from(u16::MAX),
                AccessWidth::Dword => u32::MAX,
                AccessWidth::Qword => unreachable!("Qword access was rejected during validation"),
            };
            Ok(BusResponse::Read {
                value: u64::from(self.read_register(offset) & access_mask),
            })
        } else {
            self.write_register(offset, access.data as u32);
            Ok(BusResponse::Write)
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn reset(&mut self) -> DeviceResult {
        self.state = Mutex::new(Pl011State::new());
        Ok(())
    }
}
