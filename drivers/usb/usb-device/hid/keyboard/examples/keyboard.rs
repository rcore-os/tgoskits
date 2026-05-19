use crab_usb::{DeviceInfo, USBHost};
use log::info;
use usb_keyboard::KeyBoard;

#[tokio::main]
async fn main() {
    env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .init();

    let mut host = USBHost::new_libusb().unwrap();
    let ls = host.probe_devices().await.unwrap();

    let mut info: Option<DeviceInfo> = None;

    'devices: for probed in ls {
        println!("{probed}");
        let Some(device) = probed.into_device_info() else {
            continue;
        };

        for iface in device.interface_descriptors().cloned().collect::<Vec<_>>() {
            println!("  Interface: {:?}", iface.class());

            if KeyBoard::check(&device) {
                info!("Found video interface: {iface:?}");
                info = Some(device);
                break 'devices;
            }
        }
    }

    let info = info.expect("No device found with HID keyboard interface");

    let device = host.open_device(&info).await.unwrap();
    info!("Opened device: {device}");

    let mut keyboard = KeyBoard::new(device).await.unwrap();

    loop {
        match keyboard.recv_events().await {
            Ok(report) => {
                info!("Received report: {:?}", report);
                // Process the report as needed
            }
            Err(_e) => {
                // info!("Error receiving report: {:?}", e);
            }
        }
    }
}
