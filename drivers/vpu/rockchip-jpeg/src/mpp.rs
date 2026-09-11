//! Rockchip MPP (`/dev/mpp_service`) ABI for the JPEG decoder.
//!
//! This module reproduces the kernel-side wire ABI that `librockchip_mpp` uses,
//! so unmodified MPP consumers (`mpi_dec_test -t 8`, gstreamer `mppjpegdec`,
//! ffmpeg `rkmpp`) can drive this driver. It is OS-independent: the OS glue does
//! the `copy_from_user`/`copy_to_user` and dma-buf-fd → physical-address lookup,
//! and feeds the decoded commands into [`MppSession`], which assembles the final
//! register array for [`crate::JpuCore`].
//!
//! References: rockchip-linux/mpp `osal/inc/mpp_service.h` and the vendor kernel
//! `include/uapi/linux/rk-mpp.h` (`develop-5.10`).

use crate::registers::{self, ADDR_REG_INDICES, REG_COUNT};

/// `MPP_IOC_CFG_V1 = _IOW('v', 1, unsigned int)`.
pub const MPP_IOC_CFG_V1: u32 = 0x4004_7601;
/// `MPP_IOC_CFG_V2`.
pub const MPP_IOC_CFG_V2: u32 = 0x4004_7602;

/// Client/device type for the RK JPEG decoder (`VPU_CLIENT_JPEG_DEC`).
pub const MPP_DEVICE_RKJPEGD: u32 = 13;
/// `PROBE_HW_SUPPORT` bit reported for the JPEG decoder (`HAVE_JPEG_DEC`).
pub const HW_SUPPORT_JPEG_DEC: u32 = 1 << 13;

/// Maximum chained request records per ioctl (`MPP_MAX_MSG_NUM`).
pub const MAX_MSG_NUM: usize = 16;
/// Maximum register address-offset fixups tracked per task.
pub const MAX_REG_OFFSETS: usize = 16;

/// `mppReqV1` request flags.
pub mod flags {
    /// More records follow in this ioctl batch.
    pub const MULTI_MSG: u32 = 0x0000_0001;
    /// Final record in this ioctl batch.
    pub const LAST_MSG: u32 = 0x0000_0002;
    /// Register fds are pre-translated; do not import.
    pub const REG_FD_NO_TRANS: u32 = 0x0000_0004;
    /// Address offsets are supplied via `SET_REG_ADDR_OFFSET`, not packed in regs.
    pub const REG_OFFSET_ALONE: u32 = 0x0000_0010;
    /// `POLL_HW_FINISH` should not block.
    pub const POLL_NON_BLOCK: u32 = 0x0000_0020;
}

/// `MppServiceCmdType` command ids (the `cmd` field of a request).
pub mod cmd {
    /// Query the codec-type bitmap supported by this node.
    pub const PROBE_HW_SUPPORT: u32 = 0x000;
    /// Query the hardware id / version.
    pub const QUERY_HW_ID: u32 = 0x001;
    /// Query which commands the node supports.
    pub const QUERY_CMD_SUPPORT: u32 = 0x002;
    /// Bind this session to a client type.
    pub const INIT_CLIENT_TYPE: u32 = 0x100;
    /// Provide the register array to write to hardware.
    pub const SET_REG_WRITE: u32 = 0x200;
    /// Declare which registers to read back after completion.
    pub const SET_REG_READ: u32 = 0x201;
    /// Provide `{index, offset}` address fixups.
    pub const SET_REG_ADDR_OFFSET: u32 = 0x202;
    /// Submit + block until this task's interrupt; copy registers back.
    pub const POLL_HW_FINISH: u32 = 0x300;
    /// Submit + wait on the hardware interrupt (non-blocking variant).
    pub const POLL_HW_IRQ: u32 = 0x301;
    /// Reset the session.
    pub const RESET_SESSION: u32 = 0x400;
    /// Map a dma-buf fd to a device iova.
    pub const TRANS_FD_TO_IOVA: u32 = 0x401;
    /// Release a previously mapped fd.
    pub const RELEASE_FD: u32 = 0x402;
}

