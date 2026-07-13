//! ArceOS extensions and private adapters used by `ax-std`.
//!
//! This module intentionally exposes capabilities rather than re-exporting
//! ArceOS implementation crates. System software must depend on those crates
//! directly.

pub use ax_errno::{AxError, AxResult};

/// Platform-specific constants used by the `ax-std` implementation.
pub mod config {
    /// Stack size used when callers do not provide an explicit task stack.
    pub const TASK_STACK_SIZE: usize = 0x40000;
}

/// System operations used by `process` and libc compatibility symbols.
pub mod sys {
    pub use ax_hal::{cpu_num as ax_get_cpu_num, power::system_off as ax_terminate};
}

/// Time operations used by the standard time facade.
pub mod time {
    pub use ax_hal::time::{
        TimeValue as AxTimeValue, monotonic_time as ax_monotonic_time, wall_time as ax_wall_time,
    };
}

/// Task extensions that do not have a Rust standard-library equivalent.
pub mod task {
    #[cfg(feature = "multitask")]
    use core::time::Duration;

    #[cfg(not(feature = "multitask"))]
    pub use ax_kspin::SpinRaw as AxRawMutex;
    #[cfg(feature = "multitask")]
    pub use ax_sync::RawMutex as AxRawMutex;

    #[cfg(feature = "multitask")]
    use super::AxResult;

    #[track_caller]
    pub fn ax_sleep_until(deadline: crate::os::arceos::time::AxTimeValue) {
        #[cfg(feature = "multitask")]
        ax_task::sleep_until(deadline);
        #[cfg(not(feature = "multitask"))]
        ax_hal::time::busy_wait_until(deadline);
    }

    #[track_caller]
    pub fn ax_yield_now() {
        #[cfg(feature = "multitask")]
        ax_task::yield_now();
        #[cfg(not(feature = "multitask"))]
        if cfg!(feature = "irq") {
            ax_hal::asm::wait_for_irqs();
        } else {
            core::hint::spin_loop();
        }
    }

    #[track_caller]
    pub fn ax_exit(exit_code: i32) -> ! {
        #[cfg(feature = "multitask")]
        ax_task::exit(exit_code);
        #[cfg(not(feature = "multitask"))]
        {
            let _ = exit_code;
            ax_hal::power::system_off();
        }
    }

    #[cfg(feature = "multitask")]
    pub use ax_task::AxCpuMask;

    #[cfg(feature = "multitask")]
    pub struct AxTaskHandle {
        inner: ax_task::AxTaskRef,
        id: u64,
    }

    #[cfg(feature = "multitask")]
    impl AxTaskHandle {
        pub fn id(&self) -> u64 {
            self.id
        }

        pub fn join(self) -> i32 {
            self.inner.join()
        }
    }

    #[cfg(feature = "multitask")]
    pub struct AxWaitQueueHandle(ax_task::WaitQueue);

    #[cfg(feature = "multitask")]
    impl AxWaitQueueHandle {
        pub const fn new() -> Self {
            Self(ax_task::WaitQueue::new())
        }
    }

    #[cfg(feature = "multitask")]
    impl Default for AxWaitQueueHandle {
        fn default() -> Self {
            Self::new()
        }
    }

    #[cfg(feature = "multitask")]
    pub fn ax_spawn<F>(f: F, name: alloc_crate::string::String, stack_size: usize) -> AxTaskHandle
    where
        F: FnOnce() + Send + 'static,
    {
        let inner = ax_task::spawn_raw(f, name, stack_size);
        AxTaskHandle {
            id: inner.id().as_u64(),
            inner,
        }
    }

    #[cfg(feature = "multitask")]
    pub fn ax_set_current_priority(prio: isize) -> AxResult {
        if ax_task::set_priority(prio) {
            Ok(())
        } else {
            ax_errno::ax_err!(BadState, "failed to set task priority")
        }
    }

    #[cfg(feature = "multitask")]
    #[track_caller]
    pub fn ax_set_current_affinity(cpumask: AxCpuMask) -> AxResult {
        if ax_task::set_current_affinity(cpumask) {
            Ok(())
        } else {
            ax_errno::ax_err!(BadState, "failed to set task affinity")
        }
    }

    #[cfg(feature = "multitask")]
    #[track_caller]
    pub fn ax_wait_queue_wait(wq: &AxWaitQueueHandle, timeout: Option<Duration>) -> bool {
        #[cfg(feature = "irq")]
        if let Some(duration) = timeout {
            return wq.0.wait_timeout(duration);
        }

        if timeout.is_some() {
            ax_log::warn!("wait queue timeout is ignored without the irq feature");
        }
        wq.0.wait();
        false
    }

