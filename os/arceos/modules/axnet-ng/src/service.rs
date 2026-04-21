use alloc::boxed::Box;
use core::{
    pin::Pin,
    task::{Context, Waker},
};

use ax_hal::time::{NANOS_PER_MICROS, TimeValue, wall_time_nanos};
use ax_task::future::sleep_until;
use smoltcp::{
    iface::{Interface, SocketSet},
    time::Instant,
    wire::{HardwareAddress, IpAddress, IpListenEndpoint},
};

use crate::{SOCKET_SET, router::Router};

fn now() -> Instant {
    Instant::from_micros_const((wall_time_nanos() / NANOS_PER_MICROS) as i64)
}

pub struct Service {
    pub iface: Interface,
    router: Router,
    timeout: Option<Pin<Box<dyn Future<Output = ()> + Send>>>,
}
impl Service {
    pub fn new(mut router: Router) -> Self {
        let config = smoltcp::iface::Config::new(HardwareAddress::Ip);
        let iface = Interface::new(config, &mut router, now());

        Self {
            iface,
            router,
            timeout: None,
        }
    }

    pub fn poll(&mut self, sockets: &mut SocketSet) -> bool {
        let timestamp = now();

        self.router.poll(timestamp);
        self.iface.poll(timestamp, &mut self.router, sockets);
        self.router.dispatch(timestamp)
    }

    pub fn get_source_address(&self, dst_addr: &IpAddress) -> IpAddress {
        let Some(rule) = self.router.table.lookup(dst_addr) else {
            panic!("no route to destination: {dst_addr}");
        };
        rule.src
    }

    pub fn device_mask_for(&self, endpoint: &IpListenEndpoint) -> u32 {
        match endpoint.addr {
            Some(addr) => self
                .router
                .table
                .lookup(&addr)
                .map_or(0, |it| 1u32 << it.dev),
            None => u32::MAX,
        }
    }

    pub fn register_waker(&mut self, mask: u32, waker: &Waker) {
        // Always arm a fallback timer so the task re-polls periodically even
        // if the device IRQ waker is lost (observed on x86_64/loongarch64
        // under TCG where virtio-net PCI INTx delivery races with the
        // scheduler). smoltcp's poll_at may also return None when nothing is
        // scheduled, which would otherwise leave the task waiting only on
        // IRQ.
        const FALLBACK_POLL_MS: u64 = 50;
        let smoltcp_next = self.iface.poll_at(now(), &SOCKET_SET.inner.lock());
        let fallback =
            ax_hal::time::wall_time() + core::time::Duration::from_millis(FALLBACK_POLL_MS);
        let deadline = match smoltcp_next {
            Some(t) => {
                let smol_deadline = TimeValue::from_micros(t.total_micros() as _);
                smol_deadline.min(fallback)
            }
            None => fallback,
        };

        // drop old timeout future
        self.timeout = None;

        let mut fut = Box::pin(sleep_until(deadline));
        let mut cx = Context::from_waker(waker);

        if fut.as_mut().poll(&mut cx).is_ready() {
            waker.wake_by_ref();
        } else {
            self.timeout = Some(fut);
        }

        for (i, device) in self.router.devices.iter().enumerate() {
            if mask & (1 << i) != 0 {
                device.register_waker(waker);
            }
        }
    }
}