/// One wire request record (`mppReqV1` / `struct mpp_request`), 24 bytes.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MppRequest {
    /// Command id (see [`cmd`]).
    pub cmd: u32,
    /// Flags (see [`flags`]).
    pub flag: u32,
    /// Byte size of the payload at `data_ptr`.
    pub size: u32,
    /// Sub-offset for batched register read/write.
    pub offset: u32,
    /// Userspace pointer to this command's payload.
    pub data_ptr: u64,
}

/// One `SET_REG_ADDR_OFFSET` element (`struct reg_offset_elem`).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RegOffset {
    /// Register index to fix up.
    pub index: u32,
    /// Byte offset added to the resolved address.
    pub offset: u32,
}

/// Errors from driving the MPP session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MppError {
    /// The client type is not the JPEG decoder.
    #[error("MPP client type {0} is not the JPEG decoder")]
    BadClientType(u32),
    /// `POLL_HW_FINISH` arrived before any register array was set.
    #[error("MPP poll requested before registers were configured")]
    NoRegisters,
    /// A register address-slot fd could not be resolved to a physical address.
    #[error("MPP dma-buf file descriptor {0} could not be resolved")]
    UnresolvedFd(u32),
    /// More address-offset fixups than supported.
    #[error("MPP register offset table is full")]
    TooManyOffsets,
    /// The resolved address plus its offset wrapped the 32-bit device address.
    #[error("MPP dma-buf address calculation overflowed")]
    AddressOverflow,
    /// The requested DMA range is outside the imported dma-buf allocation.
    #[error("MPP DMA range is outside the imported dma-buf")]
    DmaRangeOutOfBounds,
    /// The hardware address register requires a stronger alignment.
    #[error("MPP DMA address is not correctly aligned")]
    MisalignedAddress,
    /// The register geometry selects an output layout not supported by this node.
    #[error("MPP JPEG output geometry is unsupported")]
    UnsupportedGeometry,
}

/// Device address and allocation size of an imported contiguous dma-buf.
///
/// The address is the physical/bus address visible to the IOMMU-bypassed JPEG
/// block. `size` is the complete allocation size, not the userspace payload
/// length, and is required to validate every register-derived DMA range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedDmaBuf {
    /// 32-bit device-visible base address.
    pub address: u32,
    /// Number of bytes owned by the allocation.
    pub size: usize,
}

/// Accumulated state for one MPP decode task.
///
/// The OS glue calls the `handle_*` methods as it decodes the request chain,
/// then [`MppSession::resolve_addresses`] (supplying an fd → physical-address
/// resolver) to produce the final register array in [`MppSession::regs`].
#[derive(Debug, Clone)]
pub struct MppSession {
    client_type: Option<u32>,
    regs: [u32; REG_COUNT],
    have_regs: bool,
    read_first_word: usize,
    read_word_count: usize,
    offsets: [RegOffset; MAX_REG_OFFSETS],
    offset_count: usize,
}

impl Default for MppSession {
    fn default() -> Self {
        Self::new()
    }
}

impl MppSession {
    /// Create an empty session.
    pub const fn new() -> Self {
        Self {
            client_type: None,
            regs: [0; REG_COUNT],
            have_regs: false,
            read_first_word: 0,
            read_word_count: 0,
            offsets: [RegOffset {
                index: 0,
                offset: 0,
            }; MAX_REG_OFFSETS],
            offset_count: 0,
        }
    }

    /// Handle `INIT_CLIENT_TYPE`; only the JPEG decoder client is accepted.
    pub fn init_client_type(&mut self, client: u32) -> Result<(), MppError> {
        if client != MPP_DEVICE_RKJPEGD {
            return Err(MppError::BadClientType(client));
        }
        self.client_type = Some(client);
        Ok(())
    }

    /// Handle `SET_REG_WRITE`: copy the register words into the task.
    pub fn set_reg_write(&mut self, words: &[u32]) {
        let n = words.len().min(REG_COUNT);
        self.regs[..n].copy_from_slice(&words[..n]);
        self.have_regs = true;
    }

    /// Handle `SET_REG_READ`: record which register window to copy back.
    pub fn set_reg_read(&mut self, offset_bytes: u32, size_bytes: u32) {
        self.read_first_word = (offset_bytes / 4) as usize;
        self.read_word_count = (size_bytes / 4) as usize;
    }

