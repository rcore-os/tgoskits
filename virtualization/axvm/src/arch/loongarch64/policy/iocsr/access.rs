//! Validate an entire access before constructing an infallible commit.

use super::{
    super::registers::{
        iocsr_mbuf_send_box, iocsr_mbuf_send_buf, iocsr_send_action, iocsr_send_byte_mask,
        iocsr_send_cpu, iocsr_send_data,
    },
    *,
};

#[derive(Clone, Copy)]
enum StorageKind {
    Ordinary,
    IpiEnable,
    ReadOnly,
    Set,
    Clear,
    Isr,
}

enum Register<'a> {
    Constant(usize, usize),
    Gate {
        width: usize,
        mask: usize,
        sticky: bool,
    },
    Storage {
        slot: &'a AtomicUsize,
        shift: usize,
        widths: usize,
        kind: StorageKind,
    },
    Command {
        address: usize,
        width: usize,
    },
}

pub(super) enum Operation<'a> {
    Read {
        slot: &'a AtomicUsize,
        shift: usize,
        mask: usize,
    },
    Value(usize),
    Merge {
        slot: &'a AtomicUsize,
        mask: usize,
        value: usize,
        notify: Option<usize>,
    },
    Set {
        slot: &'a AtomicUsize,
        value: usize,
        target: usize,
    },
    Clear {
        slot: &'a AtomicUsize,
        value: usize,
        target: usize,
    },
    Gate(Option<bool>),
}

impl Operation<'_> {
    pub(super) fn commit(self, state: &LoongArchIocsrState) -> usize {
        match self {
            Self::Read { slot, shift, mask } => (slot.load(Ordering::Acquire) >> shift) & mask,
            Self::Value(value) => value,
            Self::Merge {
                slot,
                mask,
                value,
                notify,
            } => {
                // Keep unrelated halves/bytes in the same slot's modification order.
                let _ = slot.try_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                    Some((old & !mask) | (value & mask))
                });
                if let Some(target) = notify {
                    state.notify_pending(target);
                }
                0
            }
            Self::Set {
                slot,
                value,
                target,
            } => {
                slot.fetch_or(value, Ordering::AcqRel);
                state.notify_pending(target);
                0
            }
            Self::Clear {
                slot,
                value,
                target,
            } => {
                slot.fetch_and(!value, Ordering::AcqRel);
                state.notify_pending(target);
                0
            }
            Self::Gate(enabled) => {
                if let Some(enabled) = enabled {
                    state.eio_enabled.store(enabled, Ordering::Release);
                    if enabled {
                        for (id, valid) in state.configured_vcpus.iter().enumerate() {
                            if *valid {
                                state.notify_pending(id);
                            }
                        }
                    }
                }
                0
            }
        }
    }
}

type Reason = LoongArchIocsrFaultReason;

pub(super) fn prepare(
    state: &LoongArchIocsrState,
    id: usize,
    address: usize,
    width: usize,
    write: bool,
    value: usize,
) -> Result<Operation<'_>, Reason> {
    let vcpu = state.vcpu(id).ok_or(Reason::InvalidTarget { target: id })?;
    if !address.is_multiple_of(width) {
        return Err(Reason::Misaligned);
    }
    address.checked_add(width).ok_or(Reason::UnknownAddress)?;
    match resolve(vcpu, address)? {
        Register::Constant(constant, expected) => {
            check_width(width, expected)?;
            if write {
                return Err(Reason::Permission);
            }
            Ok(Operation::Value(constant))
        }
        Register::Gate {
            width: expected,
            mask,
            sticky,
        } => {
            check_width(width, expected)?;
            if !write {
                return Ok(Operation::Value(
                    if state.eio_enabled.load(Ordering::Acquire) {
                        mask
                    } else {
                        0
                    },
                ));
            }
            if value & !mask != 0 {
                return Err(Reason::ReservedBits);
            }
            Ok(Operation::Gate(if sticky && value == 0 {
                None
            } else {
                Some(value != 0)
            }))
        }
        Register::Storage {
            slot,
            shift,
            widths,
            kind,
        } => {
            check_width(width, widths)?;
            let mask = if width == 8 {
                usize::MAX
            } else {
                u32::MAX as usize
            };
            if !write {
                if matches!(kind, StorageKind::Set | StorageKind::Clear) {
                    return Err(Reason::Permission);
                }
                return Ok(Operation::Read { slot, shift, mask });
            }
            match kind {
                StorageKind::ReadOnly => Err(Reason::Permission),
                StorageKind::Set => Ok(Operation::Set {
                    slot,
                    value,
                    target: id,
                }),
                StorageKind::Clear | StorageKind::Isr => Ok(Operation::Clear {
                    slot,
                    value: value << shift,
                    target: id,
                }),
                StorageKind::Ordinary | StorageKind::IpiEnable => Ok(Operation::Merge {
                    slot,
                    mask: mask << shift,
                    value: value << shift,
                    notify: Some(id),
                }),
            }
        }
        Register::Command {
            address,
            width: expected,
        } => {
            check_width(width, expected)?;
            if !write {
                return Err(Reason::Permission);
            }
            prepare_send(state, address, value)
        }
    }
}

