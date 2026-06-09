use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use anyhow::{Context, bail};

use crate::test::case::HostHttpServerConfig;

pub(crate) struct HostHttpServerGuard {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl HostHttpServerGuard {
    pub(crate) fn start(config: &HostHttpServerConfig, case_name: &str) -> anyhow::Result<Self> {
        let addr = format!("{}:{}", config.bind, config.port);
        let listener = TcpListener::bind(&addr).with_context(|| {
            format!("failed to bind host HTTP server `{addr}` for `{case_name}`")
        })?;
        listener
            .set_nonblocking(true)
            .with_context(|| format!("failed to configure host HTTP server `{addr}`"))?;

        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let body = HostHttpBody::from_config(config);
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            let _ = ready_tx.send(());
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _peer)) => {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                        let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
                        let mut request = [0; 1024];
                        let _ = stream.read(&mut request);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len(),
                        );
                        if stream.write_all(response.as_bytes()).is_ok() {
                            let _ = body.write_to(&mut stream);
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        if ready_rx.recv_timeout(Duration::from_secs(1)).is_err() {
            stop.store(true, Ordering::Release);
            bail!("host HTTP server `{addr}` did not become ready for `{case_name}`");
        }

        println!("  host http server: {addr}");
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

#[derive(Debug, Clone)]
enum HostHttpBody {
    Static(Vec<u8>),
    Generated { size: usize, byte: u8 },
}

impl HostHttpBody {
    fn from_config(config: &HostHttpServerConfig) -> Self {
        match config.body_size {
            Some(size) => Self::Generated {
                size,
                byte: config.body_byte,
            },
            None => Self::Static(config.body.as_bytes().to_vec()),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Static(body) => body.len(),
            Self::Generated { size, .. } => *size,
        }
    }

    fn write_to(&self, stream: &mut impl Write) -> std::io::Result<()> {
        match self {
            Self::Static(body) => stream.write_all(body),
            Self::Generated { size, byte } => {
                let chunk = vec![*byte; 16 * 1024];
                let mut remaining = *size;
                while remaining > 0 {
                    let len = remaining.min(chunk.len());
                    stream.write_all(&chunk[..len])?;
                    remaining -= len;
                }
                Ok(())
            }
        }
    }
}

impl Drop for HostHttpServerGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{TcpListener, TcpStream};

    use super::*;

    #[test]
    fn generated_body_serves_configured_size_and_byte() {
        let port = free_local_port();
        let config = HostHttpServerConfig {
            bind: "127.0.0.1".to_string(),
            port,
            body: "unused".to_string(),
            body_size: Some(7),
            body_byte: b'X',
        };

        let _guard = HostHttpServerGuard::start(&config, "generated-body").unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(b"GET /payload.bin HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();

        let body_offset = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|index| index + 4)
            .unwrap();
        let headers = String::from_utf8_lossy(&response[..body_offset]);
        assert!(headers.contains("Content-Length: 7"));
        assert_eq!(&response[body_offset..], b"XXXXXXX");
    }

    fn free_local_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }
}
