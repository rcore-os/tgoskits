//! Public APIs and types for [ArceOS] modules
//!
//! [ArceOS]: https://github.com/arceos-org/arceos

#![no_std]
#![allow(unused_imports)]

extern crate alloc;

#[macro_use]
mod macros;
mod error;
mod imp;

pub use error::{ApiError, ApiResult};

/// Platform-specific constants and parameters.
pub mod config {
    /// Stack size used when callers do not provide an explicit task stack.
    pub const TASK_STACK_SIZE: usize = 0x40000;
}

/// System operations.
pub mod sys {
    define_api! {
        /// Returns the number of available logical CPUs.
        pub fn ax_get_cpu_num() -> usize;
        /// Drain task-console output, then shut down the whole system and all CPUs.
        pub fn ax_terminate() -> !;
    }
}

/// Time-related operations.
pub mod time {
    define_api_type! {
        pub type AxTimeValue;
    }

    define_api! {
        /// Returns the time elapsed since system boot.
        pub fn ax_monotonic_time() -> AxTimeValue;
        /// Returns the time elapsed since epoch, also known as realtime.
        pub fn ax_wall_time() -> AxTimeValue;
    }
}

/// Memory management.
pub mod mem {
    use core::{alloc::Layout, ptr::NonNull};

    define_api! {
        @cfg "alloc";
        /// Allocates a continuous memory blocks with the given `layout` in
        /// the global allocator.
        ///
        /// Returns [`None`] if the allocation fails.
        ///
        /// # Safety
        ///
        /// This function is unsafe because it requires users to manually manage
        /// the buffer life cycle.
        pub unsafe fn ax_alloc(layout: Layout) -> Option<NonNull<u8>>;
        /// Deallocates the memory block at the given `ptr` pointer with the given
        /// `layout`, which should be allocated by [`ax_alloc`].
        ///
        /// # Safety
        ///
        /// This function is unsafe because it requires users to manually manage
        /// the buffer life cycle.
        pub unsafe fn ax_dealloc(ptr: NonNull<u8>, layout: Layout);
    }
}

/// Standard input and output.
pub mod stdio {
    use core::fmt;
    define_api! {
        /// Reads a slice of bytes from the console, returns the number of bytes read.
        pub fn ax_console_read_bytes(buf: &mut [u8]) -> crate::ApiResult<usize>;
        /// Writes a slice of bytes to the console, returns the number of bytes written.
        pub fn ax_console_write_bytes(buf: &[u8]) -> crate::ApiResult<usize>;
        /// Writes a formatted string through the sleepable TTY console path.
        pub fn ax_console_write_fmt(args: fmt::Arguments) -> fmt::Result;
        /// Sleeps until task-console input becomes readable.
        pub fn ax_console_wait_readable() -> crate::ApiResult;
        /// Drains queued task output through the physical UART.
        pub fn ax_console_flush() -> crate::ApiResult;
    }
}

/// Multi-threading management.
pub mod task {
    define_api_type! {
        pub type AxTaskHandle;
        pub type AxWaitQueueHandle;
        pub type AxCpuMask;
        pub type AxRawMutex;
    }

    define_api! {
        /// Current task is going to sleep, it will be woken up at the given monotonic deadline.
        #[track_caller]
        pub fn ax_sleep_until(deadline: crate::time::AxTimeValue);

        /// Current task gives up the CPU time voluntarily, and switches to another
        /// ready task.
        #[track_caller]
        pub fn ax_yield_now();

        /// Exits the current task with the given exit code.
        #[track_caller]
        pub fn ax_exit(exit_code: i32) -> !;
    }

