//! Basic eBPF helper functions module.
pub(crate) mod ringbuf;
use alloc::{collections::btree_map::BTreeMap, string::String, vec::Vec};
use core::{
    ffi::{c_char, c_int, c_void},
    fmt::Write,
};

use consts::BPF_F_CURRENT_CPU;

use crate::{
    BpfError, BpfResult as Result, KernelAuxiliaryOps,
    map::{BpfCallBackFn, UnifiedMap},
};

pub mod consts;

/// Type alias for a raw BPF helper function.
pub type RawBPFHelperFn = fn(u64, u64, u64, u64, u64) -> u64;

/// Transmute a function pointer to a RawBPFHelperFn.
macro_rules! helper_func {
    ($name:ident::<$($generic:ident),*>) => {
        unsafe {
            core::mem::transmute::<usize, RawBPFHelperFn>($name::<$($generic),*> as *const () as usize)
        }
    };
    ($name:ident) => {
        unsafe {
            core::mem::transmute::<usize, RawBPFHelperFn>($name as *const () as usize)
        }
    };
}

// use printf_compat::{format, output};

/// Printf according to the format string, function will return the number of bytes written(including '\0')
///
/// # Safety
/// The caller must ensure that the format string and arguments are valid.
pub unsafe extern "C" fn printf(w: &mut impl Write, str: *const c_char, args: ...) -> c_int {
    // let bytes_written = unsafe { format(str as _, args, output::fmt_write(w)) };
    // bytes_written + 1
    0
}

fn extract_format_specifiers(format_str: &str) -> usize {
    // let mut result = Vec::new();
    let mut fmt_arg_count = 0;
    let chars: Vec<char> = format_str.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '%' {
            if i + 1 < chars.len() && chars[i + 1] == '%' {
                // Skip literal %%
                i += 2;
            } else {
                let start = i;
                i += 1;

                // Parse optional flags
                while i < chars.len() && "-+#0 .0123456789lhL*".contains(chars[i]) {
                    i += 1;
                }

                // Parse type specifier (a single letter)
                if i < chars.len() && "cdieEfFgGosuxXpn".contains(chars[i]) {
                    i += 1;
                    let _spec: String = chars[start..i].iter().collect();
                    // result.push(spec);
                    fmt_arg_count += 1; // Count this format specifier
                }
            }
        } else {
            i += 1;
        }
    }

    fmt_arg_count
}

/// See <https://ebpf-docs.dylanreimerink.nl/linux/helper-function/bpf_trace_printk/>
///
/// ## Warning
/// The `arg3`, `arg4`, and `arg5` parameters are pointers to the stack, so we first read the value
/// they point to before passing them to the `printf` function.
pub fn trace_printf<F: KernelAuxiliaryOps>(
    fmt_ptr: u64,
    fmt_len: u64,
    arg3: u64,
    arg4: u64,
    arg5: u64,
) -> i64 {
    struct FakeWriter<F: KernelAuxiliaryOps> {
        _phantom: core::marker::PhantomData<F>,
    }
    impl<F: KernelAuxiliaryOps> FakeWriter<F> {
        fn default() -> Self {
            FakeWriter {
                _phantom: core::marker::PhantomData,
            }
        }
    }
    impl<F: KernelAuxiliaryOps> Write for FakeWriter<F> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            F::ebpf_write_str(s).map_err(|_| core::fmt::Error)?;
            Ok(())
        }
    }

    let fmt_str = unsafe {
        core::str::from_utf8_unchecked(core::slice::from_raw_parts(
            fmt_ptr as *const u8,
            fmt_len as usize,
        ))
    };
    let fmt_arg_count = extract_format_specifiers(fmt_str);

    let (arg3, arg4, arg5) = match fmt_arg_count {
        0 => (0, 0, 0),
        1 => (unsafe { (arg3 as *const u64).read() }, 0, 0),
        2 => (
            unsafe { (arg3 as *const u64).read() },
            unsafe { (arg4 as *const u64).read() },
            0,
        ),
        3 => (
            unsafe { (arg3 as *const u64).read() },
            unsafe { (arg4 as *const u64).read() },
            unsafe { (arg5 as *const u64).read() },
        ),
        _ => {
            log::error!("trace_printf: too many arguments, only 3 are supported");
            return -1;
        }
    };

    let mut fmt = FakeWriter::<F>::default();
    unsafe { printf(&mut fmt, fmt_ptr as _, arg3, arg4, arg5) as _ }
}

