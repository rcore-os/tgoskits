# StarryOS Bug Tracker

## Bug #1: sys_pwrite64 missing negative offset validation

- **Status**: Fixed
- **Severity**: Medium
- **File**: `kernel/src/syscall/fs/io.rs`

### Description
`sys_pwrite64()` did not validate that the `offset` parameter is non-negative. A negative `__kernel_off_t` (i64) was cast to `usize`, wrapping to a very large positive number. On Linux, `pwrite64()` with a negative offset must return `EINVAL`.

### Root Cause
Missing bounds check on the `offset` parameter, unlike the analogous `sys_pread64()` which correctly checks `if offset < 0`.

### Test Plan
1. Create a C test program that calls `pwrite64()` with a negative offset
2. Verify the syscall returns -1 with errno = EINVAL
3. Also verify `pread64()` with negative offset returns -1 with errno = EINVAL (sanity check for existing correct behavior)

### Fix
Added `if offset < 0 { return Err(AxError::InvalidInput); }` check in `sys_pwrite64()`, matching the existing check in `sys_pread64()`.

### Verification
- The test program should show PASS for both pwrite64 and pread64 negative offset checks
- `cargo fmt` and `cargo clippy` should pass

## Bug #2: exit_robust_list never processes the pending futex entry

- **Status**: Fixed
- **Severity**: High
- **File**: `kernel/src/task/ops.rs`

### Description
`exit_robust_list()` skipped the `list_op_pending` entry during the main loop (`if entry != pending`) but never processed it after the loop ended. The pending entry marks a futex that was in the middle of being acquired when the thread died. If never processed, the futex's owner-dead state is never set, causing other threads waiting on that futex to hang forever.

### Root Cause
Missing post-loop processing of the `list_op_pending` entry. The Linux kernel processes this entry after the main loop completes (see `kernel/futex/core.c`).

### Test Plan
1. Create a C test program using POSIX robust mutexes (`pthread_mutexattr_setrobust`)
2. Thread A acquires a robust mutex and exits without unlocking
3. Thread B attempts to acquire the same mutex
4. Without the fix: Thread B hangs forever (futex never woken with owner-dead state)
5. With the fix: Thread B gets `EOWNERDEAD` and can recover with `pthread_mutex_consistent()`

### Fix
Added processing of the `pending` entry after the while loop in `exit_robust_list()`:
```rust
if !pending.is_null() {
    handle_futex_death(pending, offset)?;
}
```

### Verification
- The robust mutex test program should return EOWNERDEAD (not hang) when a thread dies holding a robust mutex
- `cargo fmt` and `cargo clippy` should pass

## Bug #3: sys_fcntl F_GETFL returns wrong access mode flags

- **Status**: Fixed
- **Severity**: Critical
- **File**: `kernel/src/syscall/fs/fd_ops.rs`, `kernel/src/file/fs.rs`

### Description
`sys_fcntl()` with `F_GETFL` derived the access mode flags (`O_RDONLY`/`O_WRONLY`/`O_RDWR`) from the file's inode permission bits via `f.stat()?.mode`, instead of returning the flags the fd was opened with. This caused incorrect results when the file's inode permissions differed from the open mode (e.g., a file opened `O_RDONLY` on a read-write file would return `O_RDWR`).

### Root Cause
The `File` struct did not store the open flags (access mode) at open time. The `F_GETFL` handler tried to reconstruct the access mode from inode permissions, which is fundamentally wrong — inode permissions describe the file's accessibility, not how a particular fd was opened.

### Test Plan
1. Create a C test program that opens a file with different access modes (O_RDONLY, O_WRONLY, O_RDWR)
2. For each open mode, call `fcntl(fd, F_GETFL)` and verify the returned access mode matches the open mode
3. Also test O_NONBLOCK flag preservation
4. Without the fix: F_GETFL returns access mode derived from inode permissions (wrong)
5. With the fix: F_GETFL returns the actual open mode flags