    #[cfg(feature = "multitask")]
    #[track_caller]
    pub fn ax_wait_queue_wait_until(
        wq: &AxWaitQueueHandle,
        until_condition: impl Fn() -> bool,
        timeout: Option<Duration>,
    ) -> bool {
        #[cfg(feature = "irq")]
        if let Some(duration) = timeout {
            return wq.0.wait_timeout_until(duration, until_condition);
        }

        if timeout.is_some() {
            ax_log::warn!("wait queue timeout is ignored without the irq feature");
        }
        wq.0.wait_until(until_condition);
        false
    }

    #[cfg(feature = "multitask")]
    pub fn ax_wait_queue_wake(wq: &AxWaitQueueHandle, count: u32) {
        if count == u32::MAX {
            wq.0.notify_all(true);
        } else {
            for _ in 0..count {
                if !wq.0.notify_one(true) {
                    break;
                }
            }
        }
    }

    #[cfg(feature = "multitask")]
    pub fn ax_wait_queue_wake_one_with(wq: &AxWaitQueueHandle, func: impl Fn(u64)) {
        wq.0.notify_one_with(true, func);
    }
}

#[cfg(feature = "fs")]
pub(crate) mod fs {
    use alloc_crate::string::String;
    pub use ax_fs_ng::fops::{
        DirEntry as AxDirEntry, FileAttr as AxFileAttr, FilePerm as AxFilePerm,
        FilePermExt as AxFilePermExt, FileType as AxFileType, FileTypeExt as AxFileTypeExt,
        OpenOptions as AxOpenOptions,
    };
    use ax_fs_ng::fops::{Directory, File};
    pub use ax_io::SeekFrom as AxSeekFrom;

    use super::AxResult;

    pub struct AxFileHandle(pub(crate) File);
    pub struct AxDirHandle(pub(crate) Directory);

    pub fn ax_open_file(path: &str, options: &AxOpenOptions) -> AxResult<AxFileHandle> {
        Ok(AxFileHandle(File::open(path, options)?))
    }

    pub fn ax_open_dir(path: &str, options: &AxOpenOptions) -> AxResult<AxDirHandle> {
        Ok(AxDirHandle(Directory::open_dir(path, options)?))
    }

    pub fn ax_read_file(file: &mut AxFileHandle, buf: &mut [u8]) -> AxResult<usize> {
        file.0.read(buf)
    }

    pub fn ax_write_file(file: &mut AxFileHandle, buf: &[u8]) -> AxResult<usize> {
        file.0.write(buf)
    }

    pub fn ax_truncate_file(file: &AxFileHandle, size: u64) -> AxResult {
        file.0.truncate(size)
    }

    pub fn ax_flush_file(file: &AxFileHandle) -> AxResult {
        file.0.flush()
    }

    pub fn ax_seek_file(file: &mut AxFileHandle, pos: AxSeekFrom) -> AxResult<u64> {
        file.0.seek(pos)
    }

    pub fn ax_file_attr(file: &AxFileHandle) -> AxResult<AxFileAttr> {
        file.0.get_attr()
    }

    pub fn ax_read_dir(dir: &mut AxDirHandle, entries: &mut [AxDirEntry]) -> AxResult<usize> {
        dir.0.read_dir(entries)
    }

    pub fn ax_create_dir(path: &str) -> AxResult {
        ax_fs_ng::api::create_dir(path)
    }

    pub fn ax_remove_dir(path: &str) -> AxResult {
        ax_fs_ng::api::remove_dir(path)
    }

    pub fn ax_remove_file(path: &str) -> AxResult {
        ax_fs_ng::api::remove_file(path)
    }

    pub fn ax_rename(old: &str, new: &str) -> AxResult {
        ax_fs_ng::api::rename(old, new)
    }

    pub fn ax_current_dir() -> AxResult<String> {
        ax_fs_ng::api::current_dir()
    }

    pub fn ax_set_current_dir(path: &str) -> AxResult {
        ax_fs_ng::api::set_current_dir(path)
    }
}

#[cfg(feature = "net")]
pub(crate) mod net {
    #[cfg(feature = "dns")]
    use core::net::IpAddr;
    use core::net::SocketAddr;

    use ax_net::{
        RecvFlags, RecvOptions, SendOptions, Shutdown, Socket, SocketAddrEx, SocketOps,
        tcp::TcpSocket, udp::UdpSocket,
    };

    use super::AxResult;

    pub struct AxTcpSocketHandle(pub(crate) TcpSocket);
    pub struct AxUdpSocketHandle(pub(crate) UdpSocket);

    pub fn ax_tcp_socket() -> AxTcpSocketHandle {
        AxTcpSocketHandle(TcpSocket::new())
    }

    pub fn ax_tcp_socket_addr(socket: &AxTcpSocketHandle) -> AxResult<SocketAddr> {
        socket.0.local_addr()?.into_ip()
    }