    /// Handle `SET_REG_ADDR_OFFSET`: record `{index, offset}` fixups.
    pub fn add_reg_offsets(&mut self, elems: &[RegOffset]) -> Result<(), MppError> {
        for &elem in elems {
            if self.offset_count >= MAX_REG_OFFSETS {
                return Err(MppError::TooManyOffsets);
            }
            self.offsets[self.offset_count] = elem;
            self.offset_count += 1;
        }
        Ok(())
    }

    /// Resolve fd-bearing address registers to physical addresses (plus their
    /// recorded offsets), producing the final register array.
    ///
    /// The resolver must return the complete imported allocation size. Before
    /// programming the device, this method checks the offset, the DMA length
    /// derived from the register geometry, the 32-bit address addition, and the
    /// alignment required by each address register.
    pub fn resolve_addresses<F>(&mut self, mut resolve_fd: F) -> Result<(), MppError>
    where
        F: FnMut(u32) -> Option<ResolvedDmaBuf>,
    {
        if !self.have_regs {
            return Err(MppError::NoRegisters);
        }
        let mut resolved = self.regs;
        for &idx in ADDR_REG_INDICES {
            let fd = self.regs[idx];
            if fd == 0 {
                continue; // unused address slot
            }
            let buffer = resolve_fd(fd).ok_or(MppError::UnresolvedFd(fd))?;
            let offset = self.offset_for(idx as u32);
            let length = self.dma_length(idx)?;
            let end = (offset as usize)
                .checked_add(length)
                .ok_or(MppError::AddressOverflow)?;
            if end > buffer.size {
                return Err(MppError::DmaRangeOutOfBounds);
            }

            let address = buffer
                .address
                .checked_add(offset)
                .ok_or(MppError::AddressOverflow)?;
            if address & (Self::address_alignment(idx) - 1) != 0 {
                return Err(MppError::MisalignedAddress);
            }
            resolved[idx] = address;
        }
        self.regs = resolved;
        Ok(())
    }

    fn dma_length(&self, index: usize) -> Result<usize, MppError> {
        match index {
            registers::REG_QTBL_BASE => table_length(self.regs[registers::REG_TABLE_LEN], 0, 0x1f),
            registers::REG_HUFFMIN_BASE => {
                table_length(self.regs[registers::REG_TABLE_LEN], 8, 0x1f)
            }
            registers::REG_HUFFVAL_BASE => {
                table_length(self.regs[registers::REG_TABLE_LEN], 16, 0x3f)
            }
            registers::REG_STRM_BASE => {
                let reg = self.regs[registers::REG_STRM_LEN];
                let blocks = ((reg >> 4) & 0x0fff_ffff) as usize;
                // The low nibble is consumed by the hardware as the byte
                // position within the first block; it is not another byte
                // in the DMA range.
                (blocks + 1)
                    .checked_mul(16)
                    .ok_or(MppError::AddressOverflow)
            }
            registers::REG_DEC_OUT_BASE => self.output_length(),
            _ => Err(MppError::UnsupportedGeometry),
        }
    }

