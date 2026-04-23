#![cfg_attr(not(target_os = "none"), allow(dead_code, unused_imports))]

mod descriptor;
mod irq;
mod manager;
mod tree;

use alloc::{borrow::ToOwned, collections::VecDeque, sync::Arc};
use core::{any::Any, task::Context};

use ax_errno::{AxResult, LinuxResult};
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
    Ok(Arc::new(UsbDeviceFile {
        base: KernelFile::new(file, open_flags),
        manager,
        bus_num: ops.bus_num,
        device_num: ops.device_num,
        lease: Mutex::new(None),
        pending_urbs: Mutex::new(VecDeque::new()),
    }))
}

struct UsbDeviceFile {
    base: KernelFile,
    manager: Arc<UsbFsManager>,
    bus_num: u8,
    device_num: u8,
    lease: Mutex<Option<manager::UsbDeviceLease>>,
    pending_urbs: Mutex<VecDeque<PendingUrb>>,
}

struct PendingUrb {
    user_urb_ptr: usize,
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
            descriptor::USBDEVFS_SUBMITURB => self.submit_control_urb(arg),
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