/// See <https://ebpf-docs.dylanreimerink.nl/linux/helper-function/bpf_map_lookup_elem/>
pub fn raw_map_lookup_elem<F: KernelAuxiliaryOps>(
    map: *mut c_void,
    key: *const c_void,
) -> *const c_void {
    let res = F::get_unified_map_from_ptr(map as *const u8, |unified_map| {
        let meta = unified_map.map_meta();
        let key_size = meta.key_size as usize;
        let key = unsafe { core::slice::from_raw_parts(key as *const u8, key_size) };
        let value = map_lookup_elem(unified_map, key)?;
        Ok(value)
    });
    match res {
        Ok(Some(value)) => value as _,
        _ => core::ptr::null(),
    }
}

/// Lookup an element in map.
pub fn map_lookup_elem(unified_map: &mut UnifiedMap, key: &[u8]) -> Result<Option<*const u8>> {
    let map = unified_map.map_mut();
    let value = map.lookup_elem(key);
    match value {
        Ok(Some(value)) => Ok(Some(value.as_ptr())),
        _ => Ok(None),
    }
}

/// See https://ebpf-docs.dylanreimerink.nl/linux/helper-function/bpf_perf_event_output/
///
/// See https://man7.org/linux/man-pages/man7/bpf-helpers.7.html
pub fn raw_perf_event_output<F: KernelAuxiliaryOps>(
    ctx: *mut c_void,
    map: *mut c_void,
    flags: u64,
    data: *mut c_void,
    size: u64,
) -> i64 {
    let res = F::get_unified_map_from_ptr(map as *const u8, |unified_map| {
        let data = unsafe { core::slice::from_raw_parts(data as *const u8, size as usize) };
        perf_event_output::<F>(ctx, unified_map, flags, data)
    });

    match res {
        Ok(_) => 0,
        Err(e) => e as _,
    }
}

/// Output data to a perf event.
pub fn perf_event_output<F: KernelAuxiliaryOps>(
    ctx: *mut c_void,
    unified_map: &mut UnifiedMap,
    flags: u64,
    data: &[u8],
) -> Result<()> {
    let index = flags as u32;
    let flags = (flags >> 32) as u32;
    let key = if index == BPF_F_CURRENT_CPU as u32 {
        F::current_cpu_id()
    } else {
        index
    };
    let map = unified_map.map_mut();
    let fd = map
        .lookup_elem(&key.to_ne_bytes())?
        .ok_or(BpfError::ENOENT)?;
    let fd = u32::from_ne_bytes(fd.try_into().map_err(|_| BpfError::EINVAL)?);
    F::perf_event_output(ctx, fd, flags, data)?;
    Ok(())
}

/// See https://ebpf-docs.dylanreimerink.nl/linux/helper-function/bpf_probe_read/
fn raw_bpf_probe_read(dst: *mut c_void, size: u32, unsafe_ptr: *const c_void) -> i64 {
    let (dst, src) = unsafe {
        let dst = core::slice::from_raw_parts_mut(dst as *mut u8, size as usize);
        let src = core::slice::from_raw_parts(unsafe_ptr as *const u8, size as usize);
        (dst, src)
    };
    let res = bpf_probe_read(dst, src);
    match res {
        Ok(_) => 0,
        Err(e) => e as _,
    }
}

/// For tracing programs, safely attempt to read size
/// bytes from kernel space address unsafe_ptr and
/// store the data in dst.
pub fn bpf_probe_read(dst: &mut [u8], src: &[u8]) -> Result<()> {
    dst.copy_from_slice(src);
    Ok(())
}

/// Update entry with key in map.
///
/// See <https://docs.ebpf.io/linux/helper-function/bpf_map_update_elem/>
pub fn raw_map_update_elem<F: KernelAuxiliaryOps>(
    map: *mut c_void,
    key: *const c_void,
    value: *const c_void,
    flags: u64,
) -> i64 {
    let res = F::get_unified_map_from_ptr(map as *const u8, |unified_map| {
        let meta = unified_map.map_meta();
        let key_size = meta.key_size as usize;
        let value_size = meta.value_size as usize;
        let key = unsafe { core::slice::from_raw_parts(key as *const u8, key_size) };
        let value = unsafe { core::slice::from_raw_parts(value as *const u8, value_size) };
        map_update_elem(unified_map, key, value, flags)
    });
    match res {
        Ok(_) => 0,
        Err(e) => e as _,
    }
}

