//! Bounded HTTP I/O whose completion includes acknowledgement of response bytes.

use alloc::string::String;

use smoltcp::{iface::SocketHandle, socket::tcp, time::Duration};
use uefi::{Status, boot};

use super::{
    super::{http::HttpConnection, serial::SerialBeacon},
    Network,
};

pub(super) struct Connection<'a> {
    network: &'a mut Network,
    serial: &'a mut SerialBeacon,
    epoch: String,
    tcp: SocketHandle,
}

impl<'a> Connection<'a> {
    pub(super) fn new(
        network: &'a mut Network,
        serial: &'a mut SerialBeacon,
        epoch: String,
        tcp: SocketHandle,
    ) -> Self {
        Self {
            network,
            serial,
            epoch,
            tcp,
        }
    }

    fn progress(&mut self) -> uefi::Result<()> {
        self.network
            .drive(self.serial, &self.epoch, Some(self.tcp))?;
        boot::stall(core::time::Duration::from_millis(1));
        Ok(())
    }

    fn socket(&mut self) -> &mut tcp::Socket<'static> {
        self.network.sockets.get_mut::<tcp::Socket>(self.tcp)
    }

    pub(super) fn finish(&mut self) -> uefi::Result<()> {
        let mut remaining = self.socket().send_queue();
        let mut deadline = self.network.device.now() + Duration::from_secs(30);
        while remaining != 0 {
            self.progress()?;
            // A reset or lost DHCP lease can discard the TX queue without an
            // ACK. Only the original live connection can confirm the response.
            if self.network.address.is_none()
                || !matches!(
                    self.socket().state(),
                    tcp::State::Established | tcp::State::CloseWait
                )
            {
                return Err(Status::ABORTED.into());
            }
            let queued = self.socket().send_queue();
            if queued < remaining {
                remaining = queued;
                deadline = self.network.device.now() + Duration::from_secs(30);
            }
            if remaining != 0
                && (!self.socket().is_active() || self.network.device.now() >= deadline)
            {
                return Err(Status::TIMEOUT.into());
            }
        }
        // smoltcp removes bytes from tx_buffer only when the peer ACK advances.
        // In particular, a boot action cannot outlive an unacknowledged 202.
        self.socket().close();
        let deadline = self.network.device.now() + Duration::from_secs(5);
        while self.socket().is_active() {
            self.progress()?;
            if self.network.device.now() >= deadline {
                self.socket().abort();
                break;
            }
        }
        Ok(())
    }

    pub(super) fn abort(&mut self) {
        self.socket().abort();
    }
}

impl HttpConnection for Connection<'_> {
    fn receive(&mut self, buffer: &mut [u8]) -> uefi::Result<usize> {
        let deadline = self.network.device.now() + Duration::from_secs(30);
        loop {
            if self.socket().can_recv() {
                return self
                    .socket()
                    .recv_slice(buffer)
                    .map_err(|_| Status::DEVICE_ERROR.into());
            }
            if !self.socket().may_recv() {
                return Ok(0);
            }
            if self.network.device.now() >= deadline {
                return Err(Status::TIMEOUT.into());
            }
            self.progress()?;
        }
    }

    fn send(&mut self, mut buffer: &[u8]) -> uefi::Result<()> {
        let mut deadline = self.network.device.now() + Duration::from_secs(30);
        while !buffer.is_empty() {
            if self.socket().can_send() {
                let sent = self
                    .socket()
                    .send_slice(buffer)
                    .map_err(|_| Status::DEVICE_ERROR)?;
                buffer = &buffer[sent..];
                deadline = self.network.device.now() + Duration::from_secs(30);
            }
            if !buffer.is_empty() {
                if !self.socket().may_send() {
                    return Err(Status::ABORTED.into());
                }
                if self.network.device.now() >= deadline {
                    return Err(Status::TIMEOUT.into());
                }
                self.progress()?;
            }
        }
        Ok(())
    }
}
