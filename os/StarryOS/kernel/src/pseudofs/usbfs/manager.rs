use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

use ax_kspin::SpinNoIrq;
use crab_usb::EventHandler;
use event_listener::{Event as NotifyEvent, listener};
use rdrive::DeviceId as RDriveDeviceId;

use super::{
    descriptor::{UsbDeviceSnapshot, snapshot_device_info},
    irq::{self, PendingUsbIrqSlot},
};

pub(super) struct UsbHostState {
    pub(super) device_id: RDriveDeviceId,
    pub(super) bus_num: u8,
    pub(super) irq_num: Option<usize>,
    pub(super) needs_probe: bool,
    pub(super) next_device_num: u8,
    pub(super) stable_id_to_device_num: BTreeMap<usize, u8>,
}

#[derive(Default)]
struct UsbFsState {
    hosts: Vec<UsbHostState>,
    devices: BTreeMap<(u8, u8), UsbDeviceSnapshot>,
}

pub(super) struct UsbFsManager {
    state: SpinNoIrq<UsbFsState>,
    pub(super) refresh_event: NotifyEvent,
}

impl UsbFsManager {
    pub(super) fn new(hosts: Vec<UsbHostState>) -> Self {
        Self {
            state: SpinNoIrq::new(UsbFsState {
                hosts,
                devices: BTreeMap::new(),
            }),
            refresh_event: NotifyEvent::new(),
        }
    }

    pub(super) fn refresh_dirty_hosts(&self) {
        #[cfg(not(target_os = "none"))]
        return;
        #[cfg(target_os = "none")]
        {
            let pending_hosts = {
                let mut state = self.state.lock();
                let mut pending = Vec::new();
                for host in &mut state.hosts {
                    let irq_dirty = host.irq_num.map(irq::take_dirty).unwrap_or(false);
                    if host.needs_probe || irq_dirty {
                        host.needs_probe = false;
                        pending.push((host.device_id, host.bus_num));
                    }
                }
                pending
            };

            for (device_id, bus_num) in pending_hosts {
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
                let Some(host_index) = state
                    .hosts
                    .iter()
                    .position(|host| host.device_id == device_id)
                else {
                    continue;
                };

                state
                    .devices
                    .retain(|(snapshot_bus_num, _), _| *snapshot_bus_num != bus_num);

                let snapshots = {
                    let host_state = &mut state.hosts[host_index];
                    devices
                        .into_iter()
                        .map(|info| {
                            snapshot_device_info(
                                bus_num,
                                &mut host_state.next_device_num,
                                &mut host_state.stable_id_to_device_num,
                                &info,
                            )
                        })
                        .collect::<Vec<_>>()
                };
                for snapshot in snapshots {
                    state
                        .devices
                        .insert((snapshot.bus_num, snapshot.device_num), snapshot);
                }
            }
        }
    }

    pub(super) fn bus_numbers(&self) -> Vec<u8> {
        let state = self.state.lock();
        state.hosts.iter().map(|host| host.bus_num).collect()
    }

    pub(super) fn device_numbers(&self, bus_num: u8) -> Vec<u8> {
        let state = self.state.lock();
        state
            .devices
            .values()
            .filter(|snapshot| snapshot.bus_num == bus_num)
            .map(|snapshot| snapshot.device_num)
            .collect()
    }

    pub(super) fn device_snapshot(&self, bus_num: u8, device_num: u8) -> Option<UsbDeviceSnapshot> {
        self.state
            .lock()
            .devices
            .get(&(bus_num, device_num))
            .cloned()
    }
}

pub(super) async fn usbfs_refresh_task(manager: Arc<UsbFsManager>) {
    loop {
        listener!(manager.refresh_event => refresh_listener);
        refresh_listener.await;
        manager.refresh_dirty_hosts();
    }
}

pub(super) fn initialize_hosts(manager: &UsbFsManager) -> usize {
    #[cfg(not(target_os = "none"))]
    {
        let _ = manager;
        0
    }
    #[cfg(target_os = "none")]
    {
        let hosts = {
            let state = manager.state.lock();
            state
                .hosts
                .iter()
                .map(|host| (host.device_id, host.bus_num, host.irq_num))
                .collect::<Vec<_>>()
        };

        let mut initialized = 0usize;
        let mut failed_device_ids = Vec::new();

        for (device_id, bus_num, irq_num) in hosts {
            let host = match rdrive::get::<axplat_dyn::drivers::usb::PlatformUsbHost>(device_id) {
                Ok(host) => host,
                Err(err) => {
                    warn!(
                        "usbfs: failed to reacquire USB host {:?} for init: {err:?}",
                        device_id
                    );
                    failed_device_ids.push((device_id, irq_num));
                    continue;
                }
            };

            let mut guard = match host.lock() {
                Ok(guard) => guard,
                Err(err) => {
                    warn!(
                        "usbfs: failed to lock USB host {:?} for init: {err:?}",
                        device_id
                    );
                    failed_device_ids.push((device_id, irq_num));
                    continue;
                }
            };

            info!("usbfs: initializing host on bus {}", bus_num);
            if let Err(err) = ax_task::future::block_on(guard.host_mut().init()) {
                warn!("usbfs: failed to initialize USB host on bus {bus_num}: {err:?}");
                failed_device_ids.push((device_id, irq_num));
                continue;
            }

            info!("usbfs: host on bus {} initialized", bus_num);
            initialized += 1;
        }

        if !failed_device_ids.is_empty() {
            let mut state = manager.state.lock();
            state.hosts.retain(|host| {
                !failed_device_ids
                    .iter()
                    .any(|(failed_device_id, _)| *failed_device_id == host.device_id)
            });
        }

        for (_, irq_num) in failed_device_ids {
            if let Some(irq_num) = irq_num {
                let _ = ax_hal::irq::unregister(irq_num);
            }
        }

        info!("usbfs: {} host(s) ready", initialized);
        initialized
    }
}

pub(super) fn discover_hosts() -> (Vec<UsbHostState>, Vec<PendingUsbIrqSlot>) {
    #[cfg(not(target_os = "none"))]
    {
        (Vec::new(), Vec::new())
    }
    #[cfg(target_os = "none")]
    {
        let hosts = rdrive::get_list::<axplat_dyn::drivers::usb::PlatformUsbHost>();
        let mut initialized_hosts = Vec::new();
        let mut irq_slots = Vec::new();

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
            let event_handler: EventHandler = guard.host_mut().create_event_handler();
            drop(guard);

            if let Some(irq_num) = irq_num {
                irq_slots.push(PendingUsbIrqSlot {
                    irq_num,
                    device_id,
                    bus_num,
                    handler: event_handler,
                });
            }

            initialized_hosts.push(UsbHostState {
                device_id,
                bus_num,
                irq_num,
                needs_probe: true,
                next_device_num: 1,
                stable_id_to_device_num: BTreeMap::new(),
            });
        }

        info!("usbfs: discovered {} USB host(s)", initialized_hosts.len());
        (initialized_hosts, irq_slots)
    }
}
