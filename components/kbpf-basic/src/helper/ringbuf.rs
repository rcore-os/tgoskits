use core::ffi::c_void;

use crate::{
    BpfError, BpfResult as Result, KernelAuxiliaryOps,
    map::{
        UnifiedMap,
        stream::{BpfDynPtr, BpfDynptrType, RingBufMap, ringbuf::RingBuf},
    },
};

bitflags::bitflags! {
    /// BPF_FUNC_bpf_ringbuf_commit, BPF_FUNC_bpf_ringbuf_discard, and
    /// BPF_FUNC_bpf_ringbuf_output flags.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct BpfRingbufFlags: u64 {
        const NO_WAKEUP = 1 << 0;
        const FORCE_WAKEUP = 1 << 1;
    }
}

/// Copy size bytes from data into a ring buffer ringbuf. If BPF_RB_NO_WAKEUP is specified in flags, no   
/// notification of new data availability is sent. If BPF_RB_FORCE_WAKEUP is specified in flags, notification of
/// new data availability is sent unconditionally. If 0 is specified in flags, an adaptive notification of new
/// data availability is sent.
///
/// An adaptive notification is a notification sent whenever the user-space process has caught up and consumed
/// all available payloads. In case the user-space process is still processing a previous payload, then no
/// notification is needed as it will process the newly added payload automatically.
///
/// See <https://docs.ebpf.io/linux/helper-function/bpf_ringbuf_output/>
pub fn raw_bpf_ringbuf_output<F: KernelAuxiliaryOps>(
    map: *mut c_void,
    data: *const u8,
    size: u64,
    flags: u64,
) -> i64 {
    let res = F::get_unified_map_from_ptr(map as *const u8, |unified_map| {
        let data = unsafe { core::slice::from_raw_parts(data, size as usize) };
        bpf_ringbuf_output::<F>(unified_map, data, flags)
    });
    match res {
        Ok(_) => 0,
        Err(e) => e as _,
    }
}

pub fn bpf_ringbuf_output<F: KernelAuxiliaryOps>(
    unified_map: &mut UnifiedMap,
    data: &[u8],
    flags: u64,
) -> Result<()> {
    let ringbuf_map = unified_map
        .map_mut()
        .as_any_mut()
        .downcast_mut::<RingBufMap<F>>()
        .ok_or(BpfError::EINVAL)?;
    let flags = BpfRingbufFlags::from_bits(flags).ok_or(BpfError::EINVAL)?;

    let data_buf = ringbuf_map
        .reserve(data.len() as u64)
        .map_err(|_| BpfError::EAGAIN)?;

    data_buf.copy_from_slice(data);

    RingBuf::<F>::commit(data_buf, flags, false)
}

/// Reserve size bytes of payload in a ring buffer ringbuf. flags must be 0.
///
/// See https://docs.ebpf.io/linux/helper-function/bpf_ringbuf_reserve/
pub fn raw_bpf_ringbuf_reserve<F: KernelAuxiliaryOps>(
    map: *mut c_void,
    size: u64,
    flags: u64,
) -> *mut u8 {
    if flags != 0 {
        return core::ptr::null_mut();
    }
    let res = F::get_unified_map_from_ptr(map as *const u8, |unified_map| {
        bpf_ringbuf_reserve::<F>(unified_map, size)
    });
    match res {
        Ok(ptr) => ptr,
        Err(_) => core::ptr::null_mut(),
    }
}

pub fn bpf_ringbuf_reserve<F: KernelAuxiliaryOps>(
    unified_map: &mut UnifiedMap,
    size: u64,
) -> Result<*mut u8> {
    let ringbuf_map = unified_map
        .map_mut()
        .as_any_mut()
        .downcast_mut::<RingBufMap<F>>()
        .ok_or(BpfError::EINVAL)?;
    ringbuf_map.reserve(size).map(|buf| buf.as_mut_ptr())
}

/// Submit reserved ring buffer sample, pointed to by data. If BPF_RB_NO_WAKEUP is specified in flags, no
/// notification of new data availability is sent. If BPF_RB_FORCE_WAKEUP is specified in flags, notification of
/// new data availability is sent unconditionally. If 0 is specified in flags, an adaptive notification of new
/// data availability is sent.
///
/// See <https://docs.ebpf.io/linux/helper-function/bpf_ringbuf_submit/>
pub fn raw_bpf_ringbuf_submit<F: KernelAuxiliaryOps>(sample: *const u8, flags: u64) -> i64 {
    let sample = unsafe { core::slice::from_raw_parts(sample, 1) };
    let res = bpf_ringbuf_submit::<F>(sample, flags);
    match res {
        Ok(_) => 0,
        Err(e) => e as _,
    }
}

