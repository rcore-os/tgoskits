#[cfg(feature = "multitask")]
use alloc::boxed::Box;
#[cfg(feature = "fs")]
use alloc::string::{String, ToString};
#[cfg(feature = "multitask")]
use alloc::sync::Arc;
use alloc::{collections::BTreeMap, vec::Vec};
use core::{
    alloc::Layout,
    ffi::{c_char, c_int, c_long, c_uint, c_void},
    mem::{align_of, size_of},
    ptr::{self, NonNull},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
#[cfg(feature = "multitask")]
use core::{sync::atomic::AtomicUsize, time::Duration};

use ax_errno::LinuxError;
use spin::{LazyLock, Mutex};

type SizeT = libc::size_t;
type SSizeT = libc::ssize_t;
type OffT = libc::off_t;
#[cfg(feature = "fs")]
type ModeT = libc::mode_t;
#[cfg(feature = "fs")]
type AxStat = ax_posix_api::ctypes::stat;

const MALLOC_ALIGN: usize = size_of::<usize>() * 2;
const CTRL_BLK_ALIGN: usize = align_of::<MemoryControlBlock>();
const FUTEX_WAIT: c_int = 0;
const FUTEX_WAKE: c_int = 1;
const FUTEX_WAIT_BITSET: c_int = 9;
const FUTEX_WAKE_BITSET: c_int = 10;
const FUTEX_CMD_MASK: c_int = !(libc::FUTEX_PRIVATE_FLAG | libc::FUTEX_CLOCK_REALTIME);
#[cfg(feature = "multitask")]
const FUTEX_BITSET_MATCH_ANY: u32 = u32::MAX;
#[cfg(feature = "multitask")]
const MAX_PTHREAD_KEYS: usize = 1024;
#[cfg(feature = "multitask")]
const PTHREAD_DESTRUCTOR_ITERATIONS: usize = 4;
#[cfg(feature = "multitask")]
const PTHREAD_COND_SIZE: usize = 48;
#[cfg(feature = "multitask")]
const PTHREAD_ATTR_SIZE: usize = 56;
#[cfg(feature = "multitask")]
const PTHREAD_ATTR_STACK_SIZE_OFFSET: usize = 8;
#[cfg(feature = "multitask")]
const DEFAULT_STACK_SIZE: usize = 2 * 1024 * 1024;
#[cfg(feature = "fs")]
const LINUX_DIRENT64_NAME_OFFSET: usize = 19;
#[cfg(feature = "fs")]
const LIBC_DIRENT64_NAME_OFFSET: usize = 19;
#[cfg(feature = "fs")]
const LIBC_DIRENT64_SIZE: usize = 280;

static FD_LAYER_READY: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy)]
struct CxaThreadDtor {
    dtor: unsafe extern "C" fn(*mut c_void),
    arg: *mut c_void,
}

unsafe impl Send for CxaThreadDtor {}

#[repr(C)]
#[derive(Clone, Copy)]
struct MemoryControlBlock {
    size: usize,
    allocation_size: usize,
    allocation_align: usize,
    base_offset: usize,
}

#[cfg(feature = "fs")]
struct DirStream {
    fd: c_int,
    entries: Vec<DirEntryBuf>,
    next: usize,
    current: [u8; LIBC_DIRENT64_SIZE],
}

#[cfg(feature = "fs")]
struct DirEntryBuf {
    name: Vec<u8>,
    d_type: u8,
}

#[cfg(feature = "fs")]
struct FdPath {
    path: String,
    is_dir: bool,
}

#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static mut errno: c_int = 0;

fn set_errno(code: i32) {
    unsafe {
        errno = code;
    }
}

fn ok_or_errno(ret: c_int) -> c_int {
    if ret < 0 {
        set_errno(ret.wrapping_neg());
        -1
    } else {
        ret
    }
}

fn ok_or_errno_isize(ret: isize) -> isize {
    if ret < 0 {
        set_errno(ret.wrapping_neg() as i32);
        -1
    } else {
        ret
    }
}

#[cfg(feature = "fs")]
fn ax_stat_to_libc_stat(src: AxStat) -> libc::stat {
    let mut dst = unsafe { core::mem::zeroed::<libc::stat>() };
    dst.st_dev = src.st_dev as _;
    dst.st_ino = src.st_ino as _;
    dst.st_mode = src.st_mode as _;
    dst.st_nlink = src.st_nlink as _;
    dst.st_uid = src.st_uid as _;
    dst.st_gid = src.st_gid as _;
    dst.st_rdev = src.st_rdev as _;
    dst.st_size = src.st_size as _;
    dst.st_blksize = src.st_blksize as _;
    dst.st_blocks = src.st_blocks as _;
    dst.st_atime = src.st_atime.tv_sec as _;
    dst.st_atime_nsec = src.st_atime.tv_nsec as _;
    dst.st_mtime = src.st_mtime.tv_sec as _;
    dst.st_mtime_nsec = src.st_mtime.tv_nsec as _;
    dst.st_ctime = src.st_ctime.tv_sec as _;
    dst.st_ctime_nsec = src.st_ctime.tv_nsec as _;
    dst
}

#[cfg(feature = "fs")]
fn write_libc_stat(buf: *mut libc::stat, stat: AxStat) -> c_int {
    if buf.is_null() {
        set_errno(LinuxError::EFAULT.code());
        -1
    } else {
        unsafe { buf.write(ax_stat_to_libc_stat(stat)) };
        0
    }
}

#[cfg(feature = "fs")]
fn stat_ret_to_libc(ret: c_int, stat: AxStat, buf: *mut libc::stat) -> c_int {
    if ret < 0 {
        ok_or_errno(ret)
    } else {
        write_libc_stat(buf, stat)
    }
}

fn fail(err: LinuxError) -> c_int {
    set_errno(err as i32);
    -1
}

fn is_stdio_fd(fd: c_int) -> bool {
    matches!(fd, libc::STDOUT_FILENO | libc::STDERR_FILENO)
}

fn early_stdio_write(fd: c_int, buf: *const c_void, count: SizeT) -> Option<SSizeT> {
    if !is_stdio_fd(fd) || FD_LAYER_READY.load(Ordering::Acquire) {
        return None;
    }
    if count == 0 {
        return Some(0);
    }
    if buf.is_null() && count != 0 {
        set_errno(LinuxError::EFAULT as i32);
        return Some(-1);
    }
    if count > isize::MAX as usize {
        set_errno(LinuxError::EINVAL as i32);
        return Some(-1);
    }

    let bytes = unsafe { core::slice::from_raw_parts(buf.cast::<u8>(), count) };
    ax_hal::console::write_text_bytes(bytes);
    Some(count as SSizeT)
}

fn early_stdio_writev(fd: c_int, iov: *const libc::iovec, iocnt: c_int) -> Option<SSizeT> {
    if !is_stdio_fd(fd) || FD_LAYER_READY.load(Ordering::Acquire) {
        return None;
    }
    if !(0..=1024).contains(&iocnt) {
        set_errno(LinuxError::EINVAL as i32);
        return Some(-1);
    }
    if iocnt == 0 {
        return Some(0);
    }
    if iov.is_null() && iocnt != 0 {
        set_errno(LinuxError::EFAULT as i32);
        return Some(-1);
    }

    let iovs = unsafe { core::slice::from_raw_parts(iov, iocnt as usize) };
    let mut written = 0usize;
    for iov in iovs {
        if iov.iov_len == 0 {
            continue;
        }
        if iov.iov_base.is_null() {
            set_errno(LinuxError::EFAULT as i32);
            return Some(-1);
        }
        let Some(next) = written.checked_add(iov.iov_len) else {
            set_errno(LinuxError::EINVAL as i32);
            return Some(-1);
        };
        if next > isize::MAX as usize {
            set_errno(LinuxError::EINVAL as i32);
            return Some(-1);
        }

        let bytes = unsafe { core::slice::from_raw_parts(iov.iov_base.cast::<u8>(), iov.iov_len) };
        ax_hal::console::write_text_bytes(bytes);
        written = next;
    }
    Some(written as SSizeT)
}

fn align_up_checked(addr: usize, align: usize) -> Option<usize> {
    debug_assert!(align.is_power_of_two());
    addr.checked_add(align - 1)
        .map(|value| value & !(align - 1))
}

fn alloc_with_alignment(size: SizeT, alignment: usize) -> Result<*mut c_void, LinuxError> {
    if !alignment.is_power_of_two() {
        return Err(LinuxError::EINVAL);
    }

    let size = size.max(1);
    let alignment = alignment.max(MALLOC_ALIGN);
    let Some(extra) = size_of::<MemoryControlBlock>().checked_add(alignment - 1) else {
        return Err(LinuxError::ENOMEM);
    };
    let Some(total) = size.checked_add(extra) else {
        return Err(LinuxError::ENOMEM);
    };
    let layout = Layout::from_size_align(total, CTRL_BLK_ALIGN).map_err(|_| LinuxError::EINVAL)?;
    let ptr = ax_alloc::global_allocator()
        .alloc(layout)
        .map_err(|_| LinuxError::ENOMEM)?;

    let base_addr = ptr.as_ptr() as usize;
    let Some(data_start) = base_addr.checked_add(size_of::<MemoryControlBlock>()) else {
        ax_alloc::global_allocator().dealloc(ptr, layout);
        return Err(LinuxError::ENOMEM);
    };
    let Some(user_addr) = align_up_checked(data_start, alignment) else {
        ax_alloc::global_allocator().dealloc(ptr, layout);
        return Err(LinuxError::ENOMEM);
    };
    let header_addr = user_addr - size_of::<MemoryControlBlock>();
    unsafe {
        (header_addr as *mut MemoryControlBlock).write(MemoryControlBlock {
            size,
            allocation_size: total,
            allocation_align: CTRL_BLK_ALIGN,
            base_offset: user_addr - base_addr,
        });
    }

    Ok(user_addr as *mut c_void)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __errno_location() -> *mut c_int {
    &raw mut errno
}

/// # Safety
///
/// Callers must pass a valid destructor function and argument according to the
/// Itanium C++ ABI used by Linux/musl.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __cxa_thread_atexit_impl(
    dtor: unsafe extern "C" fn(*mut c_void),
    arg: *mut c_void,
    _dso: *mut c_void,
) -> c_int {
    push_cxa_thread_dtor(CxaThreadDtor { dtor, arg });
    0
}

#[cfg(not(feature = "multitask"))]
static CXA_THREAD_DTORS: Mutex<Vec<CxaThreadDtor>> = Mutex::new(Vec::new());

#[cfg(not(feature = "multitask"))]
fn push_cxa_thread_dtor(record: CxaThreadDtor) {
    CXA_THREAD_DTORS.lock().push(record);
}

#[cfg(feature = "multitask")]
fn push_cxa_thread_dtor(record: CxaThreadDtor) {
    pthread::push_cxa_thread_dtor(record);
}

