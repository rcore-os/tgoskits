#![cfg_attr(not(target_os = "none"), allow(dead_code, unused_imports))]

mod descriptor;
mod irq;
mod manager;
mod tree;

use alloc::{borrow::ToOwned, collections::VecDeque, sync::Arc, vec::Vec};
use core::{any::Any, mem::size_of, task::Context};

use ax_errno::{AxError, AxResult, LinuxResult};
use axfs_ng_vfs::Filesystem;
use axpoll::{IoEvents, Pollable};
use spin::Mutex;
use starry_vm::VmMutPtr;

use self::{irq::manager, manager::UsbFsManager, tree::UsbRootDir};
use crate::{
    file::{File as KernelFile, FileLike, IoDst, IoSrc, Kstat},
    pseudofs::{SimpleDir, SimpleFs},
};

fn create_filesystem(manager: Arc<UsbFsManager>) -> Filesystem {
    info!("usbfs: creating filesystem instance");
    SimpleFs::new_with("usbfs".into(), descriptor::USBFS_MAGIC, move |fs| {
        SimpleDir::new_maker(
            fs.clone(),
            Arc::new(UsbRootDir {
                fs: fs.clone(),
                manager: manager.clone(),
            }),
        )
    })
}

pub(crate) fn new_usbfs() -> LinuxResult<Filesystem> {
    if let Some(manager) = manager() {
        return Ok(create_filesystem(manager));
    }

    info!("usbfs: initializing manager");
    let (hosts, irq_slots) = manager::discover_hosts();
    let manager = Arc::new(UsbFsManager::new(hosts));
    irq::init_globals(manager.clone(), irq_slots);
    let should_spawn_refresh = manager::initialize_hosts(&manager) > 0;

    if should_spawn_refresh {
        info!("usbfs: spawning refresh task");
        let refresh_manager = manager.clone();
        ax_task::spawn_with_name(
            move || ax_task::future::block_on(manager::usbfs_refresh_task(refresh_manager.clone())),
            "usbfs-refresh".to_owned(),
        );
        manager.refresh_event.notify(1);
    }

    Ok(create_filesystem(manager))
}

pub(crate) fn is_usbfs_device(inner: &dyn Any) -> bool {
    inner.is::<tree::UsbDeviceOps>()
}

pub(crate) fn open_usbfs_file(
    inner: &dyn Any,
    file: ax_fs::File,
    open_flags: u32,
) -> AxResult<Arc<dyn FileLike>> {
    let ops = inner
        .downcast_ref::<tree::UsbDeviceOps>()
        .ok_or(ax_errno::AxError::InvalidInput)?;
    let manager = manager().ok_or(ax_errno::AxError::NoSuchDevice)?;
    let snapshot = manager
        .device_snapshot(ops.bus_num, ops.device_num)
        .ok_or(ax_errno::AxError::NoSuchDevice)?;
    Ok(Arc::new(UsbDeviceFile {
        base: KernelFile::new(file, open_flags),
        manager,
        bus_num: ops.bus_num,
        device_num: ops.device_num,
        snapshot,
        lease: Mutex::new(None),
        claimed_interfaces: Mutex::new(Default::default()),
        pending_urbs: Mutex::new(VecDeque::new()),
    }))
}

struct UsbDeviceFile {
    base: KernelFile,
    manager: Arc<UsbFsManager>,
    bus_num: u8,
    device_num: u8,
    snapshot: descriptor::UsbDeviceSnapshot,
    lease: Mutex<Option<manager::UsbDeviceLease>>,
    claimed_interfaces: Mutex<alloc::collections::BTreeMap<u8, u8>>,
    pending_urbs: Mutex<VecDeque<PendingUrb>>,
}

struct PendingUrb {
    user_urb_ptr: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EndpointTransferType {
    Bulk,
    Interrupt,
    Isochronous,
}

#[derive(Clone, Copy)]
struct ClaimedEndpoint {
    interface: u8,
    alternate: u8,
    transfer_type: EndpointTransferType,
}

impl UsbDeviceFile {
    fn with_live_lease<R>(
        &self,
        f: impl FnOnce(&manager::UsbDeviceLease) -> AxResult<R>,
    ) -> AxResult<R> {
        let mut lease = self.lease.lock();
        if lease.is_none() {
            *lease = Some(self.manager.acquire_device(self.bus_num, self.device_num)?);
        }
        f(lease.as_ref().unwrap())
    }

