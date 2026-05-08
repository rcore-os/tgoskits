use core::fmt;

use ax_errno::LinuxResult;
use axnet::{
    RecvOptions, SocketAddrEx, SocketOps,
    unix::{DgramTransport, UnixSocket, UnixSocketAddr},
};

pub fn bind_dev_log() -> LinuxResult<()> {
    let server = UnixSocket::new(DgramTransport::new(1));
    server.bind(SocketAddrEx::Unix(UnixSocketAddr::Path("/dev/log".into())))?;
    ax_task::spawn_with_name(
        move || {
            let mut buf = [0u8; 65536];
            loop {
                match server.recv(&mut buf[..], RecvOptions::default()) {
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
