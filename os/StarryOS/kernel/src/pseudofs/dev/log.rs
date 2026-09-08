use core::fmt;

use ax_net::{
    RecvOptions, SocketAddrEx, SocketOps, poll_socket_io,
    unix::{DgramTransport, UnixSocket, UnixSocketAddr},
};
use axpoll::IoEvents;

use crate::StarryResult;

pub fn bind_dev_log() -> StarryResult<()> {
    let server = UnixSocket::new(DgramTransport::new(1));
    server.bind(SocketAddrEx::Unix(UnixSocketAddr::Path("/dev/log".into())))?;
    crate::task::spawn_kernel_thread(
        move || {
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
        },
        "dev-log-server".into(),
    );
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
