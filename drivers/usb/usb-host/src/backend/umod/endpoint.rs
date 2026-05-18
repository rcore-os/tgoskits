use std::{
    collections::HashMap,
    ptr::null_mut,
    sync::{Arc, Weak, atomic::AtomicBool},
};

use futures::task::AtomicWaker;
use libusb1_sys::{
    libusb_cancel_transfer, libusb_control_transfer_get_data, libusb_fill_bulk_transfer,
    libusb_fill_control_setup, libusb_fill_control_transfer, libusb_fill_iso_transfer,
    libusb_submit_transfer, libusb_transfer,
};
use log::trace;
use usb_if::{
    endpoint::{RequestId, TransferCompletion, TransferRequest},
    err::TransferError,
    transfer::{BmRequestType, Direction},
};

use super::{device::DeviceHandle, err::transfer_status_to_result};
use crate::backend::ty::{
    ep::{EndpointOp, transfer_to_completion},
    transfer::{Transfer, TransferKind},
};

pub struct EndpointImpl {
    dev: Arc<DeviceHandle>,
    address: u8,
    transfers: HashMap<u64, Arc<TransferHandleRaw>>,
}

impl EndpointImpl {
    pub fn new(dev: Arc<DeviceHandle>, address: u8) -> Self {
        Self {
            dev,
            address,
            transfers: HashMap::new(),
        }
    }

    fn make_transfer(
        &mut self,
        transfer: Transfer,
    ) -> Result<Arc<TransferHandleRaw>, TransferError> {
        // 对于 ISO transfer，需要指定 iso_packets 数量
        let iso_packets = match &transfer.kind {
            TransferKind::Isochronous { packet_lengths } => packet_lengths.len() as i32,
            _ => 0,
        };

        let trans_ptr = unsafe { libusb1_sys::libusb_alloc_transfer(iso_packets) };
        if trans_ptr.is_null() {
            return Err(TransferError::Other(anyhow!(
                "Failed to allocate libusb transfer"
            )));
        }

        // 保存类型和方向
        let direction = transfer.direction;
        let mut buffer = null_mut();
        let data_len;
        let timeout = 1000; // TODO: make it configurable

        if let Some((buff_ptr, buff_len)) = transfer.buffer {
            buffer = buff_ptr.as_ptr();
            data_len = buff_len;
        } else {
            data_len = 0;
        }

        // 判断是否为控制传输
        let temp_buff = if matches!(transfer.kind, TransferKind::Control(_)) {
            let total_len = 8 + data_len;
            vec![0u8; total_len]
        } else {
            vec![]
        };

        let temp_buff_ptr = temp_buff.as_ptr() as *mut u8;

        let trans_handle = Arc::new(TransferHandleRaw {
            transfer: trans_ptr,
            origin: transfer,
            waker: AtomicWaker::new(),
            ok: AtomicBool::new(false),
            _temp_buff: temp_buff,
        });

        let dev_handle = self.dev.raw();
        let weak = Arc::downgrade(&trans_handle);
        let user_data = Weak::into_raw(weak) as *mut core::ffi::c_void;

        match &trans_handle.origin.kind {
            TransferKind::Control(setup) => {
                unsafe {
                    buffer = temp_buff_ptr;

                    // OUT 传输：复制用户数据到 buffer[8..]
                    if direction == Direction::Out
                        && data_len > 0
                        && let Some((ptr, _len)) = trans_handle.origin.buffer
                    {
                        core::ptr::copy_nonoverlapping(ptr.as_ptr(), buffer.add(8), data_len);
                    }

                    // 填充 setup 包到 buffer[0..8]
                    libusb_fill_control_setup(
                        buffer,
                        BmRequestType::new(direction, setup.request_type, setup.recipient).into(),
                        setup.request.into(),
                        setup.value,
                        setup.index,
                        data_len as _, // wLength = 数据长度
                    );

                    libusb_fill_control_transfer(
                        trans_ptr,
                        dev_handle,
                        buffer,
                        transfer_callback,
                        user_data,
                        timeout,
                    )
                };
            }
            TransferKind::Bulk => {
                unsafe {
                    libusb_fill_bulk_transfer(
                        trans_ptr,
                        dev_handle,
                        self.address,
                        buffer,
                        data_len as i32,
                        transfer_callback,
                        user_data,
                        timeout,
                    )
                };
            }
            TransferKind::Interrupt => {
                unsafe {
                    libusb_fill_bulk_transfer(
                        trans_ptr,
                        dev_handle,
                        self.address,
                        buffer,
                        data_len as i32,
                        transfer_callback,
                        user_data,
                        timeout,
                    )
                };
            }
            TransferKind::Isochronous { packet_lengths } => {
                let num_pkgs = packet_lengths.len();
                trace!(
                    "Filling ISO transfer: buff@{:p} num_pkgs={}, data_len={}",
                    buffer, num_pkgs, data_len
                );
                unsafe {
                    libusb_fill_iso_transfer(
                        trans_ptr,
                        dev_handle,
                        self.address,
                        buffer,
                        data_len as i32,
                        num_pkgs as _,
                        transfer_callback,
                        user_data,
                        timeout,
                    )
                };

                for (i, packet_length) in packet_lengths.iter().copied().enumerate() {
                    let packet = unsafe { &mut *(*trans_ptr).iso_packet_desc.as_mut_ptr().add(i) };
                    packet.length = packet_length as u32;
                }
            }
        }

        Ok(trans_handle)
    }
}

