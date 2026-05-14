#![cfg(not(target_os = "none"))]

use crab_usb::{USBHost, device::DeviceInfo, usb_if::descriptor::Class};
use log::info;

#[tokio::test]
async fn test() {
    env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .is_test(true)
        .init();

    let mut host = USBHost::new_libusb().unwrap();

    let ls = host.probe_devices().await.unwrap();

    let mut info: Option<DeviceInfo> = None;

    'devices: for probed in ls {
        println!("{probed:?}");
        let Some(device) = probed.into_device_info() else {
            continue;
        };

        for iface in device.interface_descriptors().cloned().collect::<Vec<_>>() {
            println!("  Interface: {:?}", iface.class());

            // if device.vendor_id() == 0x1a86 && device.product_id() == 0x7523 {
            //     info = Some(device);
            //     break;
            // }

            if matches!(iface.class(), Class::Video | Class::AudioVideo(_)) {
                info!("Found video interface: {iface:?}");
                info = Some(device);
                break 'devices;
            }
        }
    }
    let info = info.unwrap();

    let mut device = host.open_device(&info).await.unwrap();
    info!("Opened device: {}", device.descriptor().product_id);

    if let Some(s) = device.manufacturer() {
        info!("Manufacturer: {s}");
    }

    let config = device.current_configuration_descriptor().await.unwrap();

    for iface in &config.interfaces {
        let iface = iface.first_alt_setting();

        info!("Interface: {iface:?}");
        device
            .claim_interface(iface.interface_number, 0)
            .await
            .unwrap();

        info!("  Claimed interface {}", iface.interface_number);

        for ep in &iface.endpoints {
            info!("  Endpoint: {ep:?}");
            // if matches!(ep.direction, Direction::In)
            //     && matches!(ep.transfer_type, EndpointType::Isochronous)
            // {
            //     let mut endpoint = interface.endpoint_iso_in(ep.address).unwrap();
            //     let mut buf = vec![0u8; ep.max_packet_size as usize];
            //     let transfer = endpoint.submit(&mut buf, 1).unwrap();
            //     let n = transfer.await.unwrap();
            //     info!("    Read {n} bytes: {:x?}", &buf[..n]);
            // }

            // if matches!(ep.direction, Direction::In)
            //     && matches!(ep.transfer_type, EndpointType::Bulk)
            // {
            //     let mut endpoint = interface.endpoint_bulk_in(ep.address).unwrap();
            //     let mut buf = vec![0u8; ep.max_packet_size as usize];
            //     let transfer = endpoint.submit(&mut buf).unwrap();
            //     let n = transfer.await.unwrap();
            //     info!("    Wrote {n} bytes: {:x?}", &buf[..n]);
            // }
        }
    }

    drop(host);
}