fn check_width(width: usize, supported: usize) -> Result<(), Reason> {
    if supported & width == width {
        Ok(())
    } else {
        Err(Reason::UnsupportedWidth)
    }
}

fn resolve(vcpu: &LoongArchVcpuIocsrState, address: usize) -> Result<Register<'_>, Reason> {
    let storage = |slot, shift, widths, kind| Register::Storage {
        slot,
        shift,
        widths,
        kind,
    };
    Ok(match address {
        0x8 => Register::Constant(0x18, 4),
        0x10 => Register::Constant(u64::from_le_bytes(*b"Axvisor\0") as usize, 8),
        0x20 => Register::Constant(u64::from_le_bytes(*b"LA64VM\0\0") as usize, 8),
        EXTIOI_VIRT_FEATURES => Register::Constant(0x3, 4),
        IOCSR_MISC_FUNC => Register::Gate {
            width: 8,
            mask: MISC_EIO_ENABLE,
            sticky: true,
        },
        EXTIOI_VIRT_CONFIG => Register::Gate {
            width: 4,
            mask: CONFIG_EIO_ENABLE,
            sticky: false,
        },
        LOONGARCH_IOCSR_IPI_STATUS => storage(&vcpu.ipi_status, 0, 4, StorageKind::ReadOnly),
        LOONGARCH_IOCSR_IPI_EN => storage(&vcpu.ipi_enable, 0, 4, StorageKind::IpiEnable),
        LOONGARCH_IOCSR_IPI_SET => storage(&vcpu.ipi_status, 0, 4, StorageKind::Set),
        LOONGARCH_IOCSR_IPI_CLEAR => storage(&vcpu.ipi_status, 0, 4, StorageKind::Clear),
        LOONGARCH_IOCSR_MAIL_BUF0..IOCSR_MAIL_END => storage(
            &vcpu.mail_buf[(address - LOONGARCH_IOCSR_MAIL_BUF0) / 8],
            (address % 8) * 8,
            4 | 8,
            StorageKind::Ordinary,
        ),
        LOONGARCH_IOCSR_IPI_SEND => Register::Command { address, width: 4 },
        LOONGARCH_IOCSR_MBUF_SEND | LOONGARCH_IOCSR_ANY_SEND => {
            Register::Command { address, width: 8 }
        }
        EIOINTC_ISR_COMPAT_BASE..EIOINTC_ISR_COMPAT_END => storage(
            &vcpu.eiointc_isr[(address - EIOINTC_ISR_COMPAT_BASE) / 8],
            (address % 8) * 8,
            4 | 8,
            StorageKind::Isr,
        ),
        EIOINTC_ISR_BASE..EIOINTC_ISR_END => storage(
            &vcpu.eiointc_isr[(address - EIOINTC_ISR_BASE) / 8],
            (address % 8) * 8,
            4 | 8,
            StorageKind::Isr,
        ),
        _ => {
            let slot = ordinary_bank(vcpu, address).ok_or(Reason::UnknownAddress)?;
            storage(slot, 0, 4, StorageKind::Ordinary)
        }
    })
}