pub fn bpf_ringbuf_submit<F: KernelAuxiliaryOps>(sample: &[u8], flags: u64) -> Result<()> {
    let flags = BpfRingbufFlags::from_bits(flags).ok_or(BpfError::EINVAL)?;
    RingBuf::<F>::commit(sample, flags, false)
}

/// Discard reserved ring buffer sample, pointed to by data. If BPF_RB_NO_WAKEUP is specified in flags, no
/// notification of new data availability is sent. If BPF_RB_FORCE_WAKEUP is specified in flags, notification of
/// new data availability is sent unconditionally. If 0 is specified in flags, an adaptive notification of new
/// data availability is sent.
///
/// See <https://docs.ebpf.io/linux/helper-function/bpf_ringbuf_discard/>
pub fn raw_bpf_ringbuf_discard<F: KernelAuxiliaryOps>(sample: *const u8, flags: u64) -> i64 {
    let sample = unsafe { core::slice::from_raw_parts(sample, 1) };
    let res = bpf_ringbuf_discard::<F>(sample, flags);
    match res {
        Ok(_) => 0,
        Err(e) => e as _,
    }
}

/// See [raw_bpf_ringbuf_discard]
pub fn bpf_ringbuf_discard<F: KernelAuxiliaryOps>(sample: &[u8], flags: u64) -> Result<()> {
    let flags = BpfRingbufFlags::from_bits(flags).ok_or(BpfError::EINVAL)?;
    RingBuf::<F>::commit(sample, flags, true)
}

/// BPF_FUNC_bpf_ringbuf_query flags
const BPF_RB_AVAIL_DATA: u64 = 0;
const BPF_RB_RING_SIZE: u64 = 1;
const BPF_RB_CONS_POS: u64 = 2;
const BPF_RB_PROD_POS: u64 = 3;

/// Query various characteristics of provided ring buffer. What exactly is queries is determined by flags:
/// - BPF_RB_AVAIL_DATA: Amount of data not yet consumed.
/// - BPF_RB_RING_SIZE: The size of ring buffer.
/// - BPF_RB_CONS_POS: Consumer position (can wrap around).
/// - BPF_RB_PROD_POS: Producer(s) position (can wrap around).
///
/// Data returned is just a momentary snapshot of actual values and could be inaccurate, so this facility should
/// be used to power heuristics and for reporting, not to make 100% correct calculation.
///
/// See <https://docs.ebpf.io/linux/helper-function/bpf_ringbuf_query/>
pub fn raw_bpf_ringbuf_query<F: KernelAuxiliaryOps>(map: *mut c_void, flags: u64) -> u64 {
    let res = F::get_unified_map_from_ptr(map as *const u8, |unified_map| {
        bpf_ringbuf_query::<F>(unified_map, flags)
    });
    res.unwrap_or_default()
}

pub fn bpf_ringbuf_query<F: KernelAuxiliaryOps>(
    unified_map: &mut UnifiedMap,
    flags: u64,
) -> Result<u64> {
    let ringbuf_map = unified_map
        .map()
        .as_any()
        .downcast_ref::<RingBufMap<F>>()
        .ok_or(BpfError::EINVAL)?;

    match flags {
        BPF_RB_AVAIL_DATA => Ok(ringbuf_map.avail_data_size()),
        BPF_RB_RING_SIZE => Ok(ringbuf_map.total_data_size()),
        BPF_RB_CONS_POS => Ok(ringbuf_map.consumer_pos()),
        BPF_RB_PROD_POS => Ok(ringbuf_map.producer_pos()),
        _ => Ok(0),
    }
}

fn bpf_dynptr_set_null(bpf_dyn_ptr: &mut BpfDynPtr) {
    *bpf_dyn_ptr = BpfDynPtr {
        data: core::ptr::null_mut(),
        size: 0,
        offset: 0,
    };
}

/// Reserve size bytes of payload in a ring buffer ringbuf through the dynptr interface. flags must be 0.
///
/// Please note that a corresponding bpf_ringbuf_submit_dynptr or bpf_ringbuf_discard_dynptr must be called
/// on ptr, even if the reservation fails. This is enforced by the verifier.
///
/// See <https://docs.ebpf.io/linux/helper-function/bpf_ringbuf_reserve_dynptr/>
pub fn raw_bpf_ringbuf_reserve_dynptr<F: KernelAuxiliaryOps>(
    map: *mut c_void,
    size: u32,
    flags: u64,
    bpf_dyn_ptr: *mut BpfDynPtr,
) -> i64 {
    let bpf_dyn_ptr = unsafe { &mut *bpf_dyn_ptr };
    if flags != 0 {
        bpf_dynptr_set_null(bpf_dyn_ptr);
        return BpfError::EINVAL as _;
    }

    let res = F::get_unified_map_from_ptr(map as *const u8, |unified_map| {
        bpf_ringbuf_reserve_dynptr::<F>(unified_map, size, bpf_dyn_ptr)
    });
    match res {
        Ok(_) => 0,
        Err(e) => e as _,
    }
}