    fn output_length(&self) -> Result<usize, MppError> {
        let sys = self.regs[registers::REG_SYS];
        let output_format = (sys >> registers::OUT_FMT_SHIFT) & 0x7;
        if output_format != registers::OUT_FMT_NATIVE && output_format != registers::OUT_FMT_NV12
            || sys & registers::SYS_OUT_SEQ != 0
        {
            return Err(MppError::UnsupportedGeometry);
        }

        let mode = self.regs[registers::REG_PIC_FMT] & 0x7;
        let vertical_subsampling =
            matches!(mode, registers::JPEG_MODE_420 | registers::JPEG_MODE_440);
        let height_alignment = if sys & registers::SYS_FILL_DOWN != 0 {
            16
        } else {
            8
        };
        match mode {
            registers::JPEG_MODE_400
            | registers::JPEG_MODE_411
            | registers::JPEG_MODE_420
            | registers::JPEG_MODE_440
            | registers::JPEG_MODE_422
            | registers::JPEG_MODE_444 => {}
            _ => return Err(MppError::UnsupportedGeometry),
        }
        let height = ((self.regs[registers::REG_PIC_SIZE] >> 16) & 0xffff) as usize + 1;
        let height = height
            .checked_add(height_alignment - 1)
            .ok_or(MppError::AddressOverflow)?
            / height_alignment
            * height_alignment;

        let width = (self.regs[registers::REG_PIC_SIZE] & 0xffff) as usize + 1;
        let width_aligned = width.checked_add(15).ok_or(MppError::AddressOverflow)? / 16 * 16;
        let minimum_y_stride_units = width_aligned / 16;
        let y_stride_units = (self.regs[registers::REG_HOR_VIRSTRIDE] & 0xffff) as usize
            | (((self.regs[registers::REG_TABLE_LEN] >> 24) & 1) as usize) << 16;
        if y_stride_units < minimum_y_stride_units {
            return Err(MppError::UnsupportedGeometry);
        }
        let uv_stride_units = (self.regs[registers::REG_HOR_VIRSTRIDE] >> 16) as usize;
        let minimum_uv_stride_units = match mode {
            registers::JPEG_MODE_400 => 0,
            registers::JPEG_MODE_411 => (minimum_y_stride_units / 2).max(1),
            registers::JPEG_MODE_420 | registers::JPEG_MODE_422 => minimum_y_stride_units,
            registers::JPEG_MODE_440 | registers::JPEG_MODE_444 => minimum_y_stride_units
                .checked_mul(2)
                .ok_or(MppError::AddressOverflow)?,
            _ => return Err(MppError::UnsupportedGeometry),
        };
        if uv_stride_units < minimum_uv_stride_units {
            return Err(MppError::UnsupportedGeometry);
        }

        let y_stride = y_stride_units
            .checked_mul(16)
            .ok_or(MppError::AddressOverflow)?;
        let uv_stride = uv_stride_units
            .checked_mul(16)
            .ok_or(MppError::AddressOverflow)?;
        let y_rows = y_stride
            .checked_mul(height)
            .ok_or(MppError::AddressOverflow)?;
        let y_virstride = ((self.regs[registers::REG_Y_VIRSTRIDE] >> 4) as usize)
            .checked_mul(16)
            .ok_or(MppError::AddressOverflow)?;
        if y_virstride < y_rows {
            return Err(MppError::UnsupportedGeometry);
        }
        let y_size = y_virstride;
        let chroma_height = if vertical_subsampling {
            height / 2
        } else {
            height
        };
        let uv_size = if mode == registers::JPEG_MODE_400 {
            0
        } else {
            uv_stride
                .checked_mul(chroma_height)
                .ok_or(MppError::AddressOverflow)?
        };
        y_size.checked_add(uv_size).ok_or(MppError::AddressOverflow)
    }

    const fn address_alignment(index: usize) -> u32 {
        match index {
            registers::REG_STRM_BASE => 16,
            _ => 64,
        }
    }

    fn offset_for(&self, index: u32) -> u32 {
        self.offsets[..self.offset_count]
            .iter()
            .find(|e| e.index == index)
            .map_or(0, |e| e.offset)
    }

    /// The (possibly resolved) register array to program.
    pub fn regs(&self) -> &[u32; REG_COUNT] {
        &self.regs
    }

    /// The register read-back window as `(first_word, word_count)`.
    pub fn read_window(&self) -> (usize, usize) {
        (self.read_first_word, self.read_word_count)
    }

    /// The bound client type, if any.
    pub fn client_type(&self) -> Option<u32> {
        self.client_type
    }

    /// Clear per-task state (registers, address fixups, read window) after a
    /// frame completes, keeping the bound client type for the next frame.
    pub fn clear_task(&mut self) {
        self.regs = [0; REG_COUNT];
        self.have_regs = false;
        self.read_first_word = 0;
        self.read_word_count = 0;
        self.offset_count = 0;
    }

