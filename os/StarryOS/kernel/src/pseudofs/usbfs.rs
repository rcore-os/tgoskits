#![cfg_attr(not(target_os = "none"), allow(dead_code, unused_imports))]

use alloc::{
    borrow::{Cow, ToOwned},
    boxed::Box,
    collections::BTreeMap,
    format,
    string::String,
    sync::Arc,
    vec::Vec,
};
use core::{any::Any, mem::size_of, task::Context};

use ax_errno::{AxError, LinuxResult};
use axfs_ng_vfs::{DeviceId, Filesystem, NodeFlags, NodeType, VfsResult};
use axpoll::{IoEvents, Pollable};
use crab_usb::{DeviceInfo, Event, EventHandler, usb_if};
use event_listener::{Event as NotifyEvent, listener};
use linux_raw_sys::general::{
    _IOC_DIRSHIFT, _IOC_NRSHIFT, _IOC_READ, _IOC_SIZESHIFT, _IOC_TYPESHIFT, _IOC_WRITE,
};
use rdrive::DeviceId as RDriveDeviceId;
use spin::Mutex as SpinMutex;
use starry_vm::{VmMutPtr, VmPtr};

use crate::{
    mm::UserConstPtr,
    pseudofs::{Device, DeviceOps, NodeOpsMux, SimpleDir, SimpleDirOps, SimpleFs},
};

const USBFS_MAGIC: u32 = 0x9fa2;
const USB_MAJOR: u32 = 189;
const USBDEVFS_CAP_BULK_CONTINUATION: u32 = 0x02;
const USB_REQ_GET_CONFIGURATION: u8 = 0x08;
const USB_REQTYPE_DEVICE_TO_HOST_STANDARD_DEVICE: u8 = 0x80;