    pub fn ax_tcp_peer_addr(socket: &AxTcpSocketHandle) -> AxResult<SocketAddr> {
        socket.0.peer_addr()?.into_ip()
    }

    pub fn ax_tcp_connect(socket: &AxTcpSocketHandle, addr: SocketAddr) -> AxResult {
        socket.0.connect(SocketAddrEx::Ip(addr))
    }

    pub fn ax_tcp_bind(socket: &AxTcpSocketHandle, addr: SocketAddr) -> AxResult {
        socket.0.bind(SocketAddrEx::Ip(addr))
    }

    pub fn ax_tcp_listen(socket: &AxTcpSocketHandle, backlog: usize) -> AxResult {
        socket.0.listen(backlog)
    }

    pub fn ax_tcp_accept(socket: &AxTcpSocketHandle) -> AxResult<(AxTcpSocketHandle, SocketAddr)> {
        let Socket::Tcp(socket) = socket.0.accept()? else {
            unreachable!("TCP listener accepted a non-TCP socket");
        };
        let addr = socket.peer_addr()?.into_ip()?;
        Ok((AxTcpSocketHandle(*socket), addr))
    }

    pub fn ax_tcp_send(socket: &AxTcpSocketHandle, buf: &[u8]) -> AxResult<usize> {
        socket.0.send(buf, SendOptions::default())
    }

    pub fn ax_tcp_recv(socket: &AxTcpSocketHandle, buf: &mut [u8]) -> AxResult<usize> {
        socket.0.recv(buf, RecvOptions::default())
    }

    pub fn ax_tcp_shutdown(socket: &AxTcpSocketHandle) -> AxResult {
        socket.0.shutdown(Shutdown::Both)
    }

    pub fn ax_udp_socket() -> AxUdpSocketHandle {
        AxUdpSocketHandle(UdpSocket::new())
    }

    pub fn ax_udp_socket_addr(socket: &AxUdpSocketHandle) -> AxResult<SocketAddr> {
        socket.0.local_addr()?.into_ip()
    }

    pub fn ax_udp_peer_addr(socket: &AxUdpSocketHandle) -> AxResult<SocketAddr> {
        socket.0.peer_addr()?.into_ip()
    }

    pub fn ax_udp_bind(socket: &AxUdpSocketHandle, addr: SocketAddr) -> AxResult {
        socket.0.bind(SocketAddrEx::Ip(addr))
    }

    pub fn ax_udp_recv_from(
        socket: &AxUdpSocketHandle,
        buf: &mut [u8],
    ) -> AxResult<(usize, SocketAddr)> {
        let mut from = SocketAddrEx::Ip(SocketAddr::from(([0, 0, 0, 0], 0)));
        let len = socket.0.recv(
            buf,
            RecvOptions {
                from: Some(&mut from),
                ..RecvOptions::default()
            },
        )?;
        Ok((len, from.into_ip()?))
    }

    pub fn ax_udp_peek_from(
        socket: &AxUdpSocketHandle,
        buf: &mut [u8],
    ) -> AxResult<(usize, SocketAddr)> {
        let mut from = SocketAddrEx::Ip(SocketAddr::from(([0, 0, 0, 0], 0)));
        let len = socket.0.recv(
            buf,
            RecvOptions {
                from: Some(&mut from),
                flags: RecvFlags::PEEK,
                ..RecvOptions::default()
            },
        )?;
        Ok((len, from.into_ip()?))
    }

    pub fn ax_udp_send_to(
        socket: &AxUdpSocketHandle,
        buf: &[u8],
        addr: SocketAddr,
    ) -> AxResult<usize> {
        socket.0.send(
            buf,
            SendOptions {
                to: Some(SocketAddrEx::Ip(addr)),
                ..SendOptions::default()
            },
        )
    }

    pub fn ax_udp_connect(socket: &AxUdpSocketHandle, addr: SocketAddr) -> AxResult {
        socket.0.connect(SocketAddrEx::Ip(addr))
    }

    pub fn ax_udp_send(socket: &AxUdpSocketHandle, buf: &[u8]) -> AxResult<usize> {
        socket.0.send(buf, SendOptions::default())
    }

    pub fn ax_udp_recv(socket: &AxUdpSocketHandle, buf: &mut [u8]) -> AxResult<usize> {
        socket.0.recv(buf, RecvOptions::default())
    }

    #[cfg(feature = "dns")]
    pub fn ax_dns_query(domain_name: &str) -> AxResult<alloc_crate::vec::Vec<IpAddr>> {
        ax_net::dns_query(domain_name)
    }
}

/// Framebuffer access, which has no Rust standard-library equivalent.
#[cfg(feature = "display")]
pub mod display {
    pub use ax_display::{DisplayInfo, framebuffer_flush, framebuffer_info};
}