    fn claim_interface(&self, interface: u8, alternate: u8) -> AxResult<usize> {
        if !snapshot_has_interface(&self.snapshot, interface, alternate) {
            return Err(AxError::NotFound);
        }
        self.with_live_lease(|lease| lease.claim_interface(interface, alternate))?;
        self.claimed_interfaces.lock().insert(interface, alternate);
        Ok(0)
    }

    fn release_interface(&self, interface: u8) -> AxResult<usize> {
        self.claimed_interfaces.lock().remove(&interface);
        Ok(0)
    }

    fn claimed_endpoint(&self, endpoint: u8) -> AxResult<ClaimedEndpoint> {
        let claimed = self.claimed_interfaces.lock();
        snapshot_claimed_endpoint(&self.snapshot, endpoint, &claimed)
            .ok_or(AxError::OperationNotPermitted)
    }

    fn run_endpoint_transfer(
        &self,
        endpoint: u8,
        transfer_type: EndpointTransferType,
        data: *mut u8,
        len: usize,
        iso_packet_lengths: &[usize],
    ) -> AxResult<usize> {
        let claimed_endpoint = self.claimed_endpoint(endpoint)?;
        if claimed_endpoint.transfer_type != transfer_type {
            return Err(AxError::InvalidInput);
        }
        self.with_live_lease(|lease| {
            lease.claim_interface(claimed_endpoint.interface, claimed_endpoint.alternate)?;
            if endpoint & 0x80 != 0 {
                let mut buffer = alloc::vec![0; len];
                let actual = match transfer_type {
                    EndpointTransferType::Bulk => lease.bulk_in(endpoint, &mut buffer)?,
                    EndpointTransferType::Interrupt => lease.interrupt_in(endpoint, &mut buffer)?,
                    EndpointTransferType::Isochronous => {
                        lease.iso_in(endpoint, &mut buffer, iso_packet_lengths)?
                    }
                };
                if actual > len {
                    return Err(AxError::InvalidData);
                }
                if actual > 0 {
                    crate::mm::UserPtr::<u8>::from(data)
                        .get_as_mut_slice(actual)?
                        .copy_from_slice(&buffer[..actual]);
                }
                Ok(actual)
            } else {
                let buffer = crate::mm::UserConstPtr::<u8>::from(data as *const u8)
                    .get_as_slice(len)?
                    .to_vec();
                match transfer_type {
                    EndpointTransferType::Bulk => lease.bulk_out(endpoint, &buffer),
                    EndpointTransferType::Interrupt => lease.interrupt_out(endpoint, &buffer),
                    EndpointTransferType::Isochronous => {
                        lease.iso_out(endpoint, &buffer, iso_packet_lengths)
                    }
                }
            }
        })
    }

    fn bulk_ioctl(&self, arg: usize) -> AxResult<usize> {
        let bulk = descriptor::read_usbdevfs_bulktransfer(arg)?;
        if bulk.ep > u8::MAX as u32 {
            return Err(AxError::InvalidInput);
        }
        self.run_endpoint_transfer(
            bulk.ep as u8,
            EndpointTransferType::Bulk,
            bulk.data,
            bulk.len as usize,
            &[],
        )
    }

    fn read_iso_packet_lengths(&self, urb_ptr: usize, num_packets: usize) -> AxResult<Vec<usize>> {
        let packet_descs = usbdevfs_iso_packet_descs_mut(urb_ptr, num_packets)?;
        let mut total_length = 0usize;
        let mut packet_lengths = Vec::with_capacity(num_packets);
        for packet_desc in packet_descs.iter() {
            let packet_length = packet_desc.length as usize;
            total_length = total_length
                .checked_add(packet_length)
                .ok_or(AxError::OutOfRange)?;
            packet_lengths.push(packet_length);
        }
        Ok(packet_lengths)
    }

