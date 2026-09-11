//! Host peer for the PL011 RX regression. QEMU read watchpoints control the
//! late arrival; the second burst waits for the guest to consume the first.

use anyhow::{Context, bail};
use ostool::run::qemu::QemuConfig;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

pub(super) struct SerialRxFixture {
    task: Option<JoinHandle<anyhow::Result<()>>>,
}

impl SerialRxFixture {
    pub(super) async fn start(qemu: &mut QemuConfig) -> anyhow::Result<Self> {
        if !cfg!(target_os = "linux") {
            bail!("serial RX fixture requires Linux QEMU chardev logging to /dev/stdout");
        }
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let serial = qemu
            .args
            .iter()
            .position(|arg| arg == "-serial")
            .context("serial RX case requires -serial")?;
        let backend = qemu
            .args
            .get_mut(serial + 1)
            .context("missing serial backend")?;
        *backend = "chardev:serial-rx".into();
        qemu.args.extend([
            "-chardev".into(),
            format!("socket,id=serial-rx,host=127.0.0.1,port={port},logfile=/dev/stdout"),
        ]);
        let debugger = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        qemu.args.extend([
            "-chardev".into(),
            format!(
                "socket,id=serial-rx-debugger,host=127.0.0.1,port={},nodelay=on",
                debugger.local_addr()?.port()
            ),
            "-gdb".into(),
            "chardev:serial-rx-debugger".into(),
        ]);
        let timeout =
            std::time::Duration::from_secs(qemu.timeout.context("serial RX requires a timeout")?);
        let task = tokio::spawn(async move {
            tokio::time::timeout(timeout, exchange(listener, debugger))
                .await
                .context("serial RX host exchange timed out")?
        });
        Ok(Self { task: Some(task) })
    }

    pub(super) async fn finish(mut self) -> anyhow::Result<()> {
        self.task
            .take()
            .context("serial RX fixture already joined")?
            .await
            .context("serial RX host task failed")?
    }
}

impl Drop for SerialRxFixture {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

async fn exchange(listener: TcpListener, debugger: TcpListener) -> anyhow::Result<()> {
    let (mut stream, _) = listener.accept().await?;
    stream.set_nodelay(true)?;
    let debug_stream = debugger.accept().await?.0;
    debug_stream.set_nodelay(true)?;
    let mut debugger = Debugger(debug_stream);
    let mut register_base = None;
    let mut phase = 0;
    let mut tail = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            bail!("serial disconnected before both receive phases completed");
        }
        tail.extend_from_slice(&chunk[..count]);
        let output = String::from_utf8_lossy(&tail);
        if output.contains("ARCEOS_TEST_FAIL") || output.contains("panicked") {
            bail!("serial RX guest failed: {output}");
        }
        if let Some((_, address)) = output.split_once("PL011 serial@0x")
            && let Some((hex, _)) = address.split_once(' ')
        {
            register_base = Some(u64::from_str_radix(hex, 16)?);
        }
        if phase == 0 && output.contains("SERIAL_RX_READY_1") {
            debugger
                .inject_after_empty(
                    &mut stream,
                    register_base.context("PL011 register mapping was not logged")?,
                )
                .await?;
            phase = 1;
            tail.clear();
            continue;
        }
        let payload: Option<&[u8]> = match phase {
            1 if output.contains("SERIAL_RX_READY_2") => Some(b"second-input-after-first-burst"),
            2 if output.contains("ArceOS test suite run OK!") => return Ok(()),
            _ => None,
        };
        if let Some(payload) = payload {
            stream.write_all(payload).await?;
            phase += 1;
            tail.clear();
        } else if tail.len() > 4096 {
            tail.drain(..tail.len() - 2048);
        }
    }
}

/// Minimal GDB remote client for QEMU's MMIO read watchpoints. No target code
/// or register values are changed: only execution is paused at the RX window.
struct Debugger(TcpStream);