fn ordinary_bank(vcpu: &LoongArchVcpuIocsrState, address: usize) -> Option<&AtomicUsize> {
    let (slots, base): (&[AtomicUsize], usize) = match address {
        EIOINTC_NODEMAP_BASE..EIOINTC_NODEMAP_END => (&vcpu.eiointc_nodemap, EIOINTC_NODEMAP_BASE),
        EIOINTC_IPMAP_BASE..EIOINTC_IPMAP_END => (&vcpu.eiointc_ipmap, EIOINTC_IPMAP_BASE),
        EIOINTC_ENABLE_BASE..EIOINTC_ENABLE_END => (&vcpu.eiointc_enable, EIOINTC_ENABLE_BASE),
        EIOINTC_BOUNCE_BASE..EIOINTC_BOUNCE_END => (&vcpu.eiointc_bounce, EIOINTC_BOUNCE_BASE),
        EIOINTC_COREMAP_BASE..EIOINTC_COREMAP_END => (&vcpu.eiointc_coremap, EIOINTC_COREMAP_BASE),
        _ => return None,
    };
    slots.get((address - base) / 4)
}

fn prepare_send(
    state: &LoongArchIocsrState,
    address: usize,
    value: usize,
) -> Result<Operation<'_>, Reason> {
    const CPU_MASK: usize = 0x3ff << 16;
    const BLOCKING: usize = 1 << 31;
    let allowed = match address {
        LOONGARCH_IOCSR_IPI_SEND => 0x1f | CPU_MASK | BLOCKING,
        LOONGARCH_IOCSR_MBUF_SEND => (u32::MAX as usize) << 32 | 0x1c | CPU_MASK | BLOCKING,
        LOONGARCH_IOCSR_ANY_SEND => {
            (u32::MAX as usize) << 32 | 0xffff | CPU_MASK | (0xf << 27) | BLOCKING
        }
        _ => return Err(Reason::UnknownAddress),
    };
    if value & !allowed != 0 {
        return Err(Reason::ReservedBits);
    }
    let target = iocsr_send_cpu(value);
    let vcpu = state.vcpu(target).ok_or(Reason::InvalidTarget { target })?;
    match address {
        LOONGARCH_IOCSR_IPI_SEND => Ok(Operation::Set {
            slot: &vcpu.ipi_status,
            value: 1 << iocsr_send_action(value),
            target,
        }),
        LOONGARCH_IOCSR_MBUF_SEND => {
            let half = iocsr_mbuf_send_box(value);
            let shift = (half % 2) * 32;
            Ok(Operation::Merge {
                slot: &vcpu.mail_buf[half / 2],
                mask: (u32::MAX as usize) << shift,
                value: iocsr_mbuf_send_buf(value) << shift,
                notify: None,
            })
        }
        LOONGARCH_IOCSR_ANY_SEND => {
            let nested = value & 0xffff;
            if !nested.is_multiple_of(4) {
                return Err(Reason::Misaligned);
            }
            let (slot, shift) = match resolve(vcpu, nested) {
                Ok(Register::Storage {
                    slot,
                    shift,
                    kind: StorageKind::Ordinary | StorageKind::IpiEnable,
                    ..
                }) => (slot, shift),
                _ => return Err(Reason::InvalidSendTarget { address: nested }),
            };
            let preserved = iocsr_send_byte_mask(value);
            let mut mask = 0;
            for byte in 0..4 {
                if preserved & (1 << byte) == 0 {
                    mask |= 0xffusize << (byte * 8);
                }
            }
            Ok(Operation::Merge {
                slot,
                mask: mask << shift,
                value: iocsr_send_data(value) << shift,
                notify: Some(target),
            })
        }
        _ => unreachable!("validated command address"),
    }
}