    fn write_iso_packet_results(
        &self,
        urb_ptr: usize,
        packet_lengths: &[usize],
        actual_total: usize,
    ) -> AxResult<()> {
        let packet_descs = usbdevfs_iso_packet_descs_mut(urb_ptr, packet_lengths.len())?;
        let mut remaining = actual_total;
        for (packet_desc, packet_length) in packet_descs.iter_mut().zip(packet_lengths.iter()) {
            let packet_actual = remaining.min(*packet_length);
            packet_desc.actual_length = packet_actual as u32;
            packet_desc.status = 0;
            remaining -= packet_actual;
        }
        Ok(())
    }

    fn submit_control_urb(&self, arg: usize) -> AxResult<usize> {
        let urb = crate::mm::UserPtr::<descriptor::UsbdevfsUrb>::from(arg).get_as_mut()?;
        if urb.type_ != descriptor::USBDEVFS_URB_TYPE_CONTROL {
            return Err(ax_errno::AxError::Unsupported);
        }
        if urb.buffer_length < 8 {
            return Err(ax_errno::AxError::InvalidInput);
        }

        let transfer = crate::mm::UserPtr::<u8>::from(urb.buffer)
            .get_as_mut_slice(urb.buffer_length as usize)?;
        let b_request_type = transfer[0];
        let b_request = transfer[1];
        let w_value = u16::from_le_bytes([transfer[2], transfer[3]]);
        let w_index = u16::from_le_bytes([transfer[4], transfer[5]]);
        let w_length = u16::from_le_bytes([transfer[6], transfer[7]]) as usize;
        if transfer.len() < 8 + w_length {
            return Err(ax_errno::AxError::InvalidInput);
        }

        let actual = self.with_live_lease(|lease| {
            lease.control_transfer(
                b_request_type,
                b_request,
                w_value,
                w_index,
                &mut transfer[8..8 + w_length],
            )
        })?;
        urb.status = 0;
        urb.actual_length = actual as i32;
        self.pending_urbs
            .lock()
            .push_back(PendingUrb { user_urb_ptr: arg });
        Ok(0)
    }

    fn submit_bulk_urb(&self, arg: usize) -> AxResult<usize> {
        let urb = crate::mm::UserPtr::<descriptor::UsbdevfsUrb>::from(arg).get_as_mut()?;
        if urb.type_ != descriptor::USBDEVFS_URB_TYPE_BULK {
            return Err(ax_errno::AxError::Unsupported);
        }
        if urb.buffer_length < 0 {
            return Err(ax_errno::AxError::InvalidInput);
        }

        let actual = self.run_endpoint_transfer(
            urb.endpoint,
            EndpointTransferType::Bulk,
            urb.buffer,
            urb.buffer_length as usize,
            &[],
        )?;
        urb.status = 0;
        urb.actual_length = actual as i32;
        self.pending_urbs
            .lock()
            .push_back(PendingUrb { user_urb_ptr: arg });
        Ok(0)
    }

    fn submit_interrupt_urb(&self, arg: usize) -> AxResult<usize> {
        let urb = crate::mm::UserPtr::<descriptor::UsbdevfsUrb>::from(arg).get_as_mut()?;
        if urb.type_ != descriptor::USBDEVFS_URB_TYPE_INTERRUPT {
            return Err(ax_errno::AxError::Unsupported);
        }
        if urb.buffer_length < 0 {
            return Err(ax_errno::AxError::InvalidInput);
        }

        let actual = self.run_endpoint_transfer(
            urb.endpoint,
            EndpointTransferType::Interrupt,
            urb.buffer,
            urb.buffer_length as usize,
            &[],
        )?;
        urb.status = 0;
        urb.actual_length = actual as i32;
        self.pending_urbs
            .lock()
            .push_back(PendingUrb { user_urb_ptr: arg });
        Ok(0)
    }