    define_api! {
        /// Returns the current task's ID.
        pub fn ax_current_task_id() -> u64;
        /// Spawns a new task with the given entry point and other arguments.
        pub fn ax_spawn(
            f: impl FnOnce() + Send + 'static,
            name: alloc::string::String,
            stack_size: usize
        ) -> AxTaskHandle;
        /// Waits for the given task to exit, and returns its exit code (the
        /// argument of [`ax_exit`]).
        #[track_caller]
        pub fn ax_wait_for_exit(task: AxTaskHandle) -> i32;
        /// Sets the priority of the current task.
        pub fn ax_set_current_priority(prio: isize) -> crate::ApiResult;
        /// Sets the cpu affinity of the current task.
        #[track_caller]
        pub fn ax_set_current_affinity(cpumask: AxCpuMask) -> crate::ApiResult;
        /// Blocks the current task and put it into the wait queue, until
        /// other tasks notify the wait queue, or the given duration has
        /// elapsed (if specified).
        #[track_caller]
        pub fn ax_wait_queue_wait(wq: &AxWaitQueueHandle, timeout: Option<core::time::Duration>) -> bool;
        /// Blocks the current task and put it into the wait queue, until the
        /// given condition becomes true, or the given duration has elapsed
        /// (if specified).
        #[track_caller]
        pub fn ax_wait_queue_wait_until(
            wq: &AxWaitQueueHandle,
            until_condition: impl Fn() -> bool,
            timeout: Option<core::time::Duration>,
        ) -> bool;
        /// Wakes up one or more tasks in the wait queue.
        ///
        /// The maximum number of tasks to wake up is specified by `count`. If
        /// `count` is `u32::MAX`, it will wake up all tasks in the wait queue.
        pub fn ax_wait_queue_wake(wq: &AxWaitQueueHandle, count: u32);
        /// Wakes up at most one task in the wait queue after performing an
        /// operation on it via the provided callback `func`.
        ///
        /// The callback `func` is invoked while holding the wait-queue lock. If a
        /// task is woken, `func` is called with an implementation-defined `u64`
        /// value associated with that task.
        pub fn ax_wait_queue_wake_one_with(wq: &AxWaitQueueHandle, func: impl Fn(u64));
    }
}

/// Filesystem manipulation operations.
pub mod fs {
    use crate::ApiResult;

    define_api_type! {
        @cfg "fs";
        pub type AxFileHandle;
        pub type AxDirHandle;
        pub type AxOpenOptions;
        pub type AxFileAttr;
        pub type AxFileType;
        pub type AxFileTypeExt;
        pub type AxFilePerm;
        pub type AxFilePermExt;
        pub type AxDirEntry;
        pub type AxSeekFrom;
    }

    define_api! {
        @cfg "fs";

        /// Opens a file at the path relative to the current directory with the
        /// options specified by `opts`.
        pub fn ax_open_file(path: &str, opts: &AxOpenOptions) -> ApiResult<AxFileHandle>;
        /// Opens a directory at the path relative to the current directory with
        /// the options specified by `opts`.
        pub fn ax_open_dir(path: &str, opts: &AxOpenOptions) -> ApiResult<AxDirHandle>;

        /// Reads the file at the current position, returns the number of bytes read.
        ///
        /// After the read, the cursor will be advanced by the number of bytes read.
        pub fn ax_read_file(file: &mut AxFileHandle, buf: &mut [u8]) -> ApiResult<usize>;
        /// Reads the file at the given position, returns the number of bytes read.
        ///
        /// It does not update the file cursor.
        pub fn ax_read_file_at(file: &AxFileHandle, offset: u64, buf: &mut [u8]) -> ApiResult<usize>;
        /// Writes the file at the current position, returns the number of bytes
        /// written.
        ///
        /// After the write, the cursor will be advanced by the number of bytes
        /// written.
        pub fn ax_write_file(file: &mut AxFileHandle, buf: &[u8]) -> ApiResult<usize>;
        /// Writes the file at the given position, returns the number of bytes
        /// written.
        ///
        /// It does not update the file cursor.
        pub fn ax_write_file_at(file: &AxFileHandle, offset: u64, buf: &[u8]) -> ApiResult<usize>;
        /// Truncates the file to the specified size.
        pub fn ax_truncate_file(file: &AxFileHandle, size: u64) -> ApiResult;
        /// Flushes the file, writes all buffered data to the underlying device.
        pub fn ax_flush_file(file: &AxFileHandle) -> ApiResult;
        /// Sets the cursor of the file to the specified offset. Returns the new
        /// position after the seek.
        pub fn ax_seek_file(file: &mut AxFileHandle, pos: AxSeekFrom) -> ApiResult<u64>;
        /// Returns attributes of the file.
        pub fn ax_file_attr(file: &AxFileHandle) -> ApiResult<AxFileAttr>;

        /// Reads directory entries starts from the current position into the
        /// given buffer, returns the number of entries read.
        ///
        /// After the read, the cursor of the directory will be advanced by the
        /// number of entries read.
        pub fn ax_read_dir(dir: &mut AxDirHandle, dirents: &mut [AxDirEntry]) -> ApiResult<usize>;
        /// Creates a new, empty directory at the provided path.
        pub fn ax_create_dir(path: &str) -> ApiResult;
        /// Removes an empty directory.
        ///
        /// If the directory is not empty, it will return an error.
        pub fn ax_remove_dir(path: &str) -> ApiResult;
        /// Removes a file from the filesystem.
        pub fn ax_remove_file(path: &str) -> ApiResult;
        /// Rename a file or directory to a new name.
        ///
        /// It will delete the original file if `new` already exists.
        pub fn ax_rename(old: &str, new: &str) -> ApiResult;

        /// Returns the current working directory.
        pub fn ax_current_dir() -> ApiResult<alloc::string::String>;
        /// Changes the current working directory to the specified path.
        pub fn ax_set_current_dir(path: &str) -> ApiResult;
    }
}