### Fix
1. Added `open_flags` field to the `File` struct in `kernel/src/file/fs.rs` to store the access mode at open time
2. Updated all `File` construction sites to pass the open flags
3. Added `open_flags()` method to the `FileLike` trait with a default implementation returning 0
4. Implemented `open_flags()` for `File` to return the stored flags
5. Updated `F_GETFL` handler to use the stored `open_flags` instead of deriving from `f.stat()?.mode`

### Verification
- The fcntl_getfl test program should correctly report the access mode for O_RDONLY, O_WRONLY, O_RDWR, and O_NONBLOCK
- `cargo fmt` and `cargo clippy` should pass

## Bug #4: sys_shmat panics kernel on invalid shmid

- **Status**: Fixed
- **Severity**: Critical
- **File**: `kernel/src/syscall/ipc/shm.rs`

### Description
`sys_shmat()` used `.unwrap()` on the result of `shm_manager.get_inner_by_shmid(shmid)`, which panics the entire kernel if a userspace program passes an invalid or stale `shmid`. Any userspace process could crash the kernel by calling `shmat()` with a bogus ID — a denial-of-service vulnerability requiring no privileges.

### Root Cause
`.unwrap()` used on user-controlled input instead of proper error handling. The `get_inner_by_shmid()` returns `Option<_>`, and `.unwrap()` panics on `None` instead of returning an error.

### Test Plan
1. Create a C test program that calls `shmat()` with invalid shmid values (0, -1, 99999)
2. Verify each call returns `(void *)-1` with an appropriate errno
3. Without the fix: kernel panics on the first invalid shmat call
4. With the fix: shmat gracefully returns an error

### Fix
Replaced `.unwrap()` with `.ok_or(AxError::InvalidInput)?` to gracefully handle invalid shmid values.

### Verification
- The test program should complete without kernel panic
- All invalid shmid calls should return error, not crash

## Bug #5: TIOCSPGRP ignores user-supplied pgid, always sets caller's own group

- **Status**: Fixed
- **Severity**: High
- **File**: `kernel/src/pseudofs/dev/tty/mod.rs`

### Description
The `TIOCSPGRP` ioctl handler completely ignored the `arg` parameter (which should contain the user-supplied process group ID) and instead always set the foreground group to the calling process's own group. This broke all job control functionality — `tcsetpgrp()` could never actually change the foreground group.

### Root Cause
The handler used `current().as_thread().proc_data.proc.group()` instead of reading the pgid from the `arg` parameter. The `arg` parameter is a pointer to a `pid_t` in user space that should be dereferenced and used as the target process group ID.

### Test Plan
1. Create a C test program that calls `tcsetpgrp()` with the current process group
2. Verify the call succeeds
3. Call `tcsetpgrp()` with an invalid pgid and verify it returns an error
4. Call `tcgetpgrp()` and verify it returns the pgid that was set
5. Without the fix: tcsetpgrp always sets caller's own group regardless of argument
6. With the fix: tcsetpgrp correctly sets the specified foreground process group

### Fix
Updated the `TIOCSPGRP` handler to read the pgid from the `arg` parameter using `(arg as *const u32).vm_read()`, look up the corresponding process group via `get_process_group(pgid)`, and set it as the foreground group. The existing `set_foreground()` method already validates that the process group belongs to the terminal's session, returning `EPERM` if not.

### Verification
- The test program should correctly set and get the foreground process group
- Invalid pgid values should return appropriate errors

## Bug #6: sys_mremap always creates MAP_PRIVATE mapping, silently corrupting shared mappings

- **Status**: Fixed
- **Severity**: Medium
- **File**: `kernel/src/syscall/mm/mmap.rs`

### Description
`sys_mremap()` hardcoded `MmapFlags::PRIVATE` when allocating the new mapping, regardless of whether the original mapping was `MAP_SHARED`. If a process remapped a shared memory region (e.g., a `shmget`/`shmat` segment or a file-backed `MAP_SHARED` mapping), the new mapping would be `MAP_PRIVATE`. This meant writes to the remapped region would no longer be visible to other processes sharing the original mapping, silently breaking shared memory semantics.

### Root Cause
The `MmapFlags::PRIVATE` was hardcoded in the `sys_mmap()` call within `sys_mremap()`, instead of deriving the sharing type from the original mapping's properties.