lazy_static::lazy_static! {
    static ref USBFS_MANAGER: SpinMutex<Option<Arc<UsbFsManager>>> = SpinMutex::new(None);
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct UsbdevfsCtrlTransfer {
    b_request_type: u8,
    b_request: u8,
    w_value: u16,
    w_index: u16,
    w_length: u16,
    timeout: u32,
    data: *mut u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct UsbdevfsConnectInfo {
    devnum: u32,
    slow: u8,
    _padding: [u8; 3],
}

const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
    (dir << _IOC_DIRSHIFT)
        | ((ty as u32) << _IOC_TYPESHIFT)
        | ((nr as u32) << _IOC_NRSHIFT)
        | ((size as u32) << _IOC_SIZESHIFT)
}

const fn ior<T>(ty: u8, nr: u8) -> u32 {
    ioc(_IOC_READ, ty, nr, size_of::<T>())
}

const fn iowr<T>(ty: u8, nr: u8) -> u32 {
    ioc(_IOC_READ | _IOC_WRITE, ty, nr, size_of::<T>())
}

const USBDEVFS_CONTROL: u32 = iowr::<UsbdevfsCtrlTransfer>(b'U', 0);
const USBDEVFS_CONNECTINFO: u32 = ior::<UsbdevfsConnectInfo>(b'U', 17);
const USBDEVFS_GET_CAPABILITIES: u32 = ior::<u32>(b'U', 26);

#[derive(Clone)]
struct UsbDeviceSnapshot {
    bus_num: u8,
    device_num: u8,
    active_configuration: u8,
    descriptor_blob: Vec<u8>,
}

struct UsbHostState {
    device_id: RDriveDeviceId,
    bus_num: u8,
    irq_num: Option<usize>,
    event_handler: EventHandler,
    dirty: bool,
    next_device_num: u8,
    stable_id_to_device_num: BTreeMap<usize, u8>,
}

#[derive(Default)]
struct UsbFsState {
    hosts: Vec<UsbHostState>,
    devices: BTreeMap<(u8, u8), UsbDeviceSnapshot>,
}

struct UsbFsManager {
    state: SpinMutex<UsbFsState>,
    refresh_event: NotifyEvent,
}

impl UsbFsManager {
    fn new() -> Self {
        Self {
            state: SpinMutex::new(UsbFsState::default()),
            refresh_event: NotifyEvent::new(),
        }
    }

    fn initialize(&self) {
        #[cfg(not(target_os = "none"))]
        return;
        #[cfg(target_os = "none")]
        {
            let hosts = rdrive::get_list::<axplat_dyn::drivers::usb::PlatformUsbHost>();
            let mut initialized_hosts = Vec::new();

            for (index, host) in hosts.into_iter().enumerate() {
                let device_id = host.descriptor().device_id();
                let bus_num = (index + 1) as u8;
                info!("usbfs: preparing host {:?} as bus {}", device_id, bus_num);

                let mut guard = match host.lock() {
                    Ok(guard) => guard,
                    Err(err) => {
                        warn!("usbfs: failed to lock USB host {device_id:?}: {err:?}");
                        continue;
                    }
                };

                let irq_num = guard.irq_num();
                info!("usbfs: creating event handler for bus {}", bus_num);
                let event_handler = guard.host_mut().create_event_handler();
                info!("usbfs: initializing host on bus {}", bus_num);
                if let Err(err) = ax_task::future::block_on(guard.host_mut().init()) {
                    warn!("usbfs: failed to initialize USB host on bus {bus_num}: {err:?}");
                    continue;
                }
                info!("usbfs: host on bus {} initialized", bus_num);

                drop(guard);

                let host_state = UsbHostState {
                    device_id,
                    bus_num,
                    irq_num,
                    event_handler,
                    dirty: true,
                    next_device_num: 1,
                    stable_id_to_device_num: BTreeMap::new(),
                };
                initialized_hosts.push(host_state);
            }

            {
                let mut state = self.state.lock();
                state.hosts = initialized_hosts;
            }
            info!("usbfs: {} host(s) ready", self.state.lock().hosts.len());

            {
                let irqs = {
                    let state = self.state.lock();
                    state
                        .hosts
                        .iter()
                        .filter_map(|host| host.irq_num)
                        .collect::<Vec<_>>()
                };
                for irq_num in irqs {
                    info!("usbfs: registering IRQ callback for IRQ {}", irq_num);
                    if !ax_hal::irq::register(irq_num, usbfs_irq_handler) {
                        warn!("usbfs: failed to register IRQ callback for IRQ {}", irq_num);
                    }
                }
            }
        }
    }

    fn handle_irq(&self) {
        let mut should_refresh = false;
        let mut state = self.state.lock();
        for host in &mut state.hosts {
            if host.irq_num.is_none() {
                continue;
            }
            host.dirty = true;
            should_refresh = true;
        }
        drop(state);

        if should_refresh {
            self.refresh_event.notify(1);
        }
    }

    fn refresh_dirty_hosts(&self) {
        #[cfg(not(target_os = "none"))]
        return;
        #[cfg(target_os = "none")]
        {
            let dirty_hosts = {
                let mut state = self.state.lock();
                let mut dirty = Vec::new();
                for host in &mut state.hosts {
                    if host.dirty {
                        host.dirty = false;
                        dirty.push((host.device_id, host.bus_num));
                    }
                }
                dirty
            };

            for (device_id, bus_num) in dirty_hosts {
                let host = match rdrive::get::<axplat_dyn::drivers::usb::PlatformUsbHost>(device_id)
                {
                    Ok(host) => host,
                    Err(err) => {
                        warn!(
                            "usbfs: failed to reacquire USB host {:?}: {err:?}",
                            device_id
                        );
                        continue;
                    }
                };

                let mut guard = match host.lock() {
                    Ok(guard) => guard,
                    Err(err) => {
                        warn!("usbfs: failed to lock USB host {:?}: {err:?}", device_id);
                        continue;
                    }
                };

                let devices = match ax_task::future::block_on(guard.host_mut().probe_devices()) {
                    Ok(devices) => devices,
                    Err(err) => {
                        warn!("usbfs: refresh probe failed on bus {bus_num}: {err:?}");
                        continue;
                    }
                };
                drop(guard);

            let mut state = self.state.lock();
            let Some(host_index) = state.hosts.iter().position(|host| host.device_id == device_id) else {
                continue;
            };
            loop {
                match state.hosts[host_index].event_handler.handle_event() {
                    Event::PortChange { .. } | Event::Stopped => {}
                    Event::Nothing => break,
                }
            }
            let mut new_snapshots = Vec::new();
            for info in devices {
                if let Some(snapshot) = snapshot_device_info(bus_num, &mut state.hosts[host_index], &info) {
                    new_snapshots.push(snapshot);
                }
            }
            for snapshot in new_snapshots {
                state
                    .devices
                    .insert((snapshot.bus_num, snapshot.device_num), snapshot);
            }
        }
    }
    }

    fn bus_numbers(&self) -> Vec<u8> {
        let state = self.state.lock();
        state.hosts.iter().map(|host| host.bus_num).collect()
    }

    fn device_numbers(&self, bus_num: u8) -> Vec<u8> {
        let state = self.state.lock();
        state
            .devices
            .values()
            .filter(|snapshot| snapshot.bus_num == bus_num)
            .map(|snapshot| snapshot.device_num)
            .collect()
    }

    fn device_snapshot(&self, bus_num: u8, device_num: u8) -> Option<UsbDeviceSnapshot> {
        self.state
            .lock()
            .devices
            .get(&(bus_num, device_num))
            .cloned()
    }
}

struct UsbRootDir {
    fs: Arc<SimpleFs>,
    manager: Arc<UsbFsManager>,
}

impl SimpleDirOps for UsbRootDir {
    fn is_cacheable(&self) -> bool {
        false
    }

    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let mut names = self
            .manager
            .bus_numbers()
            .into_iter()
            .map(bus_name)
            .collect::<Vec<_>>();
        names.sort();
        Box::new(names.into_iter().map(Cow::Owned))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let Some(bus_num) = parse_numeric_component(name) else {
            return Err(AxError::NotFound.into());
        };
        if !self.manager.bus_numbers().contains(&bus_num) {
            return Err(AxError::NotFound.into());
        }

        let fs = self.fs.clone();
        let manager = self.manager.clone();
        Ok(NodeOpsMux::Dir(SimpleDir::new_maker(
            fs.clone(),
            Arc::new(UsbBusDir {
                fs,
                manager,
                bus_num,
            }),
        )))
    }
}