/// Networking primitives for TCP/UDP communication.
pub mod net {
    use core::net::{IpAddr, SocketAddr};

    use crate::{ApiResult, io::AxPollState};

    define_api_type! {
        @cfg "net";
        pub type AxTcpSocketHandle;
        pub type AxUdpSocketHandle;
    }

    define_api! {
        @cfg "net";

        // TCP socket

        /// Creates a new TCP socket.
        pub fn ax_tcp_socket() -> AxTcpSocketHandle;
        /// Returns the local address and port of the TCP socket.
        pub fn ax_tcp_socket_addr(socket: &AxTcpSocketHandle) -> ApiResult<SocketAddr>;
        /// Returns the remote address and port of the TCP socket.
        pub fn ax_tcp_peer_addr(socket: &AxTcpSocketHandle) -> ApiResult<SocketAddr>;
        /// Moves this TCP socket into or out of nonblocking mode.
        pub fn ax_tcp_set_nonblocking(socket: &AxTcpSocketHandle, nonblocking: bool) -> ApiResult;

        /// Connects the TCP socket to the given address and port.
        pub fn ax_tcp_connect(handle: &AxTcpSocketHandle, addr: SocketAddr) -> ApiResult;
        /// Binds the TCP socket to the given address and port.
        pub fn ax_tcp_bind(socket: &AxTcpSocketHandle, addr: SocketAddr) -> ApiResult;
        /// Starts listening on the bound address and port.
        pub fn ax_tcp_listen(socket: &AxTcpSocketHandle, _backlog: usize) -> ApiResult;
        /// Accepts a new connection on the TCP socket.
        ///
        /// This function will block the calling thread until a new TCP connection
        /// is established. When established, a new TCP socket is returned.
        pub fn ax_tcp_accept(socket: &AxTcpSocketHandle) -> ApiResult<(AxTcpSocketHandle, SocketAddr)>;

        /// Transmits data in the given buffer on the TCP socket.
        pub fn ax_tcp_send(socket: &AxTcpSocketHandle, buf: &[u8]) -> ApiResult<usize>;
        /// Receives data on the TCP socket, and stores it in the given buffer.
        /// On success, returns the number of bytes read.
        pub fn ax_tcp_recv(socket: &AxTcpSocketHandle, buf: &mut [u8]) -> ApiResult<usize>;
        /// Returns whether the TCP socket is readable or writable.
        pub fn ax_tcp_poll(socket: &AxTcpSocketHandle) -> ApiResult<AxPollState>;
        /// Closes the connection on the TCP socket.
        pub fn ax_tcp_shutdown(socket: &AxTcpSocketHandle) -> ApiResult;

        // UDP socket

        /// Creates a new UDP socket.
        pub fn ax_udp_socket() -> AxUdpSocketHandle;
        /// Returns the local address and port of the UDP socket.
        pub fn ax_udp_socket_addr(socket: &AxUdpSocketHandle) -> ApiResult<SocketAddr>;
        /// Returns the remote address and port of the UDP socket.
        pub fn ax_udp_peer_addr(socket: &AxUdpSocketHandle) -> ApiResult<SocketAddr>;
        /// Moves this UDP socket into or out of nonblocking mode.
        pub fn ax_udp_set_nonblocking(socket: &AxUdpSocketHandle, nonblocking: bool) -> ApiResult;

        /// Binds the UDP socket to the given address and port.
        pub fn ax_udp_bind(socket: &AxUdpSocketHandle, addr: SocketAddr) -> ApiResult;
        /// Receives a single datagram message on the UDP socket.
        pub fn ax_udp_recv_from(socket: &AxUdpSocketHandle, buf: &mut [u8]) -> ApiResult<(usize, SocketAddr)>;
        /// Receives a single datagram message on the UDP socket, without
        /// removing it from the queue.
        pub fn ax_udp_peek_from(socket: &AxUdpSocketHandle, buf: &mut [u8]) -> ApiResult<(usize, SocketAddr)>;
        /// Sends data on the UDP socket to the given address. On success,
        /// returns the number of bytes written.
        pub fn ax_udp_send_to(socket: &AxUdpSocketHandle, buf: &[u8], addr: SocketAddr) -> ApiResult<usize>;

        /// Connects this UDP socket to a remote address, allowing the `send` and
        /// `recv` to be used to send data and also applies filters to only receive
        /// data from the specified address.
        pub fn ax_udp_connect(socket: &AxUdpSocketHandle, addr: SocketAddr) -> ApiResult;
        /// Sends data on the UDP socket to the remote address to which it is
        /// connected.
        pub fn ax_udp_send(socket: &AxUdpSocketHandle, buf: &[u8]) -> ApiResult<usize>;
        /// Receives a single datagram message on the UDP socket from the remote
        /// address to which it is connected. On success, returns the number of
        /// bytes read.
        pub fn ax_udp_recv(socket: &AxUdpSocketHandle, buf: &mut [u8]) -> ApiResult<usize>;
        /// Returns whether the UDP socket is readable or writable.
        pub fn ax_udp_poll(socket: &AxUdpSocketHandle) -> ApiResult<AxPollState>;

        // Miscellaneous

        /// Resolves the host name to a list of IP addresses.
        pub fn ax_dns_query(domain_name: &str) -> ApiResult<alloc::vec::Vec<IpAddr>>;
        /// Poll the network stack.
        ///
        /// It may receive packets from the NIC and process them, and transmit queued
        /// packets to the NIC.
        pub fn ax_poll_interfaces() -> ApiResult;
    }
}