#[cfg(not(feature = "multitask"))]
fn run_cxa_thread_dtors() {
    while let Some(record) = { CXA_THREAD_DTORS.lock().pop() } {
        unsafe { (record.dtor)(record.arg) };
    }
}

#[cfg(feature = "multitask")]
fn run_cxa_thread_dtors() {
    pthread::run_cxa_thread_dtors();
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strerror(e: c_int) -> *mut c_char {
    #[allow(non_upper_case_globals)]
    static mut STRERROR_BUF: [u8; 256] = [0; 256];

    let err_str = if e == 0 {
        "Success"
    } else {
        LinuxError::try_from(e)
            .map(|err| err.as_str())
            .unwrap_or("Unknown error")
    };
    unsafe {
        let buf = &raw mut STRERROR_BUF;
        (*buf).fill(0);
        let bytes = err_str.as_bytes();
        let len = bytes.len().min((*buf).len() - 1);
        (&mut (*buf))[..len].copy_from_slice(&bytes[..len]);
        buf.cast::<c_char>()
    }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __xpg_strerror_r(errnum: c_int, buf: *mut c_char, buflen: SizeT) -> c_int {
    if buf.is_null() || buflen == 0 {
        return LinuxError::ERANGE as c_int;
    }
    let message = unsafe { strerror(errnum) };
    let mut len = unsafe { strlen(message) };
    if len >= buflen {
        len = buflen - 1;
    }
    unsafe {
        ptr::copy_nonoverlapping(message, buf, len);
        *buf.add(len) = 0;
    }
    0
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getenv(_name: *const c_char) -> *mut c_char {
    ptr::null_mut()
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn confstr(_name: c_int, _buf: *mut c_char, _len: SizeT) -> SizeT {
    set_errno(LinuxError::EINVAL as i32);
    0
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn malloc(size: SizeT) -> *mut c_void {
    match alloc_with_alignment(size, MALLOC_ALIGN) {
        Ok(ptr) => ptr,
        Err(err) => {
            set_errno(err as i32);
            ptr::null_mut()
        }
    }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn posix_memalign(
    memptr: *mut *mut c_void,
    alignment: SizeT,
    size: SizeT,
) -> c_int {
    if memptr.is_null() || !alignment.is_power_of_two() || alignment < size_of::<*mut c_void>() {
        return LinuxError::EINVAL as c_int;
    }
    match alloc_with_alignment(size, alignment) {
        Ok(ptr) => {
            unsafe {
                *memptr = ptr;
            }
            0
        }
        Err(err) => err as c_int,
    }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn calloc(nmemb: SizeT, size: SizeT) -> *mut c_void {
    let Some(total) = nmemb.checked_mul(size) else {
        set_errno(LinuxError::ENOMEM as i32);
        return ptr::null_mut();
    };
    let ptr = unsafe { malloc(total) };
    if !ptr.is_null() {
        unsafe { ptr::write_bytes(ptr, 0, total) };
    }
    ptr
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn realloc(ptr: *mut c_void, size: SizeT) -> *mut c_void {
    if ptr.is_null() {
        return unsafe { malloc(size) };
    }
    if size == 0 {
        unsafe { free(ptr) };
        return ptr::null_mut();
    }

    let old_block = unsafe { ptr.cast::<MemoryControlBlock>().sub(1) };
    let old_size = unsafe { (*old_block).size };
    let new_ptr = unsafe { malloc(size) };
    if !new_ptr.is_null() {
        unsafe {
            ptr::copy_nonoverlapping(ptr.cast::<u8>(), new_ptr.cast::<u8>(), old_size.min(size));
            free(ptr);
        }
    }
    new_ptr
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn free(ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    let block = unsafe { ptr.cast::<MemoryControlBlock>().sub(1) };
    let metadata = unsafe { *block };
    let base_addr = (ptr as usize).saturating_sub(metadata.base_offset);
    let layout =
        Layout::from_size_align(metadata.allocation_size, metadata.allocation_align).unwrap();
    let block = base_addr as *mut u8;
    if let Some(block) = NonNull::new(block.cast::<u8>()) {
        ax_alloc::global_allocator().dealloc(block, layout);
    }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcpy(dst: *mut c_void, src: *const c_void, n: SizeT) -> *mut c_void {
    let dst_u8 = dst.cast::<u8>();
    let src_u8 = src.cast::<u8>();
    for i in 0..n {
        unsafe { *dst_u8.add(i) = *src_u8.add(i) };
    }
    dst
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memmove(dst: *mut c_void, src: *const c_void, n: SizeT) -> *mut c_void {
    let dst_u8 = dst.cast::<u8>();
    let src_u8 = src.cast::<u8>();
    let dst_addr = dst_u8 as usize;
    let src_addr = src_u8 as usize;
    if dst_addr <= src_addr || dst_addr >= src_addr.saturating_add(n) {
        for i in 0..n {
            unsafe { *dst_u8.add(i) = *src_u8.add(i) };
        }
    } else {
        for i in (0..n).rev() {
            unsafe { *dst_u8.add(i) = *src_u8.add(i) };
        }
    }
    dst
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memset(dst: *mut c_void, value: c_int, n: SizeT) -> *mut c_void {
    let dst_u8 = dst.cast::<u8>();
    for i in 0..n {
        unsafe { *dst_u8.add(i) = value as u8 };
    }
    dst
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn memcmp(left: *const c_void, right: *const c_void, n: SizeT) -> c_int {
    let left = left.cast::<u8>();
    let right = right.cast::<u8>();
    for i in 0..n {
        let lhs = unsafe { *left.add(i) };
        let rhs = unsafe { *right.add(i) };
        if lhs != rhs {
            return lhs as c_int - rhs as c_int;
        }
    }
    0
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bcmp(left: *const c_void, right: *const c_void, n: SizeT) -> c_int {
    unsafe { memcmp(left, right, n) }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn strlen(s: *const c_char) -> SizeT {
    if s.is_null() {
        return 0;
    }
    let mut len = 0;
    while unsafe { *s.add(len) } != 0 {
        len += 1;
    }
    len
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn abort() -> ! {
    ax_api::sys::ax_terminate();
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn exit(exit_code: c_int) -> ! {
    run_cxa_thread_dtors();
    ax_posix_api::sys_exit(exit_code)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _exit(exit_code: c_int) -> ! {
    ax_posix_api::sys_exit(exit_code)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getpid() -> c_int {
    ok_or_errno(ax_posix_api::sys_getpid())
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sched_yield() -> c_int {
    ax_posix_api::sys_sched_yield()
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pause() -> c_int {
    fail(LinuxError::EINTR)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gettid() -> libc::pid_t {
    ax_posix_api::sys_getpid() as libc::pid_t
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sched_getaffinity(
    _pid: libc::pid_t,
    _cpusetsize: SizeT,
    _mask: *mut libc::cpu_set_t,
) -> c_int {
    fail(LinuxError::ENOSYS)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn read(fd: c_int, buf: *mut c_void, count: SizeT) -> SSizeT {
    ok_or_errno_isize(ax_posix_api::sys_read(fd, buf, count) as isize) as SSizeT
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn write(fd: c_int, buf: *const c_void, count: SizeT) -> SSizeT {
    if let Some(ret) = early_stdio_write(fd, buf, count) {
        return ret;
    }
    ok_or_errno_isize(ax_posix_api::sys_write(fd, buf, count) as isize) as SSizeT
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn writev(fd: c_int, iov: *const libc::iovec, iocnt: c_int) -> SSizeT {
    if let Some(ret) = early_stdio_writev(fd, iov, iocnt) {
        return ret;
    }
    ok_or_errno_isize(unsafe { ax_posix_api::sys_writev(fd, iov.cast(), iocnt) } as isize) as SSizeT
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fd")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn close(fd: c_int) -> c_int {
    #[cfg(feature = "fs")]
    FD_PATHS.lock().remove(&fd);
    ok_or_errno(ax_posix_api::sys_close(fd))
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(not(feature = "fd"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn close(_fd: c_int) -> c_int {
    fail(LinuxError::ENOSYS)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fd")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dup(old_fd: c_int) -> c_int {
    ok_or_errno(ax_posix_api::sys_dup(old_fd))
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(not(feature = "fd"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dup(_old_fd: c_int) -> c_int {
    fail(LinuxError::ENOSYS)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fd")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dup2(old_fd: c_int, new_fd: c_int) -> c_int {
    ok_or_errno(ax_posix_api::sys_dup2(old_fd, new_fd))
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(not(feature = "fd"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dup2(_old_fd: c_int, _new_fd: c_int) -> c_int {
    fail(LinuxError::ENOSYS)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dup3(old_fd: c_int, new_fd: c_int, flags: c_int) -> c_int {
    if old_fd == new_fd {
        return fail(LinuxError::EINVAL);
    }
    let new_fd = unsafe { dup2(old_fd, new_fd) };
    if new_fd >= 0 && flags & libc::O_CLOEXEC != 0 {
        let _ = unsafe { fcntl(new_fd, libc::F_SETFD, libc::FD_CLOEXEC as usize) };
    }
    new_fd
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fd")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcntl(fd: c_int, cmd: c_int, arg: usize) -> c_int {
    ok_or_errno(ax_posix_api::sys_fcntl(fd, cmd, arg))
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(not(feature = "fd"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcntl(_fd: c_int, _cmd: c_int, _arg: usize) -> c_int {
    fail(LinuxError::ENOSYS)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fd")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn poll(fds: *mut libc::pollfd, nfds: libc::nfds_t, timeout: c_int) -> c_int {
    ok_or_errno(ax_posix_api::sys_poll(fds.cast(), nfds as _, timeout))
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(not(feature = "fd"))]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn poll(
    _fds: *mut libc::pollfd,
    _nfds: libc::nfds_t,
    _timeout: c_int,
) -> c_int {
    fail(LinuxError::ENOSYS)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn isatty(_fd: c_int) -> c_int {
    fail(LinuxError::ENOTTY)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn open(path: *const c_char, flags: c_int, mode: ModeT) -> c_int {
    let path_string = ax_posix_api::utils::char_ptr_to_str(path)
        .ok()
        .map(ToString::to_string);
    let fd = ok_or_errno(ax_posix_api::sys_open(path, flags, mode));
    if fd >= 0
        && let Some(path) = path_string
    {
        track_fd_path(fd, path, flags & libc::O_DIRECTORY != 0);
    }
    fd
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn open64(path: *const c_char, flags: c_int, mode: ModeT) -> c_int {
    unsafe { open(path, flags, mode) }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lseek(fd: c_int, offset: OffT, whence: c_int) -> OffT {
    ok_or_errno_isize(ax_posix_api::sys_lseek(fd, offset, whence) as isize) as OffT
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lseek64(fd: c_int, offset: OffT, whence: c_int) -> OffT {
    unsafe { lseek(fd, offset, whence) }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stat(path: *const c_char, buf: *mut libc::stat) -> c_int {
    let mut ax_stat = AxStat::default();
    let ret = unsafe { ax_posix_api::sys_stat(path, &mut ax_stat) };
    stat_ret_to_libc(ret, ax_stat, buf)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn stat64(path: *const c_char, buf: *mut libc::stat64) -> c_int {
    unsafe { stat(path, buf.cast()) }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fstat(fd: c_int, buf: *mut libc::stat) -> c_int {
    let mut ax_stat = AxStat::default();
    let ret = unsafe { ax_posix_api::sys_fstat(fd, &mut ax_stat) };
    stat_ret_to_libc(ret, ax_stat, buf)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fstat64(fd: c_int, buf: *mut libc::stat64) -> c_int {
    unsafe { fstat(fd, buf.cast()) }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lstat(path: *const c_char, buf: *mut libc::stat) -> c_int {
    let mut ax_stat = AxStat::default();
    let ret = unsafe { ax_posix_api::sys_lstat(path, &mut ax_stat) };
    stat_ret_to_libc(ret as c_int, ax_stat, buf)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn lstat64(path: *const c_char, buf: *mut libc::stat64) -> c_int {
    unsafe { lstat(path, buf.cast()) }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getcwd(buf: *mut c_char, size: SizeT) -> *mut c_char {
    ax_posix_api::sys_getcwd(buf, size)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rename(old: *const c_char, new: *const c_char) -> c_int {
    ok_or_errno(ax_posix_api::sys_rename(old, new))
}

#[cfg(feature = "fs")]
fn path_from_c(path: *const c_char) -> Result<String, LinuxError> {
    Ok(ax_posix_api::utils::char_ptr_to_str(path)?.to_string())
}

#[cfg(feature = "fs")]
fn resolve_at_path(dirfd: c_int, path: *const c_char) -> Result<String, LinuxError> {
    let path = path_from_c(path)?;
    if path.starts_with('/') || dirfd == libc::AT_FDCWD {
        return Ok(path);
    }
    let paths = FD_PATHS.lock();
    let Some(parent) = paths.get(&dirfd) else {
        return Err(LinuxError::EBADF);
    };
    if !parent.is_dir {
        return Err(LinuxError::ENOTDIR);
    }
    Ok(join_paths(&parent.path, &path))
}

#[cfg(feature = "fs")]
fn join_paths(base: &str, child: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.is_empty() {
        alloc::format!("/{child}")
    } else {
        alloc::format!("{base}/{child}")
    }
}

#[cfg(feature = "fs")]
fn track_fd_path(fd: c_int, path: String, is_dir: bool) {
    FD_PATHS.lock().insert(fd, FdPath { path, is_dir });
}

#[cfg(feature = "fs")]
fn c_string_from_string(mut value: String) -> Vec<c_char> {
    value.retain(|ch| ch != '\0');
    let mut bytes = value.into_bytes();
    bytes.push(0);
    bytes.into_iter().map(|byte| byte as c_char).collect()
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn unlink(path: *const c_char) -> c_int {
    let path = match path_from_c(path) {
        Ok(path) => path,
        Err(err) => return fail(err),
    };
    match ax_api::fs::ax_remove_file(&path) {
        Ok(()) => 0,
        Err(err) if LinuxError::from(err) == LinuxError::EISDIR => ok_or_errno(
            ax_api::fs::ax_remove_dir(&path)
                .map_or_else(|err| -(LinuxError::from(err) as i32), |()| 0),
        ),
        Err(err) => fail(LinuxError::from(err)),
    }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn unlinkat(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int {
    let path = match resolve_at_path(dirfd, path) {
        Ok(path) => path,
        Err(err) => return fail(err),
    };
    if flags & libc::AT_REMOVEDIR != 0 {
        match ax_api::fs::ax_remove_dir(&path) {
            Ok(()) => 0,
            Err(err) => fail(LinuxError::from(err)),
        }
    } else {
        match ax_api::fs::ax_remove_file(&path) {
            Ok(()) => 0,
            Err(err) => fail(LinuxError::from(err)),
        }
    }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn openat(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
    mode: ModeT,
) -> c_int {
    let path = match resolve_at_path(dirfd, path) {
        Ok(path) => path,
        Err(err) => return fail(err),
    };
    let path = c_string_from_string(path);
    unsafe { open(path.as_ptr(), flags, mode) }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn openat64(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
    mode: ModeT,
) -> c_int {
    unsafe { openat(dirfd, path, flags, mode) }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mkdir(path: *const c_char, _mode: ModeT) -> c_int {
    let path = match path_from_c(path) {
        Ok(path) => path,
        Err(err) => return fail(err),
    };
    match ax_api::fs::ax_create_dir(&path) {
        Ok(()) => 0,
        Err(err) => fail(LinuxError::from(err)),
    }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mkdirat(dirfd: c_int, path: *const c_char, mode: ModeT) -> c_int {
    let path = match resolve_at_path(dirfd, path) {
        Ok(path) => path,
        Err(err) => return fail(err),
    };
    let path = c_string_from_string(path);
    unsafe { mkdir(path.as_ptr(), mode) }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rmdir(path: *const c_char) -> c_int {
    let path = match path_from_c(path) {
        Ok(path) => path,
        Err(err) => return fail(err),
    };
    match ax_api::fs::ax_remove_dir(&path) {
        Ok(()) => 0,
        Err(err) => fail(LinuxError::from(err)),
    }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chdir(path: *const c_char) -> c_int {
    let path = match path_from_c(path) {
        Ok(path) => path,
        Err(err) => return fail(err),
    };
    match ax_api::fs::ax_set_current_dir(&path) {
        Ok(()) => 0,
        Err(err) => fail(LinuxError::from(err)),
    }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getdents64(fd: c_int, buf: *mut c_void, len: SizeT) -> SSizeT {
    ok_or_errno_isize(unsafe { ax_posix_api::sys_getdents64(fd, buf.cast(), len) } as isize)
        as SSizeT
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn opendir(path: *const c_char) -> *mut libc::DIR {
    let fd = unsafe {
        open(
            path,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return ptr::null_mut();
    }
    unsafe { fdopendir(fd) }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdopendir(fd: c_int) -> *mut libc::DIR {
    let entries = match read_dir_entries(fd) {
        Ok(entries) => entries,
        Err(err) => {
            set_errno(err as i32);
            FD_PATHS.lock().remove(&fd);
            return ptr::null_mut();
        }
    };
    let stream = DirStream {
        fd,
        entries,
        next: 0,
        current: [0; LIBC_DIRENT64_SIZE],
    };
    alloc::boxed::Box::into_raw(alloc::boxed::Box::new(stream)).cast::<libc::DIR>()
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn readdir(dirp: *mut libc::DIR) -> *mut libc::dirent {
    unsafe { readdir64(dirp).cast::<libc::dirent>() }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn readdir64(dirp: *mut libc::DIR) -> *mut libc::dirent64 {
    if dirp.is_null() {
        set_errno(LinuxError::EINVAL as i32);
        return ptr::null_mut();
    }
    let stream = unsafe { &mut *dirp.cast::<DirStream>() };
    while let Some(entry) = stream.entries.get(stream.next) {
        stream.next += 1;
        if write_libc_dirent(&mut stream.current, entry, stream.next as i64) {
            return stream.current.as_mut_ptr().cast::<libc::dirent64>();
        }
    }
    ptr::null_mut()
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dirfd(dirp: *mut libc::DIR) -> c_int {
    if dirp.is_null() {
        return fail(LinuxError::EINVAL);
    }
    unsafe { (*dirp.cast::<DirStream>()).fd }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn closedir(dirp: *mut libc::DIR) -> c_int {
    if dirp.is_null() {
        return fail(LinuxError::EINVAL);
    }
    let stream = unsafe { alloc::boxed::Box::from_raw(dirp.cast::<DirStream>()) };
    FD_PATHS.lock().remove(&stream.fd);
    unsafe { close(stream.fd) }
}

#[cfg(feature = "fs")]
fn read_dir_entries(fd: c_int) -> Result<Vec<DirEntryBuf>, LinuxError> {
    let mut entries = Vec::new();
    let mut buf = [0_u8; 4096];
    loop {
        let ret = unsafe { ax_posix_api::sys_getdents64(fd, buf.as_mut_ptr(), buf.len()) };
        if ret < 0 {
            return Err(LinuxError::try_from((-ret) as i32).unwrap_or(LinuxError::EIO));
        }
        if ret == 0 {
            return Ok(entries);
        }
        let mut offset = 0;
        let used = ret as usize;
        while offset + LINUX_DIRENT64_NAME_OFFSET <= used {
            let reclen = u16::from_ne_bytes([buf[offset + 16], buf[offset + 17]]) as usize;
            if reclen == 0 || offset + reclen > used {
                return Err(LinuxError::EIO);
            }
            let d_type = buf[offset + 18];
            let name_start = offset + LINUX_DIRENT64_NAME_OFFSET;
            let name_end = buf[name_start..offset + reclen]
                .iter()
                .position(|&byte| byte == 0)
                .map(|pos| name_start + pos)
                .unwrap_or(offset + reclen);
            let name = &buf[name_start..name_end];
            if name != b"." && name != b".." {
                entries.push(DirEntryBuf {
                    name: name.to_vec(),
                    d_type,
                });
            }
            offset += reclen;
        }
    }
}

#[cfg(feature = "fs")]
fn write_libc_dirent(buf: &mut [u8; LIBC_DIRENT64_SIZE], entry: &DirEntryBuf, off: i64) -> bool {
    let name_len = entry
        .name
        .len()
        .min(LIBC_DIRENT64_SIZE - LIBC_DIRENT64_NAME_OFFSET - 1);
    let reclen = LIBC_DIRENT64_NAME_OFFSET + name_len + 1;
    buf.fill(0);
    unsafe {
        let ptr = buf.as_mut_ptr();
        ptr.cast::<u64>().write_unaligned(1);
        ptr.add(8).cast::<i64>().write_unaligned(off);
        ptr.add(16).cast::<u16>().write_unaligned(reclen as u16);
        ptr.add(18).write(entry.d_type);
        ptr.add(LIBC_DIRENT64_NAME_OFFSET)
            .copy_from_nonoverlapping(entry.name.as_ptr(), name_len);
    }
    true
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn chmod(_path: *const c_char, _mode: ModeT) -> c_int {
    0
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fchmod(_fd: c_int, _mode: ModeT) -> c_int {
    0
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ftruncate(_fd: c_int, _length: OffT) -> c_int {
    fail(LinuxError::ENOSYS)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ftruncate64(fd: c_int, length: OffT) -> c_int {
    unsafe { ftruncate(fd, length) }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fsync(_fd: c_int) -> c_int {
    0
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fdatasync(_fd: c_int) -> c_int {
    0
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn futimens(_fd: c_int, _times: *const libc::timespec) -> c_int {
    0
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn readlink(
    _path: *const c_char,
    _buf: *mut c_char,
    _bufsiz: SizeT,
) -> SSizeT {
    fail(LinuxError::EINVAL) as SSizeT
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn symlink(_target: *const c_char, _linkpath: *const c_char) -> c_int {
    fail(LinuxError::ENOSYS)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn link(_oldpath: *const c_char, _newpath: *const c_char) -> c_int {
    fail(LinuxError::ENOSYS)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[cfg(feature = "fs")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn linkat(
    _olddirfd: c_int,
    _oldpath: *const c_char,
    _newdirfd: c_int,
    _newpath: *const c_char,
    _flags: c_int,
) -> c_int {
    fail(LinuxError::ENOSYS)
}

#[cfg(not(feature = "fs"))]
mod fs_stubs {
    use super::*;

    type ModeT = libc::mode_t;

    macro_rules! fs_stub {
        ($(fn $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty => $body:block)*) => {
            $(
                /// # Safety
                ///
                /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
                #[unsafe(no_mangle)]
                pub unsafe extern "C" fn $name($($arg: $ty),*) -> $ret {
                    let _ = ($($arg,)*);
                    $body
                }
            )*
        };
    }

    fs_stub! {
        fn open(path: *const c_char, flags: c_int, mode: ModeT) -> c_int => { fail(LinuxError::ENOSYS) }
        fn open64(path: *const c_char, flags: c_int, mode: ModeT) -> c_int => { fail(LinuxError::ENOSYS) }
        fn lseek(fd: c_int, offset: OffT, whence: c_int) -> OffT => { fail(LinuxError::ENOSYS) as OffT }
        fn lseek64(fd: c_int, offset: OffT, whence: c_int) -> OffT => { fail(LinuxError::ENOSYS) as OffT }
        fn stat(path: *const c_char, buf: *mut libc::stat) -> c_int => { fail(LinuxError::ENOSYS) }
        fn stat64(path: *const c_char, buf: *mut libc::stat64) -> c_int => { fail(LinuxError::ENOSYS) }
        fn fstat(fd: c_int, buf: *mut libc::stat) -> c_int => { fail(LinuxError::ENOSYS) }
        fn fstat64(fd: c_int, buf: *mut libc::stat64) -> c_int => { fail(LinuxError::ENOSYS) }
        fn lstat(path: *const c_char, buf: *mut libc::stat) -> c_int => { fail(LinuxError::ENOSYS) }
        fn lstat64(path: *const c_char, buf: *mut libc::stat64) -> c_int => { fail(LinuxError::ENOSYS) }
        fn rename(old: *const c_char, new: *const c_char) -> c_int => { fail(LinuxError::ENOSYS) }
        fn unlink(path: *const c_char) -> c_int => { fail(LinuxError::ENOSYS) }
        fn unlinkat(dirfd: c_int, path: *const c_char, flags: c_int) -> c_int => { fail(LinuxError::ENOSYS) }
        fn openat(dirfd: c_int, path: *const c_char, flags: c_int, mode: ModeT) -> c_int => { fail(LinuxError::ENOSYS) }
        fn openat64(dirfd: c_int, path: *const c_char, flags: c_int, mode: ModeT) -> c_int => { fail(LinuxError::ENOSYS) }
        fn mkdir(path: *const c_char, mode: ModeT) -> c_int => { fail(LinuxError::ENOSYS) }
        fn mkdirat(dirfd: c_int, path: *const c_char, mode: ModeT) -> c_int => { fail(LinuxError::ENOSYS) }
        fn rmdir(path: *const c_char) -> c_int => { fail(LinuxError::ENOSYS) }
        fn chdir(path: *const c_char) -> c_int => { fail(LinuxError::ENOSYS) }
        fn getdents64(fd: c_int, buf: *mut c_void, len: SizeT) -> SSizeT => { fail(LinuxError::ENOSYS) as SSizeT }
        fn fdopendir(fd: c_int) -> *mut libc::DIR => {
            set_errno(LinuxError::ENOSYS as i32);
            ptr::null_mut()
        }
        fn readdir(dirp: *mut libc::DIR) -> *mut libc::dirent => {
            set_errno(LinuxError::ENOSYS as i32);
            ptr::null_mut()
        }
        fn readdir64(dirp: *mut libc::DIR) -> *mut libc::dirent64 => {
            set_errno(LinuxError::ENOSYS as i32);
            ptr::null_mut()
        }
        fn dirfd(dirp: *mut libc::DIR) -> c_int => { fail(LinuxError::ENOSYS) }
        fn closedir(dirp: *mut libc::DIR) -> c_int => { fail(LinuxError::ENOSYS) }
        fn chmod(path: *const c_char, mode: ModeT) -> c_int => { fail(LinuxError::ENOSYS) }
        fn fchmod(fd: c_int, mode: ModeT) -> c_int => { fail(LinuxError::ENOSYS) }
        fn ftruncate(fd: c_int, length: OffT) -> c_int => { fail(LinuxError::ENOSYS) }
        fn ftruncate64(fd: c_int, length: OffT) -> c_int => { fail(LinuxError::ENOSYS) }
        fn fsync(fd: c_int) -> c_int => { fail(LinuxError::ENOSYS) }
        fn fdatasync(fd: c_int) -> c_int => { fail(LinuxError::ENOSYS) }
        fn futimens(fd: c_int, times: *const libc::timespec) -> c_int => { fail(LinuxError::ENOSYS) }
        fn readlink(path: *const c_char, buf: *mut c_char, bufsiz: SizeT) -> SSizeT => { fail(LinuxError::ENOSYS) as SSizeT }
        fn symlink(target: *const c_char, linkpath: *const c_char) -> c_int => { fail(LinuxError::ENOSYS) }
        fn link(oldpath: *const c_char, newpath: *const c_char) -> c_int => { fail(LinuxError::ENOSYS) }
        fn linkat(olddirfd: c_int, oldpath: *const c_char, newdirfd: c_int, newpath: *const c_char, flags: c_int) -> c_int => { fail(LinuxError::ENOSYS) }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn getcwd(_buf: *mut c_char, _size: SizeT) -> *mut c_char {
        set_errno(LinuxError::ENOSYS as i32);
        ptr::null_mut()
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn opendir(_path: *const c_char) -> *mut libc::DIR {
        set_errno(LinuxError::ENOSYS as i32);
        ptr::null_mut()
    }
}

#[cfg(not(feature = "fs"))]
pub use fs_stubs::*;

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn clock_gettime(clk: libc::clockid_t, ts: *mut libc::timespec) -> c_int {
    ok_or_errno(unsafe { ax_posix_api::sys_clock_gettime(clk, ts.cast()) })
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn nanosleep(req: *const libc::timespec, rem: *mut libc::timespec) -> c_int {
    ok_or_errno(unsafe { ax_posix_api::sys_nanosleep(req.cast(), rem.cast()) })
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn clock_nanosleep(
    _clk: libc::clockid_t,
    flags: c_int,
    req: *const libc::timespec,
    rem: *mut libc::timespec,
) -> c_int {
    if flags != 0 {
        return LinuxError::ENOSYS as c_int;
    }
    if unsafe { nanosleep(req, rem) } == 0 {
        0
    } else {
        unsafe { errno }
    }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sysconf(name: c_int) -> c_long {
    ax_posix_api::sys_sysconf(name)
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getrandom(buf: *mut c_void, buflen: SizeT, _flags: c_uint) -> SSizeT {
    if buf.is_null() && buflen > 0 {
        return fail(LinuxError::EFAULT) as SSizeT;
    }
    fill_random(unsafe { core::slice::from_raw_parts_mut(buf.cast::<u8>(), buflen) });
    buflen as SSizeT
}

fn fill_random(buf: &mut [u8]) {
    static STATE: AtomicU64 = AtomicU64::new(0x9e37_79b9_7f4a_7c15);
    let mut seed = STATE
        .fetch_add(0xa076_1d64_78bd_642f, Ordering::Relaxed)
        .wrapping_add(ax_hal::time::monotonic_time_nanos());
    for byte in buf {
        seed ^= seed << 7;
        seed ^= seed >> 9;
        seed = seed.wrapping_mul(0xa24b_aed4_963e_e407);
        *byte = seed as u8;
    }
    STATE.store(seed, Ordering::Relaxed);
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getauxval(_ty: libc::c_ulong) -> libc::c_ulong {
    0
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn signal(_sig: c_int, handler: libc::sighandler_t) -> libc::sighandler_t {
    handler
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigaction(
    _signum: c_int,
    _act: *const libc::sigaction,
    oldact: *mut libc::sigaction,
) -> c_int {
    if !oldact.is_null() {
        unsafe { ptr::write_bytes(oldact, 0, 1) };
    }
    0
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sigaltstack(
    _ss: *const libc::stack_t,
    old_ss: *mut libc::stack_t,
) -> c_int {
    if !old_ss.is_null() {
        unsafe {
            old_ss.write(libc::stack_t {
                ss_sp: ptr::null_mut(),
                ss_flags: libc::SS_DISABLE,
                ss_size: 0,
            });
        }
    }
    0
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mmap(
    addr: *mut c_void,
    len: SizeT,
    _prot: c_int,
    flags: c_int,
    _fd: c_int,
    _offset: OffT,
) -> *mut c_void {
    if len == 0 {
        set_errno(LinuxError::EINVAL as i32);
        return libc::MAP_FAILED;
    }

    if flags & libc::MAP_FIXED != 0 && !addr.is_null() {
        return addr;
    }

    let ptr = unsafe { malloc(len) };
    if ptr.is_null() {
        return libc::MAP_FAILED;
    }
    MMAP_ALLOCS.lock().insert(ptr as usize, len);
    ptr
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mmap64(
    addr: *mut c_void,
    len: SizeT,
    prot: c_int,
    flags: c_int,
    fd: c_int,
    offset: libc::off64_t,
) -> *mut c_void {
    unsafe { mmap(addr, len, prot, flags, fd, offset as OffT) }
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn munmap(addr: *mut c_void, _len: SizeT) -> c_int {
    if addr.is_null() {
        return fail(LinuxError::EINVAL);
    }
    if MMAP_ALLOCS.lock().remove(&(addr as usize)).is_some() {
        unsafe { free(addr) };
    }
    0
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mprotect(_addr: *mut c_void, _len: SizeT, _prot: c_int) -> c_int {
    0
}

#[repr(C)]
pub struct UnwindContext {
    _private: [u8; 0],
}

type UnwindTraceFn = extern "C" fn(*mut UnwindContext, *mut c_void) -> c_int;

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Unwind_GetIP(_ctx: *mut UnwindContext) -> *const u8 {
    ptr::null()
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn _Unwind_Backtrace(
    _trace: UnwindTraceFn,
    _trace_argument: *mut c_void,
) -> c_int {
    5
}

/// # Safety
///
/// Callers must uphold the Linux/musl ABI contract for this libc symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn syscall(
    num: c_long,
    a0: c_long,
    a1: c_long,
    a2: c_long,
    a3: c_long,
    a4: c_long,
    a5: c_long,
) -> c_long {
    let _ = (a4, a5);
    match num {
        libc::SYS_futex => {
            (unsafe {
                futex_syscall(
                    a0 as *mut u32,
                    a1 as c_int,
                    a2 as u32,
                    a3 as *const libc::timespec,
                )
            }) as c_long
        }
        libc::SYS_getrandom => {
            (unsafe { getrandom(a0 as *mut c_void, a1 as usize, a2 as c_uint) }) as c_long
        }
        libc::SYS_gettid => unsafe { gettid() as c_long },
        #[cfg(feature = "fs")]
        libc::SYS_getdents64 => {
            (unsafe { getdents64(a0 as c_int, a1 as *mut c_void, a2 as usize) }) as c_long
        }
        _ => {
            set_errno(LinuxError::ENOSYS as i32);
            -1
        }
    }
}

unsafe fn futex_syscall(
    addr: *mut u32,
    op: c_int,
    expected: u32,
    timeout: *const libc::timespec,
) -> c_int {
    if addr.is_null() {
        return fail(LinuxError::EFAULT);
    }
    match op & FUTEX_CMD_MASK {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => unsafe { futex_wait(addr, expected, timeout) },
        FUTEX_WAKE | FUTEX_WAKE_BITSET => futex_wake(addr, expected),
        _ => fail(LinuxError::ENOSYS),
    }
}

unsafe fn futex_wait(addr: *mut u32, expected: u32, timeout: *const libc::timespec) -> c_int {
    #[cfg(not(feature = "multitask"))]
    {
        let _ = (addr, expected, timeout);
        fail(LinuxError::ENOSYS)
    }
    #[cfg(feature = "multitask")]
    {
        if unsafe { addr.read_volatile() } != expected {
            return fail(LinuxError::EAGAIN);
        }

        let key = addr as usize;
        let wq = {
            let mut map = FUTEX_QUEUES.lock();
            map.entry(key)
                .or_insert_with(|| Arc::new(ax_api::task::AxWaitQueueHandle::new()))
                .clone()
        };
        let timed_out = ax_api::task::ax_wait_queue_wait(&wq, unsafe { futex_timeout(timeout) });
        if timed_out {
            fail(LinuxError::ETIMEDOUT)
        } else {
            0
        }
    }
}

fn futex_wake(addr: *mut u32, count: u32) -> c_int {
    #[cfg(not(feature = "multitask"))]
    {
        let _ = (addr, count);
        fail(LinuxError::ENOSYS)
    }
    #[cfg(feature = "multitask")]
    {
        let Some(wq) = FUTEX_QUEUES.lock().get(&(addr as usize)).cloned() else {
            return 0;
        };
        let count = if count == FUTEX_BITSET_MATCH_ANY {
            u32::MAX
        } else {
            count
        };
        ax_api::task::ax_wait_queue_wake(&wq, count);
        count.min(i32::MAX as u32) as c_int
    }
}

#[cfg(feature = "multitask")]
unsafe fn futex_timeout(timeout: *const libc::timespec) -> Option<Duration> {
    if timeout.is_null() {
        return None;
    }
    let ts = unsafe { *timeout };
    if ts.tv_sec < 0 || ts.tv_nsec < 0 {
        return Some(Duration::ZERO);
    }
    let deadline = Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32);
    let now = ax_hal::time::monotonic_time();
    Some(deadline.saturating_sub(now))
}

#[cfg(feature = "multitask")]
static FUTEX_QUEUES: LazyLock<Mutex<BTreeMap<usize, Arc<ax_api::task::AxWaitQueueHandle>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
static MMAP_ALLOCS: LazyLock<Mutex<BTreeMap<usize, SizeT>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));
#[cfg(feature = "fs")]
static FD_PATHS: LazyLock<Mutex<BTreeMap<c_int, FdPath>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

#[cfg(feature = "multitask")]
mod pthread {
    use super::*;

    type PthreadTlsValues = [*mut c_void; MAX_PTHREAD_KEYS];
    type PthreadTlsMap = BTreeMap<u64, ForceSendSync<PthreadTlsValues>>;
    type CxaThreadDtorMap = BTreeMap<u64, Vec<CxaThreadDtor>>;

    static KEY_SLOTS: LazyLock<Mutex<Vec<Option<TlsKey>>>> =
        LazyLock::new(|| Mutex::new(Vec::new()));
    static TLS_VALUES: LazyLock<Mutex<PthreadTlsMap>> =
        LazyLock::new(|| Mutex::new(BTreeMap::new()));
    static CXA_THREAD_DTORS: LazyLock<Mutex<CxaThreadDtorMap>> =
        LazyLock::new(|| Mutex::new(BTreeMap::new()));
    static NEXT_COND_ID: AtomicUsize = AtomicUsize::new(1);
    static CONDVARS: LazyLock<Mutex<BTreeMap<usize, Arc<ax_api::task::AxWaitQueueHandle>>>> =
        LazyLock::new(|| Mutex::new(BTreeMap::new()));

    struct TlsKey {
        destructor: Option<unsafe extern "C" fn(*mut c_void)>,
    }

    #[derive(Clone, Copy)]
    struct ForceSendSync<T>(T);

    unsafe impl<T> Send for ForceSendSync<T> {}
    unsafe impl<T> Sync for ForceSendSync<T> {}

    struct PthreadStart {
        start: extern "C" fn(*mut c_void) -> *mut c_void,
        arg: *mut c_void,
    }

    extern "C" fn pthread_start_trampoline(arg: *mut c_void) -> *mut c_void {
        let start = unsafe { Box::from_raw(arg.cast::<PthreadStart>()) };
        let ret = (start.start)(start.arg);
        super::run_cxa_thread_dtors();
        ret
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_self() -> libc::pthread_t {
        ax_posix_api::sys_pthread_self() as libc::pthread_t
    }

    fn current_task_key() -> u64 {
        ax_api::task::ax_current_task_id()
    }

    pub(super) fn push_cxa_thread_dtor(record: CxaThreadDtor) {
        CXA_THREAD_DTORS
            .lock()
            .entry(current_task_key())
            .or_default()
            .push(record);
    }

    pub(super) fn run_cxa_thread_dtors() {
        let task = current_task_key();
        let mut records = CXA_THREAD_DTORS.lock().remove(&task).unwrap_or_default();
        while let Some(record) = records.pop() {
            unsafe { (record.dtor)(record.arg) };
        }
        run_pthread_key_dtors(task);
        TLS_VALUES.lock().remove(&task);
    }

    fn run_pthread_key_dtors(task: u64) {
        for _ in 0..PTHREAD_DESTRUCTOR_ITERATIONS {
            let mut records = Vec::new();
            {
                let keys = KEY_SLOTS.lock();
                let mut tls_values = TLS_VALUES.lock();
                let Some(values) = tls_values.get_mut(&task) else {
                    break;
                };
                for (key, slot) in keys.iter().enumerate() {
                    let Some(destructor) = slot.as_ref().and_then(|slot| slot.destructor) else {
                        continue;
                    };
                    let value = values.0[key];
                    if !value.is_null() {
                        values.0[key] = ptr::null_mut();
                        records.push((destructor, value));
                    }
                }
            }

            if records.is_empty() {
                break;
            }

            for (destructor, value) in records {
                unsafe { destructor(value) };
            }
        }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_create(
        res: *mut libc::pthread_t,
        attr: *const libc::pthread_attr_t,
        start: extern "C" fn(*mut c_void) -> *mut c_void,
        arg: *mut c_void,
    ) -> c_int {
        let start = Box::into_raw(Box::new(PthreadStart { start, arg }));
        let ret = unsafe {
            ax_posix_api::sys_pthread_create(
                res.cast(),
                attr.cast(),
                pthread_start_trampoline,
                start.cast(),
            )
        };
        if ret != 0 {
            unsafe { drop(Box::from_raw(start)) };
        }
        ret
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_exit(retval: *mut c_void) -> ! {
        super::run_cxa_thread_dtors();
        ax_posix_api::sys_pthread_exit(retval)
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_join(
        thread: libc::pthread_t,
        retval: *mut *mut c_void,
    ) -> c_int {
        unsafe { ax_posix_api::sys_pthread_join(thread as _, retval) }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_detach(_thread: libc::pthread_t) -> c_int {
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_getattr_np(
        _thread: libc::pthread_t,
        _attr: *mut libc::pthread_attr_t,
    ) -> c_int {
        LinuxError::ENOSYS as c_int
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_attr_init(attr: *mut libc::pthread_attr_t) -> c_int {
        if attr.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        unsafe {
            ptr::write_bytes(attr.cast::<u8>(), 0, PTHREAD_ATTR_SIZE);
            write_attr_stack_size(attr, DEFAULT_STACK_SIZE);
        }
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_attr_destroy(_attr: *mut libc::pthread_attr_t) -> c_int {
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_attr_getstack(
        _attr: *const libc::pthread_attr_t,
        stack_addr: *mut *mut c_void,
        stack_size: *mut SizeT,
    ) -> c_int {
        if stack_addr.is_null() || stack_size.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        unsafe {
            stack_addr.write(ptr::null_mut());
            stack_size.write(0);
        }
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_attr_getguardsize(
        _attr: *const libc::pthread_attr_t,
        guard_size: *mut SizeT,
    ) -> c_int {
        if guard_size.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        unsafe {
            guard_size.write(0);
        }
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_attr_setstacksize(
        attr: *mut libc::pthread_attr_t,
        stack_size: SizeT,
    ) -> c_int {
        if attr.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        unsafe { write_attr_stack_size(attr, stack_size) };
        0
    }

    unsafe fn write_attr_stack_size(attr: *mut libc::pthread_attr_t, stack_size: SizeT) {
        unsafe {
            let bytes = attr.cast::<u8>().add(PTHREAD_ATTR_STACK_SIZE_OFFSET);
            ptr::write_unaligned(bytes.cast::<SizeT>(), stack_size);
        }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_mutex_init(
        mutex: *mut libc::pthread_mutex_t,
        attr: *const libc::pthread_mutexattr_t,
    ) -> c_int {
        ax_posix_api::sys_pthread_mutex_init(mutex.cast(), attr.cast())
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_mutex_lock(mutex: *mut libc::pthread_mutex_t) -> c_int {
        ax_posix_api::sys_pthread_mutex_lock(mutex.cast())
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_mutex_trylock(mutex: *mut libc::pthread_mutex_t) -> c_int {
        ax_posix_api::sys_pthread_mutex_trylock(mutex.cast())
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_mutex_unlock(mutex: *mut libc::pthread_mutex_t) -> c_int {
        ax_posix_api::sys_pthread_mutex_unlock(mutex.cast())
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_mutex_destroy(mutex: *mut libc::pthread_mutex_t) -> c_int {
        ax_posix_api::sys_pthread_mutex_destroy(mutex.cast())
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_mutexattr_init(attr: *mut libc::pthread_mutexattr_t) -> c_int {
        if attr.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        unsafe { ptr::write_bytes(attr.cast::<u8>(), 0, size_of::<libc::pthread_mutexattr_t>()) };
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_mutexattr_settype(
        _attr: *mut libc::pthread_mutexattr_t,
        _ty: c_int,
    ) -> c_int {
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_mutexattr_destroy(
        _attr: *mut libc::pthread_mutexattr_t,
    ) -> c_int {
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_cond_init(
        cond: *mut libc::pthread_cond_t,
        _attr: *const libc::pthread_condattr_t,
    ) -> c_int {
        if cond.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        let id = NEXT_COND_ID.fetch_add(1, Ordering::Relaxed).max(1);
        unsafe {
            ptr::write_bytes(cond.cast::<u8>(), 0, PTHREAD_COND_SIZE);
            ptr::write_unaligned(cond.cast::<usize>(), id);
        }
        CONDVARS
            .lock()
            .insert(id, Arc::new(ax_api::task::AxWaitQueueHandle::new()));
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_cond_signal(cond: *mut libc::pthread_cond_t) -> c_int {
        if let Some(wq) = unsafe { cond_wait_queue(cond) } {
            ax_api::task::ax_wait_queue_wake(&wq, 1);
        }
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_cond_broadcast(cond: *mut libc::pthread_cond_t) -> c_int {
        if let Some(wq) = unsafe { cond_wait_queue(cond) } {
            ax_api::task::ax_wait_queue_wake(&wq, u32::MAX);
        }
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_cond_wait(
        cond: *mut libc::pthread_cond_t,
        mutex: *mut libc::pthread_mutex_t,
    ) -> c_int {
        unsafe { pthread_cond_wait_inner(cond, mutex, None) }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_cond_timedwait(
        cond: *mut libc::pthread_cond_t,
        mutex: *mut libc::pthread_mutex_t,
        abstime: *const libc::timespec,
    ) -> c_int {
        unsafe { pthread_cond_wait_inner(cond, mutex, super::futex_timeout(abstime)) }
    }

    unsafe fn pthread_cond_wait_inner(
        cond: *mut libc::pthread_cond_t,
        mutex: *mut libc::pthread_mutex_t,
        timeout: Option<Duration>,
    ) -> c_int {
        let Some(wq) = (unsafe { cond_wait_queue(cond) }) else {
            return LinuxError::EINVAL as c_int;
        };
        let unlock_ret = unsafe { pthread_mutex_unlock(mutex) };
        if unlock_ret != 0 {
            return unlock_ret;
        }
        let timed_out = ax_api::task::ax_wait_queue_wait(&wq, timeout);
        let lock_ret = unsafe { pthread_mutex_lock(mutex) };
        if timed_out {
            LinuxError::ETIMEDOUT as c_int
        } else {
            lock_ret
        }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_cond_destroy(cond: *mut libc::pthread_cond_t) -> c_int {
        if cond.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        let id = unsafe { ptr::read_unaligned(cond.cast::<usize>()) };
        CONDVARS.lock().remove(&id);
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_condattr_init(attr: *mut libc::pthread_condattr_t) -> c_int {
        if attr.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        unsafe { ptr::write_bytes(attr.cast::<u8>(), 0, size_of::<libc::pthread_condattr_t>()) };
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_condattr_setclock(
        _attr: *mut libc::pthread_condattr_t,
        _clock: libc::clockid_t,
    ) -> c_int {
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_condattr_destroy(
        _attr: *mut libc::pthread_condattr_t,
    ) -> c_int {
        0
    }

    unsafe fn cond_wait_queue(
        cond: *mut libc::pthread_cond_t,
    ) -> Option<Arc<ax_api::task::AxWaitQueueHandle>> {
        if cond.is_null() {
            return None;
        }
        let mut id = unsafe { ptr::read_unaligned(cond.cast::<usize>()) };
        if id == 0 {
            let ret = unsafe { pthread_cond_init(cond, ptr::null()) };
            if ret != 0 {
                return None;
            }
            id = unsafe { ptr::read_unaligned(cond.cast::<usize>()) };
        }
        CONDVARS.lock().get(&id).cloned()
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_key_create(
        key: *mut libc::pthread_key_t,
        destructor: Option<unsafe extern "C" fn(*mut c_void)>,
    ) -> c_int {
        if key.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        let mut keys = KEY_SLOTS.lock();
        let index = if let Some((index, slot)) =
            keys.iter_mut().enumerate().find(|(_, slot)| slot.is_none())
        {
            *slot = Some(TlsKey { destructor });
            index
        } else {
            if keys.len() >= MAX_PTHREAD_KEYS {
                return LinuxError::EAGAIN as c_int;
            }
            keys.push(Some(TlsKey { destructor }));
            keys.len() - 1
        };
        unsafe { key.write(index as libc::pthread_key_t) };
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_key_delete(key: libc::pthread_key_t) -> c_int {
        let mut keys = KEY_SLOTS.lock();
        let Some(slot) = keys.get_mut(key as usize) else {
            return LinuxError::EINVAL as c_int;
        };
        *slot = None;
        for values in TLS_VALUES.lock().values_mut() {
            values.0[key as usize] = ptr::null_mut();
        }
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_getspecific(key: libc::pthread_key_t) -> *mut c_void {
        if key as usize >= MAX_PTHREAD_KEYS {
            return ptr::null_mut();
        }
        TLS_VALUES
            .lock()
            .get(&current_task_key())
            .map(|values| values.0[key as usize])
            .unwrap_or(ptr::null_mut())
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_setspecific(
        key: libc::pthread_key_t,
        value: *const c_void,
    ) -> c_int {
        let keys = KEY_SLOTS.lock();
        if keys
            .get(key as usize)
            .and_then(|slot| slot.as_ref())
            .is_none()
        {
            return LinuxError::EINVAL as c_int;
        }
        let mut tls_values = TLS_VALUES.lock();
        tls_values
            .entry(current_task_key())
            .or_insert_with(|| ForceSendSync([ptr::null_mut(); MAX_PTHREAD_KEYS]))
            .0[key as usize] = value.cast_mut();
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_setname_np(
        _thread: libc::pthread_t,
        _name: *const c_char,
    ) -> c_int {
        0
    }
}

#[cfg(feature = "multitask")]
pub use pthread::*;

#[cfg(not(feature = "multitask"))]
mod pthread_stubs {
    use super::*;

    macro_rules! pthread_errno_stub {
        ($(fn $name:ident($($arg:ident: $ty:ty),*) -> c_int;)*) => {
            $(
                /// # Safety
                ///
                /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
                #[unsafe(no_mangle)]
                pub unsafe extern "C" fn $name($($arg: $ty),*) -> c_int {
                    let _ = ($($arg,)*);
                    LinuxError::ENOSYS as c_int
                }
            )*
        };
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[cfg(target_env = "musl")]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_self() -> libc::pthread_t {
        ptr::dangling_mut::<c_void>().cast()
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/glibc ABI contract for this libc symbol.
    #[cfg(not(target_env = "musl"))]
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_self() -> libc::pthread_t {
        1
    }

    pthread_errno_stub! {
        fn pthread_create(
            res: *mut libc::pthread_t,
            attr: *const libc::pthread_attr_t,
            start: extern "C" fn(*mut c_void) -> *mut c_void,
            arg: *mut c_void
        ) -> c_int;
        fn pthread_join(thread: libc::pthread_t, retval: *mut *mut c_void) -> c_int;
        fn pthread_getattr_np(thread: libc::pthread_t, attr: *mut libc::pthread_attr_t) -> c_int;
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_exit(_retval: *mut c_void) -> ! {
        unsafe { exit(0) }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_detach(_thread: libc::pthread_t) -> c_int {
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_attr_init(attr: *mut libc::pthread_attr_t) -> c_int {
        if attr.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        unsafe { ptr::write_bytes(attr.cast::<u8>(), 0, size_of::<libc::pthread_attr_t>()) };
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_attr_destroy(_attr: *mut libc::pthread_attr_t) -> c_int {
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_attr_getstack(
        _attr: *const libc::pthread_attr_t,
        stack_addr: *mut *mut c_void,
        stack_size: *mut SizeT,
    ) -> c_int {
        if stack_addr.is_null() || stack_size.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        unsafe {
            stack_addr.write(ptr::null_mut());
            stack_size.write(0);
        }
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_attr_getguardsize(
        _attr: *const libc::pthread_attr_t,
        guard_size: *mut SizeT,
    ) -> c_int {
        if guard_size.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        unsafe { guard_size.write(0) };
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_attr_setstacksize(
        _attr: *mut libc::pthread_attr_t,
        _stack_size: SizeT,
    ) -> c_int {
        0
    }

    pthread_errno_stub! {
        fn pthread_mutex_init(
            mutex: *mut libc::pthread_mutex_t,
            attr: *const libc::pthread_mutexattr_t
        ) -> c_int;
        fn pthread_mutex_lock(mutex: *mut libc::pthread_mutex_t) -> c_int;
        fn pthread_mutex_trylock(mutex: *mut libc::pthread_mutex_t) -> c_int;
        fn pthread_mutex_unlock(mutex: *mut libc::pthread_mutex_t) -> c_int;
        fn pthread_cond_wait(
            cond: *mut libc::pthread_cond_t,
            mutex: *mut libc::pthread_mutex_t
        ) -> c_int;
        fn pthread_cond_timedwait(
            cond: *mut libc::pthread_cond_t,
            mutex: *mut libc::pthread_mutex_t,
            abstime: *const libc::timespec
        ) -> c_int;
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_mutex_destroy(_mutex: *mut libc::pthread_mutex_t) -> c_int {
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_mutexattr_init(attr: *mut libc::pthread_mutexattr_t) -> c_int {
        if attr.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        unsafe { ptr::write_bytes(attr.cast::<u8>(), 0, size_of::<libc::pthread_mutexattr_t>()) };
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_mutexattr_settype(
        _attr: *mut libc::pthread_mutexattr_t,
        _ty: c_int,
    ) -> c_int {
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_mutexattr_destroy(
        _attr: *mut libc::pthread_mutexattr_t,
    ) -> c_int {
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_cond_init(
        cond: *mut libc::pthread_cond_t,
        _attr: *const libc::pthread_condattr_t,
    ) -> c_int {
        if cond.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        unsafe { ptr::write_bytes(cond.cast::<u8>(), 0, size_of::<libc::pthread_cond_t>()) };
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_cond_signal(_cond: *mut libc::pthread_cond_t) -> c_int {
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_cond_broadcast(_cond: *mut libc::pthread_cond_t) -> c_int {
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_cond_destroy(_cond: *mut libc::pthread_cond_t) -> c_int {
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_condattr_init(attr: *mut libc::pthread_condattr_t) -> c_int {
        if attr.is_null() {
            return LinuxError::EFAULT as c_int;
        }
        unsafe { ptr::write_bytes(attr.cast::<u8>(), 0, size_of::<libc::pthread_condattr_t>()) };
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_condattr_setclock(
        _attr: *mut libc::pthread_condattr_t,
        _clock: libc::clockid_t,
    ) -> c_int {
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_condattr_destroy(
        _attr: *mut libc::pthread_condattr_t,
    ) -> c_int {
        0
    }

    pthread_errno_stub! {
        fn pthread_key_create(
            key: *mut libc::pthread_key_t,
            destructor: Option<unsafe extern "C" fn(*mut c_void)>
        ) -> c_int;
        fn pthread_key_delete(key: libc::pthread_key_t) -> c_int;
        fn pthread_setspecific(key: libc::pthread_key_t, value: *const c_void) -> c_int;
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_getspecific(_key: libc::pthread_key_t) -> *mut c_void {
        ptr::null_mut()
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn pthread_setname_np(
        _thread: libc::pthread_t,
        _name: *const c_char,
    ) -> c_int {
        0
    }
}

#[cfg(not(feature = "multitask"))]
pub use pthread_stubs::*;

#[cfg(feature = "net")]
mod net {
    use ax_posix_api::ctypes as ax_ctypes;

    use super::*;

    const LINUX_SOCKET_FLAG_MASK: c_int = libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC;

    fn linux_domain_to_ax(domain: c_int) -> Result<c_int, LinuxError> {
        match domain {
            libc::AF_UNSPEC => Ok(ax_ctypes::AF_UNSPEC as c_int),
            libc::AF_INET => Ok(ax_ctypes::AF_INET as c_int),
            libc::AF_INET6 => Ok(ax_ctypes::AF_INET6 as c_int),
            _ => Err(LinuxError::EAFNOSUPPORT),
        }
    }

    fn linux_socktype_to_ax(ty: c_int) -> Result<c_int, LinuxError> {
        match ty & !LINUX_SOCKET_FLAG_MASK {
            libc::SOCK_STREAM => Ok(ax_ctypes::SOCK_STREAM as c_int),
            libc::SOCK_DGRAM => Ok(ax_ctypes::SOCK_DGRAM as c_int),
            _ => Err(LinuxError::EINVAL),
        }
    }

    fn apply_socket_flags(fd: c_int, flags: c_int) -> c_int {
        if flags & libc::SOCK_NONBLOCK != 0 {
            let ret = ax_posix_api::sys_fcntl(
                fd,
                ax_ctypes::F_SETFL as c_int,
                ax_ctypes::O_NONBLOCK as usize,
            );
            if ret < 0 {
                let _ = ax_posix_api::sys_close(fd);
                return ok_or_errno(ret);
            }
        }
        fd
    }

    unsafe fn linux_sockaddr_to_ax(
        addr: *const libc::sockaddr,
        len: libc::socklen_t,
    ) -> Result<(ax_ctypes::sockaddr, ax_ctypes::socklen_t), LinuxError> {
        if addr.is_null() {
            return Err(LinuxError::EFAULT);
        }
        if (len as usize) < size_of::<libc::sa_family_t>() {
            return Err(LinuxError::EINVAL);
        }

        match unsafe { (*addr).sa_family as c_int } {
            libc::AF_INET => {
                if (len as usize) < size_of::<libc::sockaddr_in>() {
                    return Err(LinuxError::EINVAL);
                }
                let src = unsafe { *(addr.cast::<libc::sockaddr_in>()) };
                let ax_addr = ax_ctypes::sockaddr_in {
                    sin_family: ax_ctypes::AF_INET as ax_ctypes::sa_family_t,
                    sin_port: src.sin_port,
                    sin_addr: ax_ctypes::in_addr {
                        s_addr: src.sin_addr.s_addr,
                    },
                    sin_zero: [0; 8],
                };
                Ok((
                    unsafe { *(&ax_addr as *const _ as *const ax_ctypes::sockaddr) },
                    size_of::<ax_ctypes::sockaddr>() as ax_ctypes::socklen_t,
                ))
            }
            _ => Err(LinuxError::EAFNOSUPPORT),
        }
    }

    unsafe fn write_linux_sockaddr(
        src: &ax_ctypes::sockaddr,
        addr: *mut libc::sockaddr,
        addrlen: *mut libc::socklen_t,
    ) -> Result<(), LinuxError> {
        if addr.is_null() || addrlen.is_null() {
            return Err(LinuxError::EFAULT);
        }
        if unsafe { *addrlen as usize } < size_of::<libc::sockaddr_in>() {
            return Err(LinuxError::EINVAL);
        }

        let src = unsafe { *(src as *const _ as *const ax_ctypes::sockaddr_in) };
        if src.sin_family != ax_ctypes::AF_INET as ax_ctypes::sa_family_t {
            return Err(LinuxError::EAFNOSUPPORT);
        }

        let dst = libc::sockaddr_in {
            sin_family: libc::AF_INET as libc::sa_family_t,
            sin_port: src.sin_port,
            sin_addr: libc::in_addr {
                s_addr: src.sin_addr.s_addr,
            },
            sin_zero: [0; 8],
        };
        unsafe {
            ptr::copy_nonoverlapping(
                &dst as *const _ as *const u8,
                addr.cast::<u8>(),
                size_of::<libc::sockaddr_in>(),
            );
            *addrlen = size_of::<libc::sockaddr_in>() as libc::socklen_t;
        }
        Ok(())
    }

    fn linux_sockopt_to_ax(level: c_int, optname: c_int) -> Result<(c_int, c_int), LinuxError> {
        if level == libc::SOL_SOCKET {
            let optname: c_int = match optname {
                libc::SO_REUSEADDR => ax_ctypes::SO_REUSEADDR as c_int,
                libc::SO_KEEPALIVE => ax_ctypes::SO_KEEPALIVE as c_int,
                libc::SO_BROADCAST => ax_ctypes::SO_BROADCAST as c_int,
                libc::SO_LINGER => ax_ctypes::SO_LINGER as c_int,
                libc::SO_SNDBUF => ax_ctypes::SO_SNDBUF as c_int,
                libc::SO_RCVBUF => ax_ctypes::SO_RCVBUF as c_int,
                libc::SO_SNDTIMEO => ax_ctypes::SO_SNDTIMEO as c_int,
                libc::SO_RCVTIMEO => ax_ctypes::SO_RCVTIMEO as c_int,
                libc::SO_ERROR => ax_ctypes::SO_ERROR as c_int,
                _ => return Err(LinuxError::ENOPROTOOPT),
            };
            Ok((ax_ctypes::SOL_SOCKET as c_int, optname))
        } else {
            Ok((level, optname))
        }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int {
        let domain = match linux_domain_to_ax(domain) {
            Ok(domain) => domain,
            Err(err) => return fail(err),
        };
        let socktype = match linux_socktype_to_ax(ty) {
            Ok(socktype) => socktype,
            Err(err) => return fail(err),
        };
        let fd = ok_or_errno(ax_posix_api::sys_socket(domain, socktype, protocol));
        if fd < 0 {
            fd
        } else {
            apply_socket_flags(fd, ty & LINUX_SOCKET_FLAG_MASK)
        }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn bind(
        fd: c_int,
        addr: *const libc::sockaddr,
        len: libc::socklen_t,
    ) -> c_int {
        let (addr, len) = match unsafe { linux_sockaddr_to_ax(addr, len) } {
            Ok(addr) => addr,
            Err(err) => return fail(err),
        };
        ok_or_errno(ax_posix_api::sys_bind(fd, &addr, len))
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn connect(
        fd: c_int,
        addr: *const libc::sockaddr,
        len: libc::socklen_t,
    ) -> c_int {
        let (addr, len) = match unsafe { linux_sockaddr_to_ax(addr, len) } {
            Ok(addr) => addr,
            Err(err) => return fail(err),
        };
        ok_or_errno(ax_posix_api::sys_connect(fd, &addr, len))
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn listen(fd: c_int, backlog: c_int) -> c_int {
        ok_or_errno(ax_posix_api::sys_listen(fd, backlog))
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn accept(
        fd: c_int,
        addr: *mut libc::sockaddr,
        len: *mut libc::socklen_t,
    ) -> c_int {
        let mut ax_addr = ax_ctypes::sockaddr::default();
        let mut ax_len = size_of::<ax_ctypes::sockaddr>() as ax_ctypes::socklen_t;
        let new_fd =
            ok_or_errno(unsafe { ax_posix_api::sys_accept(fd, &mut ax_addr, &mut ax_len) });
        if new_fd >= 0
            && !addr.is_null()
            && let Err(err) = unsafe { write_linux_sockaddr(&ax_addr, addr, len) }
        {
            let _ = ax_posix_api::sys_close(new_fd);
            return fail(err);
        }
        new_fd
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn accept4(
        fd: c_int,
        addr: *mut libc::sockaddr,
        len: *mut libc::socklen_t,
        flags: c_int,
    ) -> c_int {
        if flags & !LINUX_SOCKET_FLAG_MASK != 0 {
            return fail(LinuxError::EINVAL);
        }
        let new_fd = unsafe { accept(fd, addr, len) };
        if new_fd < 0 {
            new_fd
        } else {
            apply_socket_flags(new_fd, flags)
        }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn send(
        fd: c_int,
        buf: *const c_void,
        len: SizeT,
        flags: c_int,
    ) -> SSizeT {
        ok_or_errno_isize(ax_posix_api::sys_send(fd, buf, len, flags) as isize) as SSizeT
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn sendto(
        fd: c_int,
        buf: *const c_void,
        len: SizeT,
        flags: c_int,
        addr: *const libc::sockaddr,
        addrlen: libc::socklen_t,
    ) -> SSizeT {
        if addr.is_null() && addrlen == 0 {
            return unsafe { send(fd, buf, len, flags) };
        }
        let (addr, addrlen) = match unsafe { linux_sockaddr_to_ax(addr, addrlen) } {
            Ok(addr) => addr,
            Err(err) => return fail(err) as SSizeT,
        };
        ok_or_errno_isize(ax_posix_api::sys_sendto(fd, buf, len, flags, &addr, addrlen) as isize)
            as SSizeT
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn recv(fd: c_int, buf: *mut c_void, len: SizeT, flags: c_int) -> SSizeT {
        ok_or_errno_isize(ax_posix_api::sys_recv(fd, buf, len, flags) as isize) as SSizeT
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn recvfrom(
        fd: c_int,
        buf: *mut c_void,
        len: SizeT,
        flags: c_int,
        addr: *mut libc::sockaddr,
        addrlen: *mut libc::socklen_t,
    ) -> SSizeT {
        if addr.is_null() {
            return unsafe { recv(fd, buf, len, flags) };
        }
        let mut ax_addr = ax_ctypes::sockaddr::default();
        let mut ax_len = size_of::<ax_ctypes::sockaddr>() as ax_ctypes::socklen_t;
        let ret = ok_or_errno_isize(unsafe {
            ax_posix_api::sys_recvfrom(fd, buf, len, flags, &mut ax_addr, &mut ax_len)
        } as isize) as SSizeT;
        if ret >= 0
            && let Err(err) = unsafe { write_linux_sockaddr(&ax_addr, addr, addrlen) }
        {
            return fail(err) as SSizeT;
        }
        ret
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn shutdown(fd: c_int, how: c_int) -> c_int {
        ok_or_errno(ax_posix_api::sys_shutdown(fd, how))
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn getsockname(
        fd: c_int,
        addr: *mut libc::sockaddr,
        len: *mut libc::socklen_t,
    ) -> c_int {
        let mut ax_addr = ax_ctypes::sockaddr::default();
        let mut ax_len = size_of::<ax_ctypes::sockaddr>() as ax_ctypes::socklen_t;
        let ret =
            ok_or_errno(unsafe { ax_posix_api::sys_getsockname(fd, &mut ax_addr, &mut ax_len) });
        if ret < 0 {
            return ret;
        }
        match unsafe { write_linux_sockaddr(&ax_addr, addr, len) } {
            Ok(()) => 0,
            Err(err) => fail(err),
        }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn getpeername(
        fd: c_int,
        addr: *mut libc::sockaddr,
        len: *mut libc::socklen_t,
    ) -> c_int {
        let mut ax_addr = ax_ctypes::sockaddr::default();
        let mut ax_len = size_of::<ax_ctypes::sockaddr>() as ax_ctypes::socklen_t;
        let ret =
            ok_or_errno(unsafe { ax_posix_api::sys_getpeername(fd, &mut ax_addr, &mut ax_len) });
        if ret < 0 {
            return ret;
        }
        match unsafe { write_linux_sockaddr(&ax_addr, addr, len) } {
            Ok(()) => 0,
            Err(err) => fail(err),
        }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn setsockopt(
        fd: c_int,
        level: c_int,
        optname: c_int,
        optval: *const c_void,
        optlen: libc::socklen_t,
    ) -> c_int {
        let (level, optname) = match linux_sockopt_to_ax(level, optname) {
            Ok(opt) => opt,
            Err(err) => return fail(err),
        };
        ok_or_errno(unsafe { ax_posix_api::sys_setsockopt(fd, level, optname, optval, optlen) })
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn getsockopt(
        _fd: c_int,
        level: c_int,
        optname: c_int,
        optval: *mut c_void,
        optlen: *mut libc::socklen_t,
    ) -> c_int {
        if optval.is_null() || optlen.is_null() {
            return fail(LinuxError::EFAULT);
        }
        if level == libc::SOL_SOCKET {
            match optname {
                libc::SO_ERROR => {
                    let value: c_int = 0;
                    unsafe { write_sockopt(optval, optlen, &value) }
                }
                libc::SO_RCVTIMEO | libc::SO_SNDTIMEO => {
                    let value = libc::timeval {
                        tv_sec: 0,
                        tv_usec: 0,
                    };
                    unsafe { write_sockopt(optval, optlen, &value) }
                }
                libc::SO_LINGER => {
                    let value = libc::linger {
                        l_onoff: 0,
                        l_linger: 0,
                    };
                    unsafe { write_sockopt(optval, optlen, &value) }
                }
                libc::SO_REUSEADDR | libc::SO_KEEPALIVE => {
                    let value: c_int = 0;
                    unsafe { write_sockopt(optval, optlen, &value) }
                }
                libc::SO_SNDBUF | libc::SO_RCVBUF => {
                    let value: c_int = 64 * 1024;
                    unsafe { write_sockopt(optval, optlen, &value) }
                }
                _ => fail(LinuxError::ENOPROTOOPT),
            }
        } else if level == libc::IPPROTO_TCP && optname == libc::TCP_NODELAY {
            let value: c_int = 0;
            unsafe { write_sockopt(optval, optlen, &value) }
        } else {
            fail(LinuxError::ENOPROTOOPT)
        }
    }

    unsafe fn write_sockopt<T: Copy>(
        optval: *mut c_void,
        optlen: *mut libc::socklen_t,
        value: &T,
    ) -> c_int {
        let len = size_of::<T>();
        if unsafe { *optlen as usize } < len {
            return fail(LinuxError::EINVAL);
        }
        unsafe {
            ptr::copy_nonoverlapping(value as *const T as *const u8, optval.cast::<u8>(), len);
            *optlen = len as libc::socklen_t;
        }
        0
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn ioctl(fd: c_int, cmd: c_int, argp: *mut c_void) -> c_int {
        if cmd as libc::c_ulong == libc::FIONBIO as libc::c_ulong {
            if argp.is_null() {
                return fail(LinuxError::EFAULT);
            }
            let nonblocking = unsafe { *(argp as *const c_int) } != 0;
            let flags = if nonblocking {
                libc::O_NONBLOCK as usize
            } else {
                0
            };
            unsafe { fcntl(fd, libc::F_SETFL, flags) }
        } else {
            fail(LinuxError::EINVAL)
        }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn getaddrinfo(
        node: *const c_char,
        service: *const c_char,
        hints: *const libc::addrinfo,
        res: *mut *mut libc::addrinfo,
    ) -> c_int {
        let ret = unsafe { ax_posix_api::sys_getaddrinfo(node, service, hints.cast(), res.cast()) };
        if ret < 0 {
            libc::EAI_FAIL
        } else if ret == 0 {
            libc::EAI_NONAME
        } else {
            0
        }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn freeaddrinfo(res: *mut libc::addrinfo) {
        unsafe { ax_posix_api::sys_freeaddrinfo(res.cast()) };
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn gai_strerror(_errcode: c_int) -> *const c_char {
        c"getaddrinfo error".as_ptr()
    }
}

#[cfg(feature = "net")]
pub use net::*;

#[cfg(not(feature = "net"))]
mod net_stubs {
    use super::*;

    macro_rules! net_stub {
        ($(fn $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty => $body:block)*) => {
            $(
                /// # Safety
                ///
                /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
                #[unsafe(no_mangle)]
                pub unsafe extern "C" fn $name($($arg: $ty),*) -> $ret {
                    let _ = ($($arg,)*);
                    $body
                }
            )*
        };
    }

    fn fail_eai_system() -> c_int {
        set_errno(LinuxError::ENOSYS as i32);
        libc::EAI_SYSTEM
    }

    net_stub! {
        fn socket(domain: c_int, ty: c_int, protocol: c_int) -> c_int => { fail(LinuxError::ENOSYS) }
        fn bind(fd: c_int, addr: *const libc::sockaddr, len: libc::socklen_t) -> c_int => { fail(LinuxError::ENOSYS) }
        fn connect(fd: c_int, addr: *const libc::sockaddr, len: libc::socklen_t) -> c_int => { fail(LinuxError::ENOSYS) }
        fn listen(fd: c_int, backlog: c_int) -> c_int => { fail(LinuxError::ENOSYS) }
        fn accept(fd: c_int, addr: *mut libc::sockaddr, len: *mut libc::socklen_t) -> c_int => { fail(LinuxError::ENOSYS) }
        fn accept4(fd: c_int, addr: *mut libc::sockaddr, len: *mut libc::socklen_t, flags: c_int) -> c_int => { fail(LinuxError::ENOSYS) }
        fn send(fd: c_int, buf: *const c_void, len: SizeT, flags: c_int) -> SSizeT => { fail(LinuxError::ENOSYS) as SSizeT }
        fn sendto(fd: c_int, buf: *const c_void, len: SizeT, flags: c_int, addr: *const libc::sockaddr, addrlen: libc::socklen_t) -> SSizeT => { fail(LinuxError::ENOSYS) as SSizeT }
        fn recv(fd: c_int, buf: *mut c_void, len: SizeT, flags: c_int) -> SSizeT => { fail(LinuxError::ENOSYS) as SSizeT }
        fn recvfrom(fd: c_int, buf: *mut c_void, len: SizeT, flags: c_int, addr: *mut libc::sockaddr, addrlen: *mut libc::socklen_t) -> SSizeT => { fail(LinuxError::ENOSYS) as SSizeT }
        fn shutdown(fd: c_int, how: c_int) -> c_int => { fail(LinuxError::ENOSYS) }
        fn getsockname(fd: c_int, addr: *mut libc::sockaddr, len: *mut libc::socklen_t) -> c_int => { fail(LinuxError::ENOSYS) }
        fn getpeername(fd: c_int, addr: *mut libc::sockaddr, len: *mut libc::socklen_t) -> c_int => { fail(LinuxError::ENOSYS) }
        fn setsockopt(fd: c_int, level: c_int, optname: c_int, optval: *const c_void, optlen: libc::socklen_t) -> c_int => { fail(LinuxError::ENOSYS) }
        fn getsockopt(fd: c_int, level: c_int, optname: c_int, optval: *mut c_void, optlen: *mut libc::socklen_t) -> c_int => { fail(LinuxError::ENOSYS) }
        fn ioctl(fd: c_int, cmd: c_int, argp: *mut c_void) -> c_int => { fail(LinuxError::ENOSYS) }
        fn getaddrinfo(node: *const c_char, service: *const c_char, hints: *const libc::addrinfo, res: *mut *mut libc::addrinfo) -> c_int => { fail_eai_system() }
    }

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn freeaddrinfo(_res: *mut libc::addrinfo) {}

    /// # Safety
    ///
    /// Callers must uphold the Linux/musl ABI contract for this libc symbol.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn gai_strerror(_errcode: c_int) -> *const c_char {
        c"getaddrinfo error".as_ptr()
    }
}

#[cfg(not(feature = "net"))]
pub use net_stubs::*;

#[cfg_attr(not(doc), ax_runtime::ax_app_entry)]
fn axstd_std_check_entry() {
    unsafe extern "C" {
        safe fn main(argc: c_int, argv: *const *const c_char) -> c_int;
    }
    let argv: [*const c_char; 1] = [ptr::null()];
    FD_LAYER_READY.store(true, Ordering::Release);
    main(0, argv.as_ptr());
}
