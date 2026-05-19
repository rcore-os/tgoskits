# CrabUSB

A USB Host driver implementation written in Rust for embedded systems.

## Overview

CrabUSB is a `no_std` USB Host driver library that provides async support for USB device communication. It currently supports xHCI (Extensible Host Controller Interface) and is designed for embedded systems and operating system kernels.

## Features

- **Async/Await Support**: Built with async primitives for non-blocking USB operations
- **xHCI Controller Support**: Full implementation of xHCI specification
- **USB Device Management**: Device enumeration, configuration, and interface claiming
- **Standard USB Descriptors**: Complete parsing of device, configuration, interface, and endpoint descriptors
- **Multiple Transfer Types**: Support for Control, Bulk, Interrupt, and Isochronous transfers
- **Embedded-Friendly**: `no_std` compatible with minimal memory footprint
- **Flexible Integration**: Works with any async executor or can be used synchronously

## Architecture

The driver uses a lock-free design based on TRB (Transfer Request Block) rings, where each TRB represents an async task. The future queries the ring to get async results without requiring a specific executor.

## Usage

### Basic Setup

1. setup [dma-api](https://docs.rs/dma-api/latest/dma_api/)

2. implement the `Kernel` trait for your system

    ```rust
    use crab_usb::*;

    // Implement the Kernel trait for your system
    struct KernelImpl;
    impl_trait! {
        impl Kernel for KernelImpl {
            fn sleep<'a>(duration: Duration) -> BoxFuture<'a, ()> {
                your_os::sleep(duration).boxed()
            }

            fn page_size() -> usize {
                your_os::page_size()
            }
        }
    }

    // Initialize USB host controller
    let mut host = USBHost::new_xhci(mmio_base);
    let handle = host.event_handler();

    // Handle USB events in your OS irq callback
    your_os::register_irq_handler(usb_irq, move || {
         handle.handle_event();
    });

    host.init().await?;
    ```

### Device Communication

```rust
// Probe for connected devices
let devices = host.device_list().await.unwrap();

for mut device in devices {
    println!("Device: {:?}", device);
    
    // Claim an interface
    let mut interface = device.claim_interface(0, 0).await?;
    
    // Get endpoint for bulk transfers
    let mut bulk_in = interface.endpoint_bulk_in(0x81)?;
    
    // Perform data transfer
    let mut data = vec![0u8; 64];
    bulk_in.submit(&mut data)?.await?;

    // submit batch
    let mut datas = vec![vec![0u8; 64]; 10];
    let mut results = Vec::new();
    for data in datas.iter_mut() {
        let res = bulk_in.submit(data)?;
        results.push(res);  
    }
    // Wait for all transfers to complete
    for res in results {
        res.await?;
    }

}
```

## Testing

### QEMU Testing

```bash
cargo install ostool
cargo test -p crab-usb --test test --target aarch64-unknown-none-softfloat 
```

### Real Hardware (U-Boot)

```bash
cargo test -p crab-usb --test test --target aarch64-unknown-none-softfloat --  uboot
```

### Qemu using host USB devices on a Linux host

```bash
lsusb
```

for example, to find the device of a webcam:

```bash
Bus 003 Device 038: ID 1b17:0211 Sonix Technology Co., Ltd. GENERAL WEBCAM
```

add the following line to `bare-test.toml`:

```toml
args = "-usb -device qemu-xhci,id=xhci -device usb-host,bus=xhci.0,vendorid=0x1b17,productid=0x0211"
```

Then run the test

if no device is found, you may do not have the permission to access the USB device, you can run the following command to add permission:

```bash
sudo chmod 666 /dev/bus/usb/003/038
```

or you can add rules to `/etc/udev/rules.d/99-usb.rules`:

```text
SUBSYSTEM=="usb", ATTR{idVendor}=="1b17", ATTR{idProduct}=="0211", GROUP="plugdev", MODE="660"
```

then reload udev rules:

```bash
sudo usermod -aG plugdev $USER

sudo udevadm control --reload-rules
sudo udevadm trigger
```

check if the device is accessible:

```bash
ls -l /dev/bus/usb/003/038
```

## Supported USB Features

- **USB 1.1/2.0/3.x**: Full speed, High speed, and SuperSpeed devices
- **Device Classes**: HID, Mass Storage, Video (UVC), Audio, etc.
- **Transfer Types**:
  - Control transfers for device setup and configuration
  - Bulk transfers for large data transfers
  - Interrupt transfers for periodic data
  - Isochronous transfers for real-time data (audio/video)

## Platform Requirements

- **Architecture**: Currently tested on AArch64
- **Memory**: DMA-capable memory regions
- **Interrupts**: Interrupt handling capability for xHCI events
- **Timer**: For timeout and delay operations

## Contributing

Contributions are welcome! Please ensure that:

1. Code follows Rust conventions and passes `cargo clippy`
2. All tests pass on both QEMU and real hardware
3. Documentation is updated for new features
4. Commit messages are descriptive

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.

## References

- [USB 3.2 Specification](https://www.usb.org/document-library/usb-32-specification-released-september-22-2017-and-ecns)
- [xHCI Specification](https://www.intel.com/content/dam/www/public/us/en/documents/technical-specifications/extensible-host-controler-interface-usb-xhci.pdf)
- [USB Descriptors and Requests](https://www.beyondlogic.org/usbnutshell/usb5.shtml)
- [Qemu USB Emulation](https://qemu-project.gitlab.io/qemu/system/devices/usb.html)