/// Graphics manipulation operations.
pub mod display {
    define_api_type! {
        @cfg "display";
        pub type AxDisplayInfo;
    }

    define_api! {
        @cfg "display";
        /// Gets the framebuffer information.
        pub fn ax_framebuffer_info() -> AxDisplayInfo;
        /// Flushes the framebuffer, i.e. show on the screen.
        pub fn ax_framebuffer_flush() -> bool;
    }
}

/// Input/output operations.
pub mod io {
    define_api_type! {
        pub type AxPollState;
    }
}

/// Re-exports of ArceOS modules.
///
/// You should prefer to use other APIs rather than these modules. The modules
/// here should only be used if other APIs do not meet your requirements.
pub mod modules {
    #[cfg(feature = "alloc")]
    pub use ax_alloc;
    #[cfg(feature = "display")]
    pub use ax_display;
    #[cfg(feature = "fs")]
    pub use ax_fs_ng;
    pub use ax_hal;
    #[cfg(feature = "ipi")]
    pub use ax_ipi;
    pub use ax_log;
    #[cfg(feature = "paging")]
    pub use ax_mm;
    #[cfg(feature = "net")]
    pub use ax_net;
    pub use ax_runtime;
    pub use ax_task;
    pub use axklib;
    pub use dma_api;
}
