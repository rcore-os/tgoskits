use core::fmt;

use ax_net::{
    RecvOptions, SocketOps, poll_socket_io,
    unix::{DgramTransport, UnixSocket},
};
use axpoll::IoEvents;

use crate::StarryResult;

pub fn bind_dev_log() -> StarryResult<()> {
    let server = UnixSocket::new(DgramTransport::new(1));
    // The static devfs constructor already owns this inode. Binding the
    // transport neither opens it for file I/O nor requests a directory change.
    let caller = axfs_ng_vfs::NativeLinkCaller::root();
    let view = ax_fs_ng::FsView::current(ax_fs_ng::vfs::current_fs_context(), &caller);
    let log = view.resolve("/dev/log")?;
    if log.node_type() != axfs_ng_vfs::NodeType::Socket {
        return Err(crate::StarryError::BadState);
    }
    server.bind_path_with("/dev/log".into(), |_| -> StarryResult<_> {
        Ok(log.user_data().get_or_insert_with(Default::default))
    })?;
    crate::task::kernel_thread_builder("dev-log-server".into())
        .spawn(move || {
            let mut buf = [0u8; 65536];
            loop {
                let mut dst = &mut buf[..];
                let mut options = RecvOptions::default();
                match crate::task::future::block_on(poll_socket_io(
                    &server,
                    IoEvents::IN,
                    false,
                    || server.try_recv(&mut dst, &mut options),
                )) {
                    Ok(read) => {
                        let msg = LossyByteStr(buf[..read].trim_ascii_end());
                        info!("{msg}");
                    }
                    Err(err) => {
                        warn!("Failed to receive logs from client: {err:?}");
                        break;
                    }
                }
            }
        })
        .expect("failed to spawn kernel thread");
    Ok(())
}

struct LossyByteStr<'a>(&'a [u8]);

impl fmt::Display for LossyByteStr<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for chunk in self.0.utf8_chunks() {
            f.write_str(chunk.valid())?;
            if !chunk.invalid().is_empty() {
                f.write_str("\u{FFFD}")?;
            }
        }
        Ok(())
    }
}
