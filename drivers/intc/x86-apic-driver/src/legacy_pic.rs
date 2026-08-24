//! Host 8259 PIC support for PC/AT-compatible virtual-wire systems.

const MASTER_COMMAND: u16 = 0x20;
const MASTER_DATA: u16 = 0x21;
const SLAVE_COMMAND: u16 = 0xa0;
const SLAVE_DATA: u16 = 0xa1;
const MASTER_VECTOR_BASE: u8 = 0x30;
const SLAVE_VECTOR_BASE: u8 = 0x38;
const NON_SPECIFIC_EOI: u8 = 0x20;
const CASCADE_IRQ: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PicWrite {
    port: u16,
    value: u8,
}

const EMPTY_WRITE: PicWrite = PicWrite { port: 0, value: 0 };

fn initialization_writes() -> [PicWrite; 12] {
    [
        PicWrite {
            port: MASTER_DATA,
            value: 0xff,
        },
        PicWrite {
            port: SLAVE_DATA,
            value: 0xff,
        },
        PicWrite {
            port: MASTER_COMMAND,
            value: 0x11,
        },
        PicWrite {
            port: MASTER_DATA,
            value: MASTER_VECTOR_BASE,
        },
        PicWrite {
            port: MASTER_DATA,
            value: 1 << CASCADE_IRQ,
        },
        PicWrite {
            port: MASTER_DATA,
            value: 0x01,
        },
        PicWrite {
            port: SLAVE_COMMAND,
            value: 0x11,
        },
        PicWrite {
            port: SLAVE_DATA,
            value: SLAVE_VECTOR_BASE,
        },
        PicWrite {
            port: SLAVE_DATA,
            value: CASCADE_IRQ,
        },
        PicWrite {
            port: SLAVE_DATA,
            value: 0x01,
        },
        PicWrite {
            port: MASTER_DATA,
            value: 0xff,
        },
        PicWrite {
            port: SLAVE_DATA,
            value: 0xff,
        },
    ]
}

fn updated_mask(mask: u8, irq: u8, enabled: bool) -> u8 {
    if enabled {
        mask & !(1 << irq)
    } else {
        mask | (1 << irq)
    }
}

fn eoi_writes(irq: u8) -> ([PicWrite; 2], usize) {
    if irq < 8 {
        (
            [
                PicWrite {
                    port: MASTER_COMMAND,
                    value: NON_SPECIFIC_EOI,
                },
                EMPTY_WRITE,
            ],
            1,
        )
    } else {
        (
            [
                PicWrite {
                    port: SLAVE_COMMAND,
                    value: NON_SPECIFIC_EOI,
                },
                PicWrite {
                    port: MASTER_COMMAND,
                    value: NON_SPECIFIC_EOI,
                },
            ],
            2,
        )
    }
}

/// Register-level owner for one PC/AT-compatible 8259 PIC pair.
pub struct X86LegacyPic {
    master_mask: u8,
    slave_mask: u8,
    initialized: bool,
}

impl X86LegacyPic {
    /// Creates a dormant PIC owner without touching hardware.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that no other live driver instance owns or
    /// accesses the standard PC/AT PIC ports.
    pub const unsafe fn new() -> Self {
        Self {
            master_mask: u8::MAX,
            slave_mask: u8::MAX,
            initialized: false,
        }
    }

    /// Remaps the PIC pair into the kernel external-vector range and masks all sources.
    ///
    /// # Safety
    ///
    /// The caller must own the standard PC/AT PIC ports and exclude concurrent
    /// access while the initialization command words are in flight.
    pub unsafe fn initialize(&mut self) {
        for write in initialization_writes() {
            unsafe { write_pic(write) };
        }
        self.master_mask = u8::MAX;
        self.slave_mask = u8::MAX;
        self.initialized = true;
    }

    /// Masks or unmasks one PIC input.
    pub fn set_irq_enabled(&mut self, irq: u8, enabled: bool) -> Result<(), crate::ApicError> {
        if irq >= 16 {
            return Err(crate::ApicError::InvalidLegacyPicIrq(irq));
        }
        if !self.initialized {
            unsafe { self.initialize() };
        }
        if irq < 8 {
            self.master_mask = updated_mask(self.master_mask, irq, enabled);
        } else {
            self.slave_mask = updated_mask(self.slave_mask, irq - 8, enabled);
            self.master_mask =
                updated_mask(self.master_mask, CASCADE_IRQ, self.slave_mask != u8::MAX);
        }
        unsafe {
            write_pic(PicWrite {
                port: MASTER_DATA,
                value: self.master_mask,
            });
            write_pic(PicWrite {
                port: SLAVE_DATA,
                value: self.slave_mask,
            });
        }
        Ok(())
    }

    /// Completes one acknowledged PIC interrupt.
    pub fn eoi(&mut self, irq: u8) -> Result<(), crate::ApicError> {
        if irq >= 16 {
            return Err(crate::ApicError::InvalidLegacyPicIrq(irq));
        }
        let (writes, len) = eoi_writes(irq);
        for write in writes.into_iter().take(len) {
            unsafe { write_pic(write) };
        }
        Ok(())
    }
}

unsafe fn write_pic(write: PicWrite) {
    unsafe {
        x86::io::outb(write.port, write.value);
        x86::io::outb(0x80, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialization_remaps_both_pics_and_leaves_every_source_masked() {
        assert_eq!(
            initialization_writes(),
            [
                PicWrite {
                    port: MASTER_DATA,
                    value: 0xff
                },
                PicWrite {
                    port: SLAVE_DATA,
                    value: 0xff
                },
                PicWrite {
                    port: MASTER_COMMAND,
                    value: 0x11
                },
                PicWrite {
                    port: MASTER_DATA,
                    value: MASTER_VECTOR_BASE
                },
                PicWrite {
                    port: MASTER_DATA,
                    value: 1 << 2
                },
                PicWrite {
                    port: MASTER_DATA,
                    value: 0x01
                },
                PicWrite {
                    port: SLAVE_COMMAND,
                    value: 0x11
                },
                PicWrite {
                    port: SLAVE_DATA,
                    value: SLAVE_VECTOR_BASE
                },
                PicWrite {
                    port: SLAVE_DATA,
                    value: 2
                },
                PicWrite {
                    port: SLAVE_DATA,
                    value: 0x01
                },
                PicWrite {
                    port: MASTER_DATA,
                    value: 0xff
                },
                PicWrite {
                    port: SLAVE_DATA,
                    value: 0xff
                },
            ]
        );
    }

    #[test]
    fn master_irq_mask_and_eoi_are_scoped_to_irq4() {
        assert_eq!(updated_mask(0xff, 4, true), 0xef);
        assert_eq!(updated_mask(0xef, 4, false), 0xff);
        assert_eq!(
            eoi_writes(4),
            (
                [
                    PicWrite {
                        port: MASTER_COMMAND,
                        value: NON_SPECIFIC_EOI
                    },
                    EMPTY_WRITE
                ],
                1
            )
        );
    }

    #[test]
    fn slave_eoi_precedes_master_cascade_eoi() {
        assert_eq!(
            eoi_writes(12),
            (
                [
                    PicWrite {
                        port: SLAVE_COMMAND,
                        value: NON_SPECIFIC_EOI
                    },
                    PicWrite {
                        port: MASTER_COMMAND,
                        value: NON_SPECIFIC_EOI
                    },
                ],
                2
            )
        );
    }
}
