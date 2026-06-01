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
        let body = config.body.clone();
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
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes());
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

impl Drop for HostHttpServerGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
