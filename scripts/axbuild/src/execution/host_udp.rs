//! Optional UDP echo fixture, configured by the caller's runtime description.
use std::{
    fs,
    io::ErrorKind,
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, ensure};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EchoConfig {
    #[serde(default = "loopback")]
    bind: IpAddr,
    port: u16,
}

fn loopback() -> IpAddr {
    Ipv4Addr::LOCALHOST.into()
}

#[derive(Deserialize)]
struct RuntimeFixtures {
    host_udp_echo: Option<EchoConfig>,
}

pub(crate) struct HostUdpEchoGuard {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<anyhow::Result<()>>>,
}

impl HostUdpEchoGuard {
    pub(crate) fn start_for_case(path: &Path) -> anyhow::Result<Option<Self>> {
        let config: RuntimeFixtures = toml::from_str(&fs::read_to_string(path)?)
            .with_context(|| format!("invalid UDP echo configuration in {}", path.display()))?;
        config.host_udp_echo.as_ref().map(Self::start).transpose()
    }

    pub(crate) fn is_configured(runtime: &toml::Value) -> anyhow::Result<bool> {
        let fixtures: RuntimeFixtures = runtime
            .clone()
            .try_into()
            .context("invalid host_udp_echo runtime configuration")?;
        Ok(fixtures.host_udp_echo.is_some())
    }

    fn start(config: &EchoConfig) -> anyhow::Result<Self> {
        let requested = SocketAddr::new(config.bind, config.port);
        let socket = UdpSocket::bind(requested)
            .with_context(|| format!("failed to bind host UDP echo {requested}"))?;
        let address = socket.local_addr()?;
        // Binding makes the socket ready before QEMU starts. Bounded I/O lets
        // cancellation join the owner thread without leaving a live fixture.
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        socket.set_write_timeout(Some(Duration::from_millis(100)))?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::Builder::new()
            .name("host-udp-echo".into())
            .spawn(move || echo_datagrams(socket, &worker_stop))
            .context("failed to spawn host UDP echo worker")?;
        println!("  host udp echo: {address}");
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }

    pub(crate) fn finish(mut self) -> anyhow::Result<()> {
        self.stop_and_join()
    }

    fn stop_and_join(&mut self) -> anyhow::Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| anyhow::anyhow!("host UDP echo {} panicked", self.address))?
                .with_context(|| format!("host UDP echo {} failed", self.address))?;
        }
        Ok(())
    }
}

fn echo_datagrams(socket: UdpSocket, stop: &AtomicBool) -> anyhow::Result<()> {
    let mut buffer = [0; 65536];
    while !stop.load(Ordering::Acquire) {
        match socket.recv_from(&mut buffer) {
            Ok((len, peer)) => {
                let sent = socket
                    .send_to(&buffer[..len], peer)
                    .context("failed to echo UDP datagram")?;
                ensure!(sent == len, "UDP echo returned a partial datagram");
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::TimedOut | ErrorKind::WouldBlock | ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error).context("failed to receive UDP datagram"),
        }
    }
    Ok(())
}

impl Drop for HostUdpEchoGuard {
    fn drop(&mut self) {
        if let Err(error) = self.stop_and_join() {
            eprintln!("  host udp echo cleanup: {error:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datagrams_are_preserved_and_fixture_releases_its_exclusive_port() {
        let fixture = HostUdpEchoGuard::start(&EchoConfig {
            bind: loopback(),
            port: 0,
        })
        .unwrap();
        let address = fixture.address;
        assert!(
            HostUdpEchoGuard::start(&EchoConfig {
                bind: address.ip(),
                port: address.port()
            })
            .is_err()
        );
        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        for payload in [&[][..], b"message\0\xff".as_slice(), &[0xa5; 4096][..]] {
            assert_eq!(client.send_to(payload, address).unwrap(), payload.len());
            let mut received = [0; 4097];
            let (len, peer) = client.recv_from(&mut received).unwrap();
            assert_eq!(peer, address);
            assert_eq!(&received[..len], payload);
        }
        fixture.finish().unwrap();
        let rebound = UdpSocket::bind(address).unwrap();
        drop(rebound);
        let fixture = HostUdpEchoGuard::start(&EchoConfig {
            bind: address.ip(),
            port: address.port(),
        })
        .unwrap();
        drop(fixture);
        assert!(UdpSocket::bind(address).is_ok());
    }

    #[test]
    fn optional_fixture_is_explicit_and_invalid_configuration_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("runtime.toml");
        fs::write(&path, "args=[]\n").unwrap();
        assert!(HostUdpEchoGuard::start_for_case(&path).unwrap().is_none());
        fs::write(&path, "[host_udp_echo]\nbind=\"not-an-address\"\nport=1\n").unwrap();
        assert!(HostUdpEchoGuard::start_for_case(&path).is_err());
        fs::write(&path, "[host_udp_echo]\nport=0\nunknown=true\n").unwrap();
        assert!(HostUdpEchoGuard::start_for_case(&path).is_err());
    }
}
