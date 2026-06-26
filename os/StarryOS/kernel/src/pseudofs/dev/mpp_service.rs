//! `/dev/mpp_service` — a Rockchip MPP-compatible node for the RK3588 hardware
//! JPEG decoder.
//!
//! It speaks the subset of the `mpp_service` ioctl ABI that `librockchip_mpp`'s
//! JPEG-decode path uses, so unmodified MPP consumers (`mpi_dec_test -t 8`,
//! gstreamer `mppjpegdec`, ffmpeg `rkmpp`) can drive the decoder. We deliberately
//! expose no `/proc/mpp_service/support_cmd`, so MPP uses its old-kernel command
//! fallback (the exact command set implemented here).
//!
//! The wire ABI + register assembly live in `rockchip-jpeg`'s `mpp` module
//! (host-tested); this node only does `copy_from_user`/`copy_to_user`, dma-buf-fd
//! → physical-address resolution (via the /dev/dma_heap DmaBufFile), and the
//! hardware run.

use core::{any::Any, ffi::c_int, mem::size_of};

use ax_driver::jpeg::{self, mpp, registers};
use ax_runtime::hal::cpu::asm::user_copy;
use ax_sync::Mutex;
use axfs_ng_vfs::{DeviceId, VfsError, VfsResult};

use crate::{file::dmabuf::resolve_contiguous_dmabuf, pseudofs::DeviceOps};

fn copy_from_user(dst: *mut u8, src: *const u8, size: usize) -> VfsResult<()> {
    if unsafe { user_copy(dst, src, size) } != 0 {
        return Err(VfsError::InvalidData);
    }
    Ok(())
}

fn copy_to_user(dst: *mut u8, src: *const u8, size: usize) -> VfsResult<()> {
    if unsafe { user_copy(dst, src, size) } != 0 {
        return Err(VfsError::InvalidData);
    }
    Ok(())
}

/// Char-device id for `/dev/mpp_service` (opened by path; id is informational).
pub const MPP_SERVICE_DEVICE_ID: DeviceId = DeviceId::new(0xF1, 0x10);

/// Polled-completion ceiling. A small JPEG decodes in microseconds; this only
/// bounds the failure case (board completion IRQs may not fire).
const DECODE_TIMEOUT_NS: u64 = 100_000_000;

struct TaskState {
    session: mpp::MppSession,
    /// User pointer from `SET_REG_READ` to copy the register file back into.
    read_dst: usize,
}

/// The `/dev/mpp_service` device.
pub struct MppService {
    state: Mutex<TaskState>,
}

impl MppService {
    /// Create the device (one global session; MPP serializes one decode at a time).
    pub fn new() -> Self {
        Self {
            state: Mutex::new(TaskState {
                session: mpp::MppSession::new(),
                read_dst: 0,
            }),
        }
    }
}

impl Default for MppService {
    fn default() -> Self {
        Self::new()
    }
}

impl DeviceOps for MppService {
    fn read_at(&self, _buf: &mut [u8], _offset: u64) -> VfsResult<usize> {
        Ok(0)
    }

    fn write_at(&self, buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Ok(buf.len())
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        // Only the V1 request layout is implemented; V2 uses a different record
        // and must not be parsed as V1.
        if cmd != mpp::MPP_IOC_CFG_V1 {
            return Err(VfsError::NotATty);
        }
        if arg == 0 {
            return Err(VfsError::InvalidInput);
        }

        let mut state = self.state.lock();
        // Walk the chained request records (MULTI_MSG ... LAST_MSG).
        for i in 0..mpp::MAX_MSG_NUM {
            let req = read_request(arg + i * size_of::<mpp::MppRequest>())?;
            handle_request(&mut state, &req)?;
            if req.flag & mpp::flags::LAST_MSG != 0 || req.flag & mpp::flags::MULTI_MSG == 0 {
                break;
            }
        }
        Ok(0)
    }
}

fn read_request(uaddr: usize) -> VfsResult<mpp::MppRequest> {
    let mut req = mpp::MppRequest::default();
    copy_from_user(
        (&mut req) as *mut mpp::MppRequest as *mut u8,
        uaddr as *const u8,
        size_of::<mpp::MppRequest>(),
    )?;
    Ok(req)
}