/// Update entry with key in map.
pub fn map_update_elem(
    unified_map: &mut UnifiedMap,
    key: &[u8],
    value: &[u8],
    flags: u64,
) -> Result<()> {
    let map = unified_map.map_mut();

    map.update_elem(key, value, flags)
}

/// Delete entry with key from map.
///
/// The delete map element helper call is used to delete values from maps.
pub fn raw_map_delete_elem<F: KernelAuxiliaryOps>(map: *mut c_void, key: *const c_void) -> i64 {
    let res = F::get_unified_map_from_ptr(map as *const u8, |unified_map| {
        let meta = unified_map.map_meta();
        let key_size = meta.key_size as usize;
        let key = unsafe { core::slice::from_raw_parts(key as *const u8, key_size) };
        map_delete_elem(unified_map, key)
    });
    match res {
        Ok(_) => 0,
        Err(e) => e as _,
    }
}

/// Delete entry with key from map.
pub fn map_delete_elem(unified_map: &mut UnifiedMap, key: &[u8]) -> Result<()> {
    let map = unified_map.map_mut();

    map.delete_elem(key)
}

/// For each element in map, call callback_fn function with map, callback_ctx and other map-specific
/// parameters. The callback_fn should be a static function and the callback_ctx should be a pointer
/// to the stack. The flags is used to control certain aspects of the helper.  Currently, the flags must
/// be 0.
///
/// The following are a list of supported map types and their respective expected callback signatures:
/// - BPF_MAP_TYPE_HASH
/// - BPF_MAP_TYPE_PERCPU_HASH
/// - BPF_MAP_TYPE_LRU_HASH
/// - BPF_MAP_TYPE_LRU_PERCPU_HASH
/// - BPF_MAP_TYPE_ARRAY
/// - BPF_MAP_TYPE_PERCPU_ARRAY
///
/// `long (*callback_fn)(struct bpf_map *map, const void key, void *value, void *ctx);`
///
/// For per_cpu maps, the map_value is the value on the cpu where the bpf_prog is running.
pub fn raw_map_for_each_elem<F: KernelAuxiliaryOps>(
    map: *mut c_void,
    cb: *const c_void,
    ctx: *const c_void,
    flags: u64,
) -> i64 {
    if cb.is_null() {
        return BpfError::EINVAL as _;
    }
    let res = F::get_unified_map_from_ptr(map as *const u8, |unified_map| {
        let cb = unsafe { *(cb as *const BpfCallBackFn) };
        map_for_each_elem(unified_map, cb, ctx as _, flags)
    });
    match res {
        Ok(v) => v as i64,
        Err(e) => e as _,
    }
}

/// Do some action for each element in map.
pub fn map_for_each_elem(
    unified_map: &mut UnifiedMap,
    cb: BpfCallBackFn,
    ctx: *const u8,
    flags: u64,
) -> Result<u32> {
    let map = unified_map.map_mut();

    map.for_each_elem(cb, ctx, flags)
}

/// Perform a lookup in percpu map for an entry associated to key on cpu.
///
/// See https://ebpf-docs.dylanreimerink.nl/linux/helper-function/bpf_map_lookup_percpu_elem/
pub fn raw_map_lookup_percpu_elem<F: KernelAuxiliaryOps>(
    map: *mut c_void,
    key: *const c_void,
    cpu: u32,
) -> *const c_void {
    let res = F::get_unified_map_from_ptr(map as *const u8, |unified_map| {
        let meta = unified_map.map_meta();
        let key_size = meta.key_size as usize;
        let key = unsafe { core::slice::from_raw_parts(key as *const u8, key_size) };
        map_lookup_percpu_elem(unified_map, key, cpu)
    });
    match res {
        Ok(Some(value)) => value as *const c_void,
        _ => core::ptr::null_mut(),
    }
}