struct UsbBusDir {
    fs: Arc<SimpleFs>,
    manager: Arc<UsbFsManager>,
    bus_num: u8,
}

impl SimpleDirOps for UsbBusDir {
    fn is_cacheable(&self) -> bool {
        false
    }

    fn child_names<'a>(&'a self) -> Box<dyn Iterator<Item = Cow<'a, str>> + 'a> {
        let mut names = self
            .manager
            .device_numbers(self.bus_num)
            .into_iter()
            .map(device_name)
            .collect::<Vec<_>>();
        names.sort();
        Box::new(names.into_iter().map(Cow::Owned))
    }

    fn lookup_child(&self, name: &str) -> VfsResult<NodeOpsMux> {
        let Some(device_num) = parse_numeric_component(name) else {
            return Err(AxError::NotFound.into());
        };
        if self
            .manager
            .device_snapshot(self.bus_num, device_num)
            .is_none()
        {
            return Err(AxError::NotFound.into());
        }

        Ok(NodeOpsMux::File(Device::new(
            self.fs.clone(),
            NodeType::CharacterDevice,
            usb_device_id(self.bus_num, device_num),
            Arc::new(UsbDeviceOps {
                manager: self.manager.clone(),
                bus_num: self.bus_num,
                device_num,
            }),
        )))
    }
}

struct UsbDeviceOps {
    manager: Arc<UsbFsManager>,
    bus_num: u8,
    device_num: u8,
}

impl DeviceOps for UsbDeviceOps {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> VfsResult<usize> {
        let snapshot = self
            .manager
            .device_snapshot(self.bus_num, self.device_num)
            .ok_or(AxError::NotFound)?;
        let offset = offset as usize;
        if offset >= snapshot.descriptor_blob.len() {
            return Ok(0);
        }
        let data = &snapshot.descriptor_blob[offset..];
        let len = data.len().min(buf.len());
        buf[..len].copy_from_slice(&data[..len]);
        Ok(len)
    }

    fn write_at(&self, _buf: &[u8], _offset: u64) -> VfsResult<usize> {
        Err(AxError::InvalidInput.into())
    }