    /// Reset to the empty state (`RESET_SESSION`).
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

fn table_length(reg: u32, shift: u32, mask: u32) -> Result<usize, MppError> {
    (((reg >> shift) & mask) as usize + 1)
        .checked_mul(16)
        .ok_or(MppError::AddressOverflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registers;

    #[test]
    fn ioctl_and_constants() {
        assert_eq!(MPP_IOC_CFG_V1, 0x4004_7601);
        assert_eq!(MPP_DEVICE_RKJPEGD, 13);
        assert_eq!(HW_SUPPORT_JPEG_DEC, 0x2000);
        assert_eq!(core::mem::size_of::<MppRequest>(), 24);
        assert_eq!(core::mem::size_of::<RegOffset>(), 8);
    }

    #[test]
    fn accepts_jpeg_client_rejects_others() {
        let mut s = MppSession::new();
        assert_eq!(s.init_client_type(MPP_DEVICE_RKJPEGD), Ok(()));
        assert_eq!(s.client_type(), Some(MPP_DEVICE_RKJPEGD));
        let mut s2 = MppSession::new();
        assert_eq!(s2.init_client_type(8), Err(MppError::BadClientType(8)));
    }

    #[test]
    fn resolves_fd_address_slots_with_offsets() {
        let mut s = MppSession::new();
        let mut words = [0u32; REG_COUNT];
        // Address slots carry bare fds (jpegd convention).
        words[registers::REG_QTBL_BASE] = 7; // table fd
        words[registers::REG_HUFFMIN_BASE] = 7;
        words[registers::REG_HUFFVAL_BASE] = 7;
        words[registers::REG_STRM_BASE] = 9; // stream fd
        words[registers::REG_DEC_OUT_BASE] = 11; // output fd
        words[registers::REG_PIC_SIZE] = 15 | (15 << 16);
        words[registers::REG_HOR_VIRSTRIDE] = 1 | (1 << 16);
        words[registers::REG_Y_VIRSTRIDE] = 16 << 4;
        s.set_reg_write(&words);
        // Offsets, as MPP sends for jpegd (table mincode/value, stream start).
        s.add_reg_offsets(&[
            RegOffset {
                index: registers::REG_HUFFMIN_BASE as u32,
                offset: 384,
            },
            RegOffset {
                index: registers::REG_HUFFVAL_BASE as u32,
                offset: 704,
            },
            RegOffset {
                index: registers::REG_STRM_BASE as u32,
                offset: 32,
            },
        ])
        .unwrap();

        // fd 7 -> 0x1000_0000 (table), 9 -> 0x2000_0000 (stream), 11 -> 0x3000_0000.
        let resolve = |fd: u32| match fd {
            7 => Some(ResolvedDmaBuf {
                address: 0x1000_0000,
                size: 4096,
            }),
            9 => Some(ResolvedDmaBuf {
                address: 0x2000_0000,
                size: 4096,
            }),
            11 => Some(ResolvedDmaBuf {
                address: 0x3000_0000,
                size: 4096,
            }),
            _ => None,
        };
        s.resolve_addresses(resolve).unwrap();

        assert_eq!(s.regs()[registers::REG_QTBL_BASE], 0x1000_0000);
        assert_eq!(s.regs()[registers::REG_HUFFMIN_BASE], 0x1000_0000 + 384);
        assert_eq!(s.regs()[registers::REG_HUFFVAL_BASE], 0x1000_0000 + 704);
        assert_eq!(s.regs()[registers::REG_STRM_BASE], 0x2000_0000 + 32);
        assert_eq!(s.regs()[registers::REG_DEC_OUT_BASE], 0x3000_0000);
    }

    #[test]
    fn resolve_fails_on_unknown_fd() {
        let mut s = MppSession::new();
        let mut words = [0u32; REG_COUNT];
        words[registers::REG_STRM_BASE] = 42;
        s.set_reg_write(&words);
        assert_eq!(
            s.resolve_addresses(|_| None),
            Err(MppError::UnresolvedFd(42))
        );
    }

    #[test]
    fn resolve_without_regs_errors() {
        let mut s = MppSession::new();
        assert_eq!(
            s.resolve_addresses(|_| {
                Some(ResolvedDmaBuf {
                    address: 0,
                    size: 4096,
                })
            }),
            Err(MppError::NoRegisters)
        );
    }

    #[test]
    fn rejects_wrapping_dma_address_offset() {
        let mut s = MppSession::new();
        let mut words = [0u32; REG_COUNT];
        words[registers::REG_STRM_BASE] = 9;
        s.set_reg_write(&words);
        s.add_reg_offsets(&[RegOffset {
            index: registers::REG_STRM_BASE as u32,
            offset: 0x20,
        }])
        .unwrap();

        // A checked address calculation must reject this instead of programming
        // the wrapped address 0x10 into the decoder.
        assert!(
            s.resolve_addresses(|_| {
                Some(ResolvedDmaBuf {
                    address: 0xffff_fff0,
                    size: 4096,
                })
            })
            .is_err()
        );
    }

    #[test]
    fn rejects_dma_range_at_or_past_imported_buffer() {
        let mut s = MppSession::new();
        let mut words = [0u32; REG_COUNT];
        words[registers::REG_STRM_BASE] = 9;
        s.set_reg_write(&words);
        s.add_reg_offsets(&[RegOffset {
            index: registers::REG_STRM_BASE as u32,
            offset: 4081,
        }])
        .unwrap();

        // The stream register describes one 16-byte DMA block. The first four
        // bytes would fit in a 4096-byte allocation, but the full range does not.
        assert_eq!(
            s.resolve_addresses(|_| {
                Some(ResolvedDmaBuf {
                    address: 0x2000_0000,
                    size: 4096,
                })
            }),
            Err(MppError::DmaRangeOutOfBounds)
        );
    }

    #[test]
    fn rejects_output_stride_beyond_imported_buffer() {
        let mut s = MppSession::new();
        let mut words = [0u32; REG_COUNT];
        words[registers::REG_DEC_OUT_BASE] = 11;
        words[registers::REG_SYS] = registers::SYS_FILL_DOWN;
        words[registers::REG_PIC_SIZE] = 15 | (15 << 16);
        words[registers::REG_PIC_FMT] = registers::JPEG_MODE_420;
        words[registers::REG_HOR_VIRSTRIDE] = 1 | (1 << 16);
        words[registers::REG_Y_VIRSTRIDE] = 32 << 4;
        s.set_reg_write(&words);

        // The programmed Y stride already consumes 512 bytes; the NV12 chroma
        // plane adds another 256 bytes, so a 512-byte import must be rejected.
        assert_eq!(
            s.resolve_addresses(|_| {
                Some(ResolvedDmaBuf {
                    address: 0x3000_0000,
                    size: 512,
                })
            }),
            Err(MppError::DmaRangeOutOfBounds)
        );
    }

    #[test]
    fn accepts_exact_nv12_output_allocation_size() {
        let mut s = MppSession::new();
        let mut words = [0u32; REG_COUNT];
        words[registers::REG_DEC_OUT_BASE] = 11;
        words[registers::REG_SYS] = registers::SYS_FILL_DOWN;
        words[registers::REG_PIC_SIZE] = 15 | (15 << 16);
        words[registers::REG_PIC_FMT] = registers::JPEG_MODE_420;
        words[registers::REG_HOR_VIRSTRIDE] = 1 | (1 << 16);
        words[registers::REG_Y_VIRSTRIDE] = 16 << 4;
        s.set_reg_write(&words);

        // 256 bytes of Y plus 128 bytes of 4:2:0 chroma is the exact legal end
        // of this two-plane output allocation.
        s.resolve_addresses(|_| {
            Some(ResolvedDmaBuf {
                address: 0x3000_0000,
                size: 384,
            })
        })
        .unwrap();
        assert_eq!(s.regs()[registers::REG_DEC_OUT_BASE], 0x3000_0000);
    }

    #[test]
    fn accepts_exact_nv12_output_allocation_size_for_440() {
        let mut s = MppSession::new();
        let mut words = [0u32; REG_COUNT];
        words[registers::REG_DEC_OUT_BASE] = 11;
        words[registers::REG_PIC_SIZE] = 15 | (7 << 16);
        words[registers::REG_PIC_FMT] = registers::JPEG_MODE_440;
        words[registers::REG_HOR_VIRSTRIDE] = 1 | (2 << 16);
        words[registers::REG_Y_VIRSTRIDE] = 8 << 4;
        s.set_reg_write(&words);

        // 128 bytes of Y plus 128 bytes of 4:4:0 chroma is exact; its UV plane
        // has half as many rows as the Y plane but twice its row stride.
        s.resolve_addresses(|_| {
            Some(ResolvedDmaBuf {
                address: 0x3000_0000,
                size: 256,
            })
        })
        .unwrap();
    }

    #[test]
    fn rejects_horizontal_stride_smaller_than_picture_width() {
        let mut s = MppSession::new();
        let mut words = [0u32; REG_COUNT];
        words[registers::REG_DEC_OUT_BASE] = 11;
        words[registers::REG_SYS] = registers::SYS_FILL_DOWN;
        // 63 pixels require a 64-byte aligned Y row, but the client supplies
        // only one 16-byte stride unit.
        words[registers::REG_PIC_SIZE] = 62 | (15 << 16);
        words[registers::REG_PIC_FMT] = registers::JPEG_MODE_420;
        words[registers::REG_HOR_VIRSTRIDE] = 1 | (1 << 16);
        words[registers::REG_Y_VIRSTRIDE] = 16 << 4;
        s.set_reg_write(&words);

        assert_eq!(
            s.resolve_addresses(|_| {
                Some(ResolvedDmaBuf {
                    address: 0x3000_0000,
                    size: 384,
                })
            }),
            Err(MppError::UnsupportedGeometry)
        );
    }

    #[test]
    fn rejects_uv_stride_smaller_than_picture_width() {
        let mut s = MppSession::new();
        let mut words = [0u32; REG_COUNT];
        words[registers::REG_DEC_OUT_BASE] = 11;
        words[registers::REG_SYS] = registers::SYS_FILL_DOWN;
        words[registers::REG_PIC_SIZE] = 31 | (15 << 16);
        words[registers::REG_PIC_FMT] = registers::JPEG_MODE_420;
        // Y has the required 32-byte stride, while NV12 UV has only 16 bytes.
        words[registers::REG_HOR_VIRSTRIDE] = 2;
        words[registers::REG_Y_VIRSTRIDE] = 32 << 4;
        s.set_reg_write(&words);

        assert_eq!(
            s.resolve_addresses(|_| {
                Some(ResolvedDmaBuf {
                    address: 0x3000_0000,
                    size: 768,
                })
            }),
            Err(MppError::UnsupportedGeometry)
        );
    }

    #[test]
    fn rejects_y_virtual_stride_smaller_than_y_plane() {
        let mut s = MppSession::new();
        let mut words = [0u32; REG_COUNT];
        words[registers::REG_DEC_OUT_BASE] = 11;
        words[registers::REG_SYS] = registers::SYS_FILL_DOWN;
        words[registers::REG_PIC_SIZE] = 15 | (15 << 16);
        words[registers::REG_PIC_FMT] = registers::JPEG_MODE_420;
        words[registers::REG_HOR_VIRSTRIDE] = 1 | (1 << 16);
        // Y needs 16 * 16 = 256 bytes, but the virtual stride is only 240.
        words[registers::REG_Y_VIRSTRIDE] = 15 << 4;
        s.set_reg_write(&words);

        assert_eq!(
            s.resolve_addresses(|_| {
                Some(ResolvedDmaBuf {
                    address: 0x3000_0000,
                    size: 384,
                })
            }),
            Err(MppError::UnsupportedGeometry)
        );
    }

    #[test]
    fn accepts_stream_range_with_nonzero_start_byte() {
        let mut s = MppSession::new();
        let mut words = [0u32; REG_COUNT];
        words[registers::REG_STRM_BASE] = 9;
        // One 16-byte DMA block, with the first coded byte at byte five inside
        // that block. The start marker is not an additional DMA byte count.
        words[registers::REG_STRM_LEN] = 5;
        s.set_reg_write(&words);
        s.add_reg_offsets(&[RegOffset {
            index: registers::REG_STRM_BASE as u32,
            offset: 4080,
        }])
        .unwrap();

        s.resolve_addresses(|_| {
            Some(ResolvedDmaBuf {
                address: 0x2000_0000,
                size: 4096,
            })
        })
        .unwrap();
    }

    #[test]
    fn clear_task_keeps_client_but_drops_regs() {
        let mut s = MppSession::new();
        s.init_client_type(MPP_DEVICE_RKJPEGD).unwrap();
        s.set_reg_write(&[5u32; REG_COUNT]);
        s.clear_task();
        assert_eq!(s.client_type(), Some(MPP_DEVICE_RKJPEGD));
        assert_eq!(s.regs()[0], 0);
        assert_eq!(
            s.resolve_addresses(|_| {
                Some(ResolvedDmaBuf {
                    address: 0,
                    size: 4096,
                })
            }),
            Err(MppError::NoRegisters)
        );
    }

    #[test]
    fn reset_clears_state() {
        let mut s = MppSession::new();
        s.init_client_type(MPP_DEVICE_RKJPEGD).unwrap();
        s.set_reg_write(&[1u32; REG_COUNT]);
        s.reset();
        assert_eq!(s.client_type(), None);
        assert_eq!(s.regs()[0], 0);
    }
}