unsafe impl Send for EndpointImpl {}

impl EndpointOp for EndpointImpl {
    fn submit_request(
        &mut self,
        request: TransferRequest,
    ) -> Result<RequestId, usb_if::err::TransferError> {
        let (kind, direction, buffer) = request.into();
        let transfer = Transfer {
            kind,
            direction,
            buffer: buffer.map(|buffer| (buffer.ptr, buffer.len)),
            transfer_len: 0,
            iso_packet_actual_lengths: Vec::new(),
        };
        let trans = self.make_transfer(transfer)?;
        let id = trans.id();
        let ptr = trans.transfer;
        self.transfers.insert(id, trans);
        let submit_result = usb!(libusb_submit_transfer(ptr))
            .map_err(|e| TransferError::Other(anyhow!("Failed to submit transfer: {e:?}")));

        if submit_result.is_err() {
            self.transfers.remove(&id);
            return Err(submit_result.err().unwrap());
        }
        trace!("Submitted libusb transfer id {:#x}, ptr{:p}", id, ptr);
        Ok(RequestId::new(id))
    }

    fn reclaim_request(
        &mut self,
        id: RequestId,
    ) -> Option<Result<TransferCompletion, usb_if::err::TransferError>> {
        let trans = self.transfers.get(&id.raw())?;
        if !trans.ok.load(std::sync::atomic::Ordering::Acquire) {
            return None;
        }
        let trans = self.transfers.remove(&id.raw()).unwrap();
        Some(
            trans
                .to_result()
                .map(|transfer| transfer_to_completion(id, transfer)),
        )
    }

    fn register_waker(&self, id: RequestId, cx: &mut std::task::Context<'_>) {
        if let Some(trans) = self.transfers.get(&id.raw()) {
            trans.register_waker(cx);
        }
    }

    fn cancel_request(&mut self, id: RequestId) -> Result<(), usb_if::err::TransferError> {
        let trans = self
            .transfers
            .get(&id.raw())
            .ok_or(TransferError::InvalidEndpoint)?;
        let res = unsafe { libusb_cancel_transfer(trans.transfer) };
        if res == libusb1_sys::constants::LIBUSB_SUCCESS {
            Ok(())
        } else {
            Err(TransferError::Other(anyhow!(
                "Failed to cancel transfer: libusb error {res}"
            )))
        }
    }
}

struct TransferHandleRaw {
    transfer: *mut libusb_transfer,
    origin: Transfer,
    ok: AtomicBool,
    waker: AtomicWaker,
    _temp_buff: Vec<u8>, // 用于控制传输的临时 buffer，保存 setup 包 + 数据
}

unsafe impl Send for TransferHandleRaw {}
unsafe impl Sync for TransferHandleRaw {}

impl TransferHandleRaw {
    fn register_waker(&self, cx: &mut std::task::Context<'_>) {
        self.waker.register(cx.waker());
    }

    fn to_result(
        &self,
    ) -> Result<crate::backend::ty::transfer::Transfer, usb_if::err::TransferError> {
        transfer_status_to_result(unsafe { (*self.transfer).status })?;
        let trans_raw = unsafe { &*self.transfer };

        if matches!(self.origin.kind, TransferKind::Control(_))
            && let Some((ptr, _len)) = self.origin.buffer
        {
            // 控制传输，提取数据部分
            let data_ptr = unsafe { libusb_control_transfer_get_data(self.transfer) };
            let data_len = trans_raw.actual_length as usize;
            unsafe {
                core::ptr::copy_nonoverlapping(data_ptr, ptr.as_ptr(), data_len);
            }
        }

        let mut out = self.origin.clone();
        out.transfer_len = trans_raw.actual_length as usize;
        if let TransferKind::Isochronous { packet_lengths } = &self.origin.kind {
            out.iso_packet_actual_lengths = Vec::with_capacity(packet_lengths.len());
            for i in 0..trans_raw.num_iso_packets as usize {
                let packet = unsafe { &*trans_raw.iso_packet_desc.as_ptr().add(i) };
                out.iso_packet_actual_lengths
                    .push(packet.actual_length as usize);
            }
        }
        Ok(out)
    }

    fn id(&self) -> u64 {
        self.transfer as usize as u64
    }
}

impl Drop for TransferHandleRaw {
    fn drop(&mut self) {
        unsafe {
            trace!("Freeing libusb transfer {:p}", self.transfer);
            libusb1_sys::libusb_free_transfer(self.transfer);
        }
    }
}

extern "system" fn transfer_callback(transfer: *mut libusb_transfer) {
    let user_data = unsafe { (*transfer).user_data };
    if user_data.is_null() {
        return;
    }
    let weak: Weak<TransferHandleRaw> =
        unsafe { Weak::from_raw(user_data as *const TransferHandleRaw) };

    if let Some(trans_handle) = weak.upgrade() {
        trace!("libusb transfer callback called, transfer={:p}", transfer);

        trans_handle
            .ok
            .store(true, std::sync::atomic::Ordering::Release);
        trans_handle.waker.wake();
    }
}