    fn ioctl(&self, cmd: u32, arg: usize) -> VfsResult<usize> {
        let snapshot = self
            .manager
            .device_snapshot(self.bus_num, self.device_num)
            .ok_or(AxError::NotFound)?;
        match cmd {
            USBDEVFS_CONTROL => {
                let ctrl = read_usbdevfs_ctrltransfer(arg)?;
                if ctrl.b_request_type == USB_REQTYPE_DEVICE_TO_HOST_STANDARD_DEVICE
                    && ctrl.b_request == USB_REQ_GET_CONFIGURATION
                    && ctrl.w_length >= 1
                {
                    (ctrl.data as *mut u8).vm_write(snapshot.active_configuration)?;
                    Ok(1)
                } else {
                    Err(AxError::Unsupported.into())
                }
            }
            USBDEVFS_CONNECTINFO => {
                (arg as *mut UsbdevfsConnectInfo).vm_write(UsbdevfsConnectInfo {
                    devnum: snapshot.device_num as u32,
                    slow: 0,
                    _padding: [0; 3],
                })?;
                Ok(0)
            }
            USBDEVFS_GET_CAPABILITIES => {
                (arg as *mut u32).vm_write(USBDEVFS_CAP_BULK_CONTINUATION)?;
                Ok(0)
            }
            _ => Err(AxError::Unsupported.into()),
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn flags(&self) -> NodeFlags {
        NodeFlags::NON_CACHEABLE | NodeFlags::STREAM
    }

    fn as_pollable(&self) -> Option<&dyn Pollable> {
        Some(self)
    }
}

fn read_usbdevfs_ctrltransfer(arg: usize) -> VfsResult<UsbdevfsCtrlTransfer> {
    let bytes = UserConstPtr::<u8>::from(arg).get_as_slice(size_of::<UsbdevfsCtrlTransfer>())?;
    let mut index = 0usize;
    let read_u8 = |bytes: &[u8], index: &mut usize| {
        let value = bytes[*index];
        *index += 1;
        value
    };
    let read_u16 = |bytes: &[u8], index: &mut usize| {
        let value = u16::from_le_bytes([bytes[*index], bytes[*index + 1]]);
        *index += 2;
        value
    };
    let read_u32 = |bytes: &[u8], index: &mut usize| {
        let value = u32::from_le_bytes([
            bytes[*index],
            bytes[*index + 1],
            bytes[*index + 2],
            bytes[*index + 3],
        ]);
        *index += 4;
        value
    };
    let read_usize = |bytes: &[u8], index: &mut usize| {
        let mut raw = [0u8; size_of::<usize>()];
        raw.copy_from_slice(&bytes[*index..*index + size_of::<usize>()]);
        *index += size_of::<usize>();
        usize::from_le_bytes(raw)
    };

    let b_request_type = read_u8(bytes, &mut index);
    let b_request = read_u8(bytes, &mut index);
    let w_value = read_u16(bytes, &mut index);
    let w_index = read_u16(bytes, &mut index);
    let w_length = read_u16(bytes, &mut index);
    let timeout = read_u32(bytes, &mut index);
    let data = read_usize(bytes, &mut index) as *mut u8;

    Ok(UsbdevfsCtrlTransfer {
        b_request_type,
        b_request,
        w_value,
        w_index,
        w_length,
        timeout,
        data,
    })
}

impl Pollable for UsbDeviceOps {
    fn poll(&self) -> IoEvents {
        IoEvents::IN | IoEvents::OUT
    }

    fn register(&self, _context: &mut Context<'_>, _events: IoEvents) {}
}

fn snapshot_device_info(
    bus_num: u8,
    host_state: &mut UsbHostState,
    info: &DeviceInfo,
) -> Option<UsbDeviceSnapshot> {
    let stable_id = info.id();
    let device_num = match host_state.stable_id_to_device_num.get(&stable_id).copied() {
        Some(device_num) => device_num,
        None => {
            let device_num = host_state.next_device_num;
            host_state.next_device_num = host_state.next_device_num.saturating_add(1);
            host_state
                .stable_id_to_device_num
                .insert(stable_id, device_num);
            device_num
        }
    };

    Some(UsbDeviceSnapshot {
        bus_num,
        device_num,
        active_configuration: info
            .configurations()
            .first()
            .map(|config| config.configuration_value)
            .unwrap_or(0),
        descriptor_blob: serialize_descriptor_blob(info),
    })
}

fn serialize_descriptor_blob(info: &DeviceInfo) -> Vec<u8> {
    let mut out = Vec::new();
    let desc = info.descriptor();
    out.push(18);
    out.push(0x01);
    out.extend_from_slice(&desc.usb_version.to_le_bytes());
    out.push(desc.class);
    out.push(desc.subclass);
    out.push(desc.protocol);
    out.push(desc.max_packet_size_0);
    out.extend_from_slice(&desc.vendor_id.to_le_bytes());
    out.extend_from_slice(&desc.product_id.to_le_bytes());
    out.extend_from_slice(&desc.device_version.to_le_bytes());
    out.push(
        desc.manufacturer_string_index
            .map(|index| index.get())
            .unwrap_or(0),
    );
    out.push(
        desc.product_string_index
            .map(|index| index.get())
            .unwrap_or(0),
    );
    out.push(
        desc.serial_number_string_index
            .map(|index| index.get())
            .unwrap_or(0),
    );
    out.push(desc.num_configurations);

    for config in info.configurations() {
        let mut config_blob = Vec::new();
        for interface in &config.interfaces {
            for alt in &interface.alt_settings {
                config_blob.push(9);
                config_blob.push(0x04);
                config_blob.push(alt.interface_number);
                config_blob.push(alt.alternate_setting);
                config_blob.push(alt.num_endpoints);
                config_blob.push(alt.class);
                config_blob.push(alt.subclass);
                config_blob.push(alt.protocol);
                config_blob.push(alt.string_index.map(|index| index.get()).unwrap_or(0));

                for endpoint in &alt.endpoints {
                    config_blob.push(7);
                    config_blob.push(0x05);
                    config_blob.push(endpoint.address);
                    config_blob.push(endpoint_attributes(endpoint.transfer_type));
                    config_blob.extend_from_slice(&endpoint.max_packet_size.to_le_bytes());
                    config_blob.push(endpoint.interval);
                }
            }
        }

        let total_length = (9 + config_blob.len()) as u16;
        out.push(9);
        out.push(0x02);
        out.extend_from_slice(&total_length.to_le_bytes());
        out.push(config.num_interfaces);
        out.push(config.configuration_value);
        out.push(config.string_index.map(|index| index.get()).unwrap_or(0));
        out.push(config.attributes);
        out.push(config.max_power);
        out.extend_from_slice(&config_blob);
    }

    out
}

fn endpoint_attributes(transfer_type: usb_if::descriptor::EndpointType) -> u8 {
    match transfer_type {
        usb_if::descriptor::EndpointType::Control => 0,
        usb_if::descriptor::EndpointType::Isochronous => 1,
        usb_if::descriptor::EndpointType::Bulk => 2,
        usb_if::descriptor::EndpointType::Interrupt => 3,
    }
}

fn bus_name(bus_num: u8) -> String {
    format!("{bus_num:03}")
}

fn device_name(device_num: u8) -> String {
    format!("{device_num:03}")
}

fn parse_numeric_component(name: &str) -> Option<u8> {
    if name.len() != 3 {
        return None;
    }
    name.parse().ok()
}

fn usb_device_id(bus_num: u8, device_num: u8) -> DeviceId {
    let minor = ((bus_num.saturating_sub(1) as u32) * 128) + device_num.saturating_sub(1) as u32;
    DeviceId::new(USB_MAJOR, minor)
}

fn manager() -> Option<Arc<UsbFsManager>> {
    USBFS_MANAGER.lock().as_ref().cloned()
}

fn usbfs_irq_handler() {
    if let Some(manager) = manager() {
        manager.handle_irq();
    }
}

async fn usbfs_refresh_task(manager: Arc<UsbFsManager>) {
    loop {
        listener!(manager.refresh_event => refresh_listener);
        refresh_listener.await;
        manager.refresh_dirty_hosts();
    }
}

pub(crate) fn new_usbfs() -> LinuxResult<Filesystem> {
    let mut should_spawn_refresh = false;
    if manager().is_none() {
        info!("usbfs: initializing manager");
        let manager = Arc::new(UsbFsManager::new());
        manager.initialize();
        should_spawn_refresh = !manager.state.lock().hosts.is_empty();
        *USBFS_MANAGER.lock() = Some(manager);
    }
    let manager = manager().expect("usbfs manager must be initialized");
    if should_spawn_refresh {
        info!("usbfs: spawning refresh task");
        let refresh_manager = manager.clone();
        ax_task::spawn_with_name(
            move || ax_task::future::block_on(usbfs_refresh_task(refresh_manager.clone())),
            "usbfs-refresh".to_owned(),
        );
        manager.refresh_event.notify(1);
    }
    info!("usbfs: creating filesystem instance");
    Ok(SimpleFs::new_with("usbfs".into(), USBFS_MAGIC, move |fs| {
        SimpleDir::new_maker(
            fs.clone(),
            Arc::new(UsbRootDir {
                fs: fs.clone(),
                manager: manager.clone(),
            }),
        )
    }))
}
