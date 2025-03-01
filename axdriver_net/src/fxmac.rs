use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::ptr::NonNull;

use axdriver_base::{BaseDriverOps, DevError, DevResult, DeviceType};
use fxmac_rs::{self, xmac_init, FXmac, FXmacGetMacAddress, FXmacLwipPortTx, FXmacRecvHandler};
use log::*;

use crate::{EthernetAddress, NetBufPtr, NetDriverOps};

pub use fxmac_rs::KernelFunc;

extern crate alloc;

const QS: usize = 64;

/// fxmac driver device
pub struct FXmacNic {
    inner: &'static mut FXmac,
    hwaddr: [u8; 6],
    rx_buffer_queue: VecDeque<NetBufPtr>,
}

unsafe impl Sync for FXmacNic {}
unsafe impl Send for FXmacNic {}

impl FXmacNic {
    /// initialize fxmac driver
    pub fn init(mapped_regs: usize) -> DevResult<Self> {
        info!("FXmacNic init @ {:#x}", mapped_regs);
        let rx_buffer_queue = VecDeque::with_capacity(QS);

        let mut hwaddr: [u8; 6] = [0; 6];
        FXmacGetMacAddress(&mut hwaddr, 0);
        info!("Got FXmac HW address: {:x?}", hwaddr);

        let inner = xmac_init(&hwaddr);
        let dev = Self {
            inner,
            hwaddr,
            rx_buffer_queue,
        };
        Ok(dev)
    }
}

impl BaseDriverOps for FXmacNic {
    fn device_name(&self) -> &str {
        "cdns,phytium-gem-1.0"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Net
    }
}

impl NetDriverOps for FXmacNic {
    fn mac_address(&self) -> EthernetAddress {
        EthernetAddress(self.hwaddr)
    }

    fn rx_queue_size(&self) -> usize {
        QS
    }

    fn tx_queue_size(&self) -> usize {
        QS
    }

    fn can_receive(&self) -> bool {
        !self.rx_buffer_queue.is_empty()
    }

    fn can_transmit(&self) -> bool {
        true
    }

    fn recycle_rx_buffer(&mut self, rx_buf: NetBufPtr) -> DevResult {
        unsafe {
            drop(Box::from_raw(rx_buf.raw_ptr::<u8>()));
        }
        drop(rx_buf);
        Ok(())
    }

    fn recycle_tx_buffers(&mut self) -> DevResult {
        // drop tx_buf
        Ok(())
    }

    fn receive(&mut self) -> DevResult<NetBufPtr> {
        if !self.rx_buffer_queue.is_empty() {
            // RX buffer have received packets.
            Ok(self.rx_buffer_queue.pop_front().unwrap())
        } else {
            match FXmacRecvHandler(self.inner) {
                None => Err(DevError::Again),
                Some(packets) => {
                    for packet in packets {
                        debug!("received packet length {}", packet.len());
                        let mut buf = Box::new(packet);
                        let buf_ptr = buf.as_mut_ptr() as *mut u8;
                        let buf_len = buf.len();
                        let rx_buf = NetBufPtr::new(
                            NonNull::new(Box::into_raw(buf) as *mut u8).unwrap(),
                            NonNull::new(buf_ptr).unwrap(),
                            buf_len,
                        );

                        self.rx_buffer_queue.push_back(rx_buf);
                    }

                    Ok(self.rx_buffer_queue.pop_front().unwrap())
                }
            }
        }
    }

    fn transmit(&mut self, tx_buf: NetBufPtr) -> DevResult {
        let mut tx_vec = Vec::new();
        tx_vec.push(tx_buf.packet().to_vec());
        let ret = FXmacLwipPortTx(self.inner, tx_vec);
        unsafe {
            drop(Box::from_raw(tx_buf.raw_ptr::<u8>()));
        }
        if ret < 0 {
            Err(DevError::Again)
        } else {
            Ok(())
        }
    }

    fn alloc_tx_buffer(&mut self, size: usize) -> DevResult<NetBufPtr> {
        let mut tx_buf = Box::new(alloc::vec![0; size]);
        let tx_buf_ptr = tx_buf.as_mut_ptr();

        Ok(NetBufPtr::new(
            NonNull::new(Box::into_raw(tx_buf) as *mut u8).unwrap(),
            NonNull::new(tx_buf_ptr).unwrap(),
            size,
        ))
    }
}