/// Lookup an element in percpu map.
pub fn map_lookup_percpu_elem(
    unified_map: &mut UnifiedMap,
    key: &[u8],
    cpu: u32,
) -> Result<Option<*const u8>> {
    let map = unified_map.map_mut();
    let value = map.lookup_percpu_elem(key, cpu);
    match value {
        Ok(Some(value)) => Ok(Some(value.as_ptr())),
        _ => Ok(None),
    }
}
/// Push an element value in map.
///
/// See https://ebpf-docs.dylanreimerink.nl/linux/helper-function/bpf_map_push_elem/
pub fn raw_map_push_elem<F: KernelAuxiliaryOps>(
    map: *mut c_void,
    value: *const c_void,
    flags: u64,
) -> i64 {
    let res = F::get_unified_map_from_ptr(map as *const u8, |unified_map| {
        let meta = unified_map.map_meta();
        let value_size = meta.value_size as usize;
        let value = unsafe { core::slice::from_raw_parts(value as *const u8, value_size) };
        map_push_elem(unified_map, value, flags)
    });
    match res {
        Ok(_) => 0,
        Err(e) => e as _,
    }
}

/// Push an element value in map.
pub fn map_push_elem(unified_map: &mut UnifiedMap, value: &[u8], flags: u64) -> Result<()> {
    let map = unified_map.map_mut();

    map.push_elem(value, flags)
}

/// Pop an element from map.
///
/// See https://ebpf-docs.dylanreimerink.nl/linux/helper-function/bpf_map_pop_elem/
pub fn raw_map_pop_elem<F: KernelAuxiliaryOps>(map: *mut c_void, value: *mut c_void) -> i64 {
    let res = F::get_unified_map_from_ptr(map as *const u8, |unified_map| {
        let meta = unified_map.map_meta();
        let value_size = meta.value_size as usize;
        let value = unsafe { core::slice::from_raw_parts_mut(value as *mut u8, value_size) };
        map_pop_elem(unified_map, value)
    });
    match res {
        Ok(_) => 0,
        Err(e) => e as _,
    }
}

/// Pop an element from map.
pub fn map_pop_elem(unified_map: &mut UnifiedMap, value: &mut [u8]) -> Result<()> {
    let map = unified_map.map_mut();

    map.pop_elem(value)
}

/// Get an element from map without removing it.
///
/// See https://ebpf-docs.dylanreimerink.nl/linux/helper-function/bpf_map_peek_elem/
pub fn raw_map_peek_elem<F: KernelAuxiliaryOps>(map: *mut c_void, value: *mut c_void) -> i64 {
    let res = F::get_unified_map_from_ptr(map as *const u8, |unified_map| {
        let meta = unified_map.map_meta();
        let value_size = meta.value_size as usize;
        let value = unsafe { core::slice::from_raw_parts_mut(value as *mut u8, value_size) };
        map_peek_elem(unified_map, value)
    });
    match res {
        Ok(_) => 0,
        Err(e) => e as _,
    }
}

/// Get an element from map without removing it.
pub fn map_peek_elem(unified_map: &mut UnifiedMap, value: &mut [u8]) -> Result<()> {
    let map = unified_map.map_mut();

    map.peek_elem(value)
}

/// Get the current kernel time in nanoseconds.
pub fn bpf_ktime_get_ns<F: KernelAuxiliaryOps>() -> u64 {
    F::ebpf_time_ns().unwrap_or_default()
}

/// Copy a NULL terminated string from an unsafe user address unsafe_ptr to dst.
/// The size should include the terminating NULL byte. In case the string length is smaller than size,
/// the target is not padded with further NULL bytes. If the string length is larger than size,
/// just size-1 bytes are copied and the last byte is set to NULL.
///
/// On success, the strictly positive length of the output string, including the trailing NULL character. On error, a negative value
///
/// See https://docs.ebpf.io/linux/helper-function/bpf_probe_read_user_str/
fn raw_probe_read_user_str<F: KernelAuxiliaryOps>(
    dst: *mut c_void,
    size: u32,
    unsafe_ptr: *const c_void,
) -> i64 {
    let dst = unsafe { core::slice::from_raw_parts_mut(dst as *mut u8, size as usize) };
    let res = probe_read_user_str::<F>(dst, unsafe_ptr as *const u8);
    match res {
        Ok(len) => len as i64,
        Err(e) => e as _,
    }
}