    fn submit_iso_urb(&self, arg: usize) -> AxResult<usize> {
        let urb = crate::mm::UserPtr::<descriptor::UsbdevfsUrb>::from(arg).get_as_mut()?;
        if urb.type_ != descriptor::USBDEVFS_URB_TYPE_ISO {
            return Err(ax_errno::AxError::Unsupported);
        }
        if urb.buffer_length < 0 || urb.number_of_packets <= 0 {
            return Err(ax_errno::AxError::InvalidInput);
        }
        let supported_flags =
            descriptor::USBDEVFS_URB_ISO_ASAP | descriptor::USBDEVFS_URB_SHORT_NOT_OK;
        if urb.flags & !supported_flags != 0 {
            return Err(AxError::Unsupported);
        }
        if urb.flags & descriptor::USBDEVFS_URB_ISO_ASAP == 0 && urb.start_frame != 0 {
            return Err(AxError::Unsupported);
        }

        let packet_lengths = self.read_iso_packet_lengths(arg, urb.number_of_packets as usize)?;
        let total_length = packet_lengths.iter().try_fold(0usize, |acc, len| {
            acc.checked_add(*len).ok_or(AxError::OutOfRange)
        })?;
        if total_length > urb.buffer_length as usize {
            return Err(AxError::InvalidInput);
        }

        let actual = self.run_endpoint_transfer(
            urb.endpoint,
            EndpointTransferType::Isochronous,
            urb.buffer,
            total_length,
            &packet_lengths,
        )?;
        self.write_iso_packet_results(arg, &packet_lengths, actual)?;
        urb.status = 0;
        urb.actual_length = actual as i32;
        urb.error_count = 0;
        self.pending_urbs
            .lock()
            .push_back(PendingUrb { user_urb_ptr: arg });
        Ok(0)
    }

    fn submit_urb(&self, arg: usize) -> AxResult<usize> {
        let type_ = crate::mm::UserPtr::<descriptor::UsbdevfsUrb>::from(arg)
            .get_as_mut()?
            .type_;
        match type_ {
            descriptor::USBDEVFS_URB_TYPE_CONTROL => self.submit_control_urb(arg),
            descriptor::USBDEVFS_URB_TYPE_BULK => self.submit_bulk_urb(arg),
            descriptor::USBDEVFS_URB_TYPE_INTERRUPT => self.submit_interrupt_urb(arg),
            descriptor::USBDEVFS_URB_TYPE_ISO => self.submit_iso_urb(arg),
            _ => Err(ax_errno::AxError::Unsupported),
        }
    }

    fn reap_urb(&self, arg: usize) -> AxResult<usize> {
        let Some(pending) = self.pending_urbs.lock().pop_front() else {
            return Err(ax_errno::AxError::WouldBlock);
        };
        (arg as *mut usize).vm_write(pending.user_urb_ptr)?;
        Ok(0)
    }
}

impl FileLike for UsbDeviceFile {
    fn read(&self, dst: &mut IoDst) -> AxResult<usize> {
        self.base.read(dst)
    }

    fn write(&self, src: &mut IoSrc) -> AxResult<usize> {
        self.base.write(src)
    }

    fn stat(&self) -> AxResult<Kstat> {
        self.base.stat()
    }

    fn path(&self) -> alloc::borrow::Cow<'_, str> {
        self.base.path()
    }