pub fn bpf_ringbuf_reserve_dynptr<F: KernelAuxiliaryOps>(
    unified_map: &mut UnifiedMap,
    size: u32,
    bpf_dyn_ptr: &mut BpfDynPtr,
) -> Result<()> {
    let res = BpfDynPtr::check_size(size);

    let Ok(_) = res else {
        bpf_dynptr_set_null(bpf_dyn_ptr);
        return Err(BpfError::EINVAL);
    };

    let ringbuf_map = unified_map
        .map_mut()
        .as_any_mut()
        .downcast_mut::<RingBufMap<F>>()
        .ok_or(BpfError::EINVAL)?;

    let data_buf = ringbuf_map.reserve(size as u64);

    let Ok(data_buf) = data_buf else {
        bpf_dynptr_set_null(bpf_dyn_ptr);
        return Err(BpfError::EINVAL);
    };

    bpf_dyn_ptr.init(data_buf, BpfDynptrType::BPF_DYNPTR_TYPE_RINGBUF, 0, size);

    Ok(())
}

/// Submit reserved ring buffer sample, pointed to by data, through the dynptr interface. This is a no-op
/// if the dynptr is invalid/null.
///
/// For more information on flags, please see 'bpf_ringbuf_submit'.
///
/// See <https://docs.ebpf.io/linux/helper-function/bpf_ringbuf_submit_dynptr/>
pub fn raw_bpf_ringbuf_submit_dynptr<F: KernelAuxiliaryOps>(
    bpf_dyn_ptr: *mut BpfDynPtr,
    flags: u64,
) -> i64 {
    let bpf_dyn_ptr = unsafe { &mut *bpf_dyn_ptr };
    let res = bpf_ringbuf_submit_dynptr::<F>(bpf_dyn_ptr, flags);
    match res {
        Ok(_) => 0,
        Err(e) => e as _,
    }
}

pub fn bpf_ringbuf_submit_dynptr<F: KernelAuxiliaryOps>(
    bpf_dyn_ptr: &mut BpfDynPtr,
    flags: u64,
) -> Result<()> {
    if bpf_dyn_ptr.data.is_null() {
        return Ok(());
    }
    let data = bpf_dyn_ptr.data;
    // we don't care about size here, as the data was reserved before
    let sample = unsafe { core::slice::from_raw_parts(data, 1) };

    let flags = BpfRingbufFlags::from_bits(flags).ok_or(BpfError::EINVAL)?;
    RingBuf::<F>::commit(sample, flags, false)?;

    bpf_dynptr_set_null(bpf_dyn_ptr);
    Ok(())
}

/// Discard reserved ring buffer sample through the dynptr interface. This is a no-op if the dynptr is
/// invalid/null.
///
/// For more information on flags, please see [bpf_ringbuf_discard].
///
/// See <https://docs.ebpf.io/linux/helper-function/bpf_ringbuf_discard_dynptr/>
pub fn raw_bpf_ringbuf_discard_dynptr<F: KernelAuxiliaryOps>(
    bpf_dyn_ptr: *mut BpfDynPtr,
    flags: u64,
) -> i64 {
    let bpf_dyn_ptr = unsafe { &mut *bpf_dyn_ptr };
    let res = bpf_ringbuf_discard_dynptr::<F>(bpf_dyn_ptr, flags);
    match res {
        Ok(_) => 0,
        Err(e) => e as _,
    }
}

pub fn bpf_ringbuf_discard_dynptr<F: KernelAuxiliaryOps>(
    bpf_dyn_ptr: &mut BpfDynPtr,
    flags: u64,
) -> Result<()> {
    if bpf_dyn_ptr.data.is_null() {
        return Ok(());
    }
    let data = bpf_dyn_ptr.data;
    // we don't care about size here, as the data was reserved before
    let sample = unsafe { core::slice::from_raw_parts(data, 1) };

    let flags = BpfRingbufFlags::from_bits(flags).ok_or(BpfError::EINVAL)?;
    RingBuf::<F>::commit(sample, flags, true)?;

    bpf_dynptr_set_null(bpf_dyn_ptr);
    Ok(())
}