/// Copy a NULL terminated string from an unsafe user address unsafe_ptr to dst.
pub fn probe_read_user_str<F: KernelAuxiliaryOps>(dst: &mut [u8], src: *const u8) -> Result<usize> {
    if dst.is_empty() {
        return Err(BpfError::EINVAL);
    }
    let str = F::string_from_user_cstr(src)?;
    let len = str.len();
    let copy_len = len.min(dst.len() - 1); // Leave space for NULL terminator
    dst[..copy_len].copy_from_slice(&str.as_bytes()[..copy_len]);
    dst[copy_len] = 0; // Null-terminate the string
    Ok(copy_len + 1) // Return length including NULL terminator
}

/// Initialize the helper functions map.
pub fn init_helper_functions<F: KernelAuxiliaryOps>() -> BTreeMap<u32, RawBPFHelperFn> {
    use consts::*;
    let mut map = BTreeMap::new();

    // Map helpers::Generic map helpers
    map.insert(
        HELPER_MAP_LOOKUP_ELEM,
        helper_func!(raw_map_lookup_elem::<F>),
    );
    map.insert(
        HELPER_MAP_UPDATE_ELEM,
        helper_func!(raw_map_update_elem::<F>),
    );
    map.insert(
        HELPER_MAP_DELETE_ELEM,
        helper_func!(raw_map_delete_elem::<F>),
    );
    map.insert(HELPER_KTIME_GET_NS, helper_func!(bpf_ktime_get_ns::<F>));
    map.insert(
        HELPER_MAP_FOR_EACH_ELEM,
        helper_func!(raw_map_for_each_elem::<F>),
    );
    map.insert(
        HELPER_MAP_LOOKUP_PERCPU_ELEM,
        helper_func!(raw_map_lookup_percpu_elem::<F>),
    );
    // map.insert(93,define_func!(raw_bpf_spin_lock);
    // map.insert(94,define_func!(raw_bpf_spin_unlock);
    // Map helpers::Perf event array helpers
    map.insert(
        HELPER_PERF_EVENT_OUTPUT,
        helper_func!(raw_perf_event_output::<F>),
    );
    // Probe and trace helpers::Memory helpers
    map.insert(HELPER_BPF_PROBE_READ, helper_func!(raw_bpf_probe_read));
    // Print helpers
    map.insert(HELPER_TRACE_PRINTF, helper_func!(trace_printf::<F>));

    // Map helpers::Queue and stack helpers
    map.insert(HELPER_MAP_PUSH_ELEM, helper_func!(raw_map_push_elem::<F>));
    map.insert(HELPER_MAP_POP_ELEM, helper_func!(raw_map_pop_elem::<F>));
    map.insert(HELPER_MAP_PEEK_ELEM, helper_func!(raw_map_peek_elem::<F>));

    // Map helpers::User space helpers
    map.insert(
        HELPER_PROBE_READ_USER_STR,
        helper_func!(raw_probe_read_user_str::<F>),
    );

    use ringbuf::*;
    // Ring Buffer helpers
    map.insert(
        HELPER_BPF_RINGBUF_OUTPUT,
        helper_func!(raw_bpf_ringbuf_output::<F>),
    );
    map.insert(
        HELPER_BPF_RINGBUF_RESERVE,
        helper_func!(raw_bpf_ringbuf_reserve::<F>),
    );
    map.insert(
        HELPER_BPF_RINGBUF_SUBMIT,
        helper_func!(raw_bpf_ringbuf_submit::<F>),
    );
    map.insert(
        HELPER_BPF_RINGBUF_DISCARD,
        helper_func!(raw_bpf_ringbuf_discard::<F>),
    );
    map.insert(
        HELPER_BPF_RINGBUF_QUERY,
        helper_func!(raw_bpf_ringbuf_query::<F>),
    );
    map.insert(
        HELPER_BPF_RINGBUF_RESERVE_DYNPTR,
        helper_func!(raw_bpf_ringbuf_reserve_dynptr::<F>),
    );
    map.insert(
        HELPER_BPF_RINGBUF_SUBMIT_DYNPTR,
        helper_func!(raw_bpf_ringbuf_submit_dynptr::<F>),
    );
    map.insert(
        HELPER_BPF_RINGBUF_DISCARD_DYNPTR,
        helper_func!(raw_bpf_ringbuf_discard_dynptr::<F>),
    );

    map
}