impl Debugger {
    async fn send(&mut self, command: &str) -> anyhow::Result<()> {
        let checksum = command.bytes().fold(0u8, u8::wrapping_add);
        self.0
            .write_all(format!("${command}#{checksum:02x}").as_bytes())
            .await?;
        Ok(())
    }

    async fn reply(&mut self) -> anyhow::Result<String> {
        while self.0.read_u8().await? != b'$' {}
        let mut bytes = Vec::new();
        loop {
            let byte = self.0.read_u8().await?;
            if byte == b'#' {
                break;
            }
            if bytes.len() == 4096 {
                bail!("oversized GDB response");
            }
            bytes.push(byte);
        }
        let mut checksum = [0; 2];
        self.0.read_exact(&mut checksum).await?;
        let expected = u8::from_str_radix(std::str::from_utf8(&checksum)?, 16)?;
        if bytes.iter().copied().fold(0u8, u8::wrapping_add) != expected {
            bail!("invalid GDB response checksum");
        }
        self.0.write_all(b"+").await?;
        Ok(String::from_utf8(bytes)?)
    }

    async fn command(&mut self, command: &str) -> anyhow::Result<String> {
        self.send(command).await?;
        self.reply().await
    }

    async fn watch(&mut self, install: bool, address: u64) -> anyhow::Result<()> {
        let op = if install { 'Z' } else { 'z' };
        let reply = self.command(&format!("{op}3,{address:x},4")).await?;
        if reply != "OK" {
            bail!("QEMU rejected MMIO read watchpoint: {reply}");
        }
        Ok(())
    }

    async fn read_u32(&mut self, address: u64) -> anyhow::Result<u32> {
        let reply = self.command(&format!("m{address:x},4")).await?;
        if reply.len() != 8 {
            bail!("invalid GDB memory response: {reply}");
        }
        Ok(u32::from_str_radix(&reply, 16)?.swap_bytes())
    }

    async fn inject_after_empty(
        &mut self,
        serial: &mut TcpStream,
        base: u64,
    ) -> anyhow::Result<()> {
        self.0.write_all(&[3]).await?;
        let stopped = self.reply().await?;
        if !stopped.starts_with('T') && !stopped.starts_with('S') {
            bail!("QEMU did not stop: {stopped}");
        }
        self.watch(true, base).await?; // UARTDR
        self.watch(true, base + 4).await?; // UARTRSR
        serial
            .write_all(b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ!")
            .await?;
        let mut received = 0;
        loop {
            let stop = self.command("c").await?;
            let address = stop
                .split_once("rwatch:")
                .and_then(|(_, rest)| rest.split(';').next())
                .context("QEMU stopped without a read watchpoint")?;
            let address = u64::from_str_radix(address, 16)?;
            if address == base {
                received += 1;
                if received > 63 {
                    bail!("unexpected UARTDR reads before injection");
                }
            } else if address == base + 4 && received == 63 {
                // read_rx_sample reaches RSR only after observing RXFE. A
                // late byte must survive this handler and reach the next IRQ.
                if self.read_u32(base + 0x18).await? & 0x10 == 0 {
                    bail!("RX FIFO was not empty at the controlled window");
                }
                serial.write_all(b"?").await?;
                while self.read_u32(base + 0x3c).await? & 0x10 == 0 {
                    tokio::task::yield_now().await;
                }
                self.watch(false, base).await?;
                self.watch(false, base + 4).await?;
                let reply = self.command("D").await?;
                if reply != "OK" {
                    bail!("QEMU detach failed: {reply}");
                }
                println!("serial RX: injected final byte after RXFE with RXMIS asserted");
                return Ok(());
            }
            // QEMU stops before the watched MMIO load. Step that load with
            // its watchpoint removed before watching the next access.
            self.watch(false, address).await?;
            let stepped = self.command("s").await?;
            if !stepped.starts_with('T') && !stepped.starts_with('S') {
                bail!("QEMU did not single-step the watched load: {stepped}");
            }
            self.watch(true, address).await?;
        }
    }
}