fn write_u32_to_user(uaddr: usize, value: u32) -> VfsResult<()> {
    copy_to_user(
        uaddr as *mut u8,
        (&value) as *const u32 as *const u8,
        size_of::<u32>(),
    )
}

fn handle_request(state: &mut TaskState, req: &mpp::MppRequest) -> VfsResult<()> {
    let data = req.data_ptr as usize;
    match req.cmd {
        mpp::cmd::PROBE_HW_SUPPORT => {
            write_u32_to_user(data, mpp::HW_SUPPORT_JPEG_DEC)?;
        }
        mpp::cmd::QUERY_HW_ID => {
            write_u32_to_user(data, jpeg::read_id().unwrap_or(0))?;
        }
        mpp::cmd::INIT_CLIENT_TYPE => {
            let mut client: u32 = 0;
            copy_from_user(
                (&mut client) as *mut u32 as *mut u8,
                data as *const u8,
                size_of::<u32>(),
            )?;
            state
                .session
                .init_client_type(client)
                .map_err(|_| VfsError::InvalidInput)?;
        }
        mpp::cmd::SET_REG_WRITE => {
            let mut words = [0u32; registers::REG_COUNT];
            let n = (req.size as usize / 4).min(registers::REG_COUNT);
            copy_from_user(words.as_mut_ptr() as *mut u8, data as *const u8, n * 4)?;
            state.session.set_reg_write(&words[..n]);
        }
        mpp::cmd::SET_REG_READ => {
            state.read_dst = data;
            state.session.set_reg_read(req.offset, req.size);
        }
        mpp::cmd::SET_REG_ADDR_OFFSET => {
            let elem = size_of::<mpp::RegOffset>();
            let cnt = (req.size as usize / elem).min(mpp::MAX_REG_OFFSETS);
            let mut elems = [mpp::RegOffset::default(); mpp::MAX_REG_OFFSETS];
            copy_from_user(elems.as_mut_ptr() as *mut u8, data as *const u8, cnt * elem)?;
            state
                .session
                .add_reg_offsets(&elems[..cnt])
                .map_err(|_| VfsError::InvalidInput)?;
        }
        mpp::cmd::POLL_HW_FINISH | mpp::cmd::POLL_HW_IRQ => {
            run_decode(state)?;
        }
        mpp::cmd::RESET_SESSION => {
            state.session.reset();
            state.read_dst = 0;
        }
        // TRANS_FD_TO_IOVA / RELEASE_FD / others: fds are translated inline in the
        // register array at POLL time, so nothing to do here.
        _ => {}
    }
    Ok(())
}

fn run_decode(state: &mut TaskState) -> VfsResult<()> {
    state
        .session
        .resolve_addresses(resolve_fd)
        .map_err(|_| VfsError::InvalidInput)?;

    let mut readback = [0u32; registers::REG_COUNT];
    jpeg::run_raw(state.session.regs(), &mut readback, DECODE_TIMEOUT_NS).map_err(map_jpeg_err)?;

    // Copy the requested register window back to the SET_REG_READ destination.
    let (first, count) = state.session.read_window();
    if state.read_dst != 0 && count > 0 && first < registers::REG_COUNT {
        let count = count.min(registers::REG_COUNT - first);
        copy_to_user(
            state.read_dst as *mut u8,
            readback[first..].as_ptr() as *const u8,
            count * 4,
        )?;
    }

    state.session.clear_task();
    state.read_dst = 0;
    Ok(())
}

/// Resolve a dma-buf fd (as MPP places it in an address register) to the
/// physical base of its contiguous buffer. MPP allocates these from our
/// `/dev/dma_heap` ([`DmaBufFile`]).
fn resolve_fd(fd: u32) -> Option<u32> {
    let buf = resolve_contiguous_dmabuf(fd as c_int)?;
    // The decoder is 32-bit (device_with_mask(u32::MAX)); reject buffers above
    // 4 GiB rather than silently truncating the address.
    u32::try_from(buf.phys_base()).ok()
}

fn map_jpeg_err(err: jpeg::Error) -> VfsError {
    match err {
        jpeg::Error::Timeout => VfsError::TimedOut,
        _ => VfsError::InvalidData,
    }
}
