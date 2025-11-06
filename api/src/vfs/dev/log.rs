use core::bstr::ByteStr;

use axerrno::LinuxResult;
use axnet::{
    RecvOptions, SocketAddrEx, SocketOps,
    unix::{DgramTransport, UnixSocket, UnixSocketAddr},
};

pub fn bind_dev_log() -> LinuxResult<()> {
    let server = UnixSocket::new(DgramTransport::new(1));
    server.bind(SocketAddrEx::Unix(UnixSocketAddr::Path("/dev/log".into())))?;
    axtask::spawn(
        move || {
            let mut buf = [0u8; 65536];
            loop {
                match server.recv(&mut buf.as_mut_slice(), RecvOptions::default()) {
                    Ok(read) => {
                        let msg = ByteStr::new(buf[..read].trim_ascii_end());
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