### Test Plan
1. Create a `MAP_SHARED|MAP_ANONYMOUS` mapping
2. Write data to it
3. Call `mremap()` to resize it
4. Verify the data is preserved and the mapping remains shared
5. Also test that `MAP_PRIVATE` mappings still work correctly after mremap
6. Without the fix: shared mappings silently become private after mremap
7. With the fix: shared mappings remain shared after mremap

### Fix
Updated `sys_mremap()` to determine the original mapping's sharing type (shared vs private) from the VMA backend type and pass the correct `MmapFlags` to `sys_mmap()` instead of hardcoding `MmapFlags::PRIVATE`.

### Verification
- The test program should correctly preserve shared mapping semantics after mremap
- Private mappings should continue to work correctly

## Bug #7: Directory read/write returns EBADF instead of EISDIR

- **Status**: Fixed
- **Severity**: Medium
- **File**: `kernel/src/file/fs.rs`

### Description
The `Directory` implementation of `FileLike` returned `AxError::BadFileDescriptor` (EBADF) for `read()` and `write()`, but per POSIX, attempting to read() or write() on a directory must return `EISDIR` (errno 21). The fd is valid — it's a directory fd — so `EBADF` is incorrect. Many programs specifically check for `EISDIR` to distinguish "this is a directory" from "bad file descriptor".

### Root Cause
Wrong error type used — `AxError::BadFileDescriptor` instead of `AxError::IsADirectory`.

### Test Plan
1. Open a directory with open()
2. Call read() on the directory fd and verify errno is EISDIR (not EBADF)
3. Call write() on the directory fd and verify errno is EISDIR (not EBADF)
4. Without the fix: both return EBADF
5. With the fix: both return EISDIR

### Fix
Changed `AxError::BadFileDescriptor` to `AxError::IsADirectory` in both `Directory::read()` and `Directory::write()` methods.

### Verification
- The test program should show EISDIR for read/write on directory fds
- `cargo fmt` and `cargo clippy` should pass

## Bug #8: File::from_fd() returns EPIPE for non-File/non-Directory fds, causing lseek on pipes to return wrong errno

- **Status**: Fixed
- **Severity**: Medium
- **File**: `kernel/src/file/fs.rs`

### Description
`File::from_fd()` returned `AxError::BrokenPipe` (EPIPE) when failing to downcast a file-like object (e.g., pipe, socket) to a `File`. This caused `lseek()` on pipes to return `EPIPE` instead of the POSIX-mandated `ESPIPE` (errno 29, "Illegal seek"). Similarly, `pread()`/`pwrite()` on pipes should also return `ESPIPE`. The pipe isn't broken — the operation simply isn't supported for this file type.

### Root Cause
`AxError::BrokenPipe` was used as a catch-all error for non-File/non-Directory file-like objects in `File::from_fd()`, but this maps to `EPIPE` which is semantically incorrect for seek operations on unseekable fds.

### Test Plan
1. Create a pipe
2. Call `lseek()` on the pipe read end
3. Verify the return value is -1 with errno = ESPIPE (not EPIPE)
4. Without the fix: lseek returns EPIPE
5. With the fix: lseek returns ESPIPE

### Fix
1. Changed the error type in `File::from_fd()` from `AxError::BrokenPipe` to `AxError::InvalidInput` (EINVAL), which is the correct generic error for unsupported file type operations.
2. Added a `file_for_seek()` helper in `kernel/src/syscall/fs/io.rs` that converts `AxError::InvalidInput` from `File::from_fd()` to `AxError::from(LinuxError::ESPIPE)` for seek-related syscalls (`sys_lseek`, `sys_pread64`, `sys_pwrite64`, `sys_preadv2`, `sys_pwritev2`).
3. Also fixed `sys_fadvise64` which used `AxError::BrokenPipe` for pipe fds — changed to `AxError::from(LinuxError::ESPIPE)` per POSIX.

### Verification
- The test program should show ESPIPE for lseek on pipes
- `cargo fmt` and `cargo clippy` should pass