    fn file_mmap(&self) -> AxResult<(ax_fs::FileBackend, ax_fs::FileFlags)> {
        self.base.file_mmap()
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> AxResult<usize> {
        match cmd {
            descriptor::USBDEVFS_CONTROL => {
                let lease = self.lease.lock();
                if let Some(lease) = lease.as_ref() {
                    return lease.ioctl(cmd, arg);
                }
                drop(lease);
                self.manager
                    .snapshot_device_ioctl(self.bus_num, self.device_num, cmd, arg)
            }
            descriptor::USBDEVFS_CLAIMINTERFACE => {
                let interface = descriptor::read_usbdevfs_u32(arg)?;
                if interface > u8::MAX as u32 {
                    return Err(AxError::InvalidInput);
                }
                self.claim_interface(interface as u8, 0)
            }
            descriptor::USBDEVFS_RELEASEINTERFACE => {
                let interface = descriptor::read_usbdevfs_u32(arg)?;
                if interface > u8::MAX as u32 {
                    return Err(AxError::InvalidInput);
                }
                self.release_interface(interface as u8)
            }
            descriptor::USBDEVFS_SETINTERFACE => {
                let set = descriptor::read_usbdevfs_setinterface(arg)?;
                if set.interface > u8::MAX as u32 || set.altsetting > u8::MAX as u32 {
                    return Err(AxError::InvalidInput);
                }
                self.claim_interface(set.interface as u8, set.altsetting as u8)
            }
            descriptor::USBDEVFS_BULK => self.bulk_ioctl(arg),
            descriptor::USBDEVFS_SUBMITURB => self.submit_urb(arg),
            descriptor::USBDEVFS_REAPURB | descriptor::USBDEVFS_REAPURBNDELAY => self.reap_urb(arg),
            descriptor::USBDEVFS_CONNECTINFO | descriptor::USBDEVFS_GET_CAPABILITIES => {
                self.with_live_lease(|lease| lease.ioctl(cmd, arg))
            }
            _ => self.with_live_lease(|lease| lease.ioctl(cmd, arg)),
        }
    }

    fn open_flags(&self) -> u32 {
        self.base.open_flags()
    }

    fn nonblocking(&self) -> bool {
        self.base.nonblocking()
    }

    fn set_nonblocking(&self, flag: bool) -> AxResult {
        self.base.set_nonblocking(flag)
    }
}

impl Pollable for UsbDeviceFile {
    fn poll(&self) -> IoEvents {
        let mut events = IoEvents::OUT;
        if !self.pending_urbs.lock().is_empty() {
            events |= IoEvents::IN;
        }
        events
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}

fn snapshot_has_interface(
    snapshot: &descriptor::UsbDeviceSnapshot,
    interface_number: u8,
    alternate_setting: u8,
) -> bool {
    let mut cursor = 18usize;
    while cursor + 2 <= snapshot.descriptor_blob.len() {
        let length = snapshot.descriptor_blob[cursor] as usize;
        if length < 2 || cursor + length > snapshot.descriptor_blob.len() {
            return false;
        }
        if snapshot.descriptor_blob[cursor + 1] == 0x04
            && length >= 9
            && snapshot.descriptor_blob[cursor + 2] == interface_number
            && snapshot.descriptor_blob[cursor + 3] == alternate_setting
        {
            return true;
        }
        cursor += length;
    }
    false
}

fn snapshot_claimed_endpoint(
    snapshot: &descriptor::UsbDeviceSnapshot,
    endpoint: u8,
    claimed_interfaces: &alloc::collections::BTreeMap<u8, u8>,
) -> Option<ClaimedEndpoint> {
    let mut cursor = 18usize;
    let mut current_interface = None;
    let mut current_alternate = 0u8;

    while cursor + 2 <= snapshot.descriptor_blob.len() {
        let length = snapshot.descriptor_blob[cursor] as usize;
        if length < 2 || cursor + length > snapshot.descriptor_blob.len() {
            return None;
        }

        match snapshot.descriptor_blob[cursor + 1] {
            0x04 if length >= 9 => {
                current_interface = Some(snapshot.descriptor_blob[cursor + 2]);
                current_alternate = snapshot.descriptor_blob[cursor + 3];
            }
            0x05 if length >= 7 && snapshot.descriptor_blob[cursor + 2] == endpoint => {
                let interface = current_interface?;
                if claimed_interfaces.get(&interface).copied() == Some(current_alternate) {
                    let transfer_type = match snapshot.descriptor_blob[cursor + 3] & 0x03 {
                        1 => EndpointTransferType::Isochronous,
                        2 => EndpointTransferType::Bulk,
                        3 => EndpointTransferType::Interrupt,
                        _ => return None,
                    };
                    return Some(ClaimedEndpoint {
                        interface,
                        alternate: current_alternate,
                        transfer_type,
                    });
                }
            }
            _ => {}
        }

        cursor += length;
    }

    None
}

fn usbdevfs_iso_packet_descs_mut(
    urb_ptr: usize,
    num_packets: usize,
) -> AxResult<&'static mut [descriptor::UsbdevfsIsoPacketDesc]> {
    let offset = urb_ptr
        .checked_add(size_of::<descriptor::UsbdevfsUrb>())
        .ok_or(AxError::OutOfRange)?;
    crate::mm::UserPtr::<descriptor::UsbdevfsIsoPacketDesc>::from(offset)
        .get_as_mut_slice(num_packets)
}
