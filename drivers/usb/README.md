# CrabUSB 🦀

A high-performance, asynchronous USB Host driver implementation written in Rust for embedded systems and operating system kernels.

## 🚀 Features

### Core Capabilities
- **🔄 Async/Await Support**: Built from the ground up with async primitives for non-blocking USB operations
- **⚡ Lock-Free Design**: Ring-based architecture using TRB (Transfer Request Block) for zero-lock async operations
- **🎯 xHCI Controller Support**: Complete implementation of the xHCI (Extensible Host Controller Interface) specification
- **📱 USB Standards Compliance**: Full support for USB 1.1, 2.0, and 3.x devices (Full, High, and SuperSpeed)
- **🔧 No-STD Compatible**: Designed for `#![no_std]` environments with minimal memory footprint
- **🖥️ User-Space libusb Backend**: Optional libusb backend for testing and development in user-space environments

### Transfer Types
- **Control Transfers**: Device setup, configuration, and standard requests
- **Bulk Transfers**: High-throughput data transfer for storage devices
- **Interrupt Transfers**: Periodic data transfer for HID devices
- **Isochronous Transfers**: Real-time streaming for audio/video devices

### Device Management
- **🔍 Device Enumeration**: Automatic discovery and enumeration of connected USB devices
- **📋 Descriptor Parsing**: Complete parsing of device, configuration, interface, and endpoint descriptors
- **🔌 Interface Management**: Easy interface claiming and endpoint access
- **🏷️ String Descriptors**: Full support for manufacturer, product, and serial number strings

### Architecture Highlights
- **Executor Agnostic**: Works with any async executor or can be used synchronously
- **DMA-Aware**: Efficient memory management with DMA coherency support
- **Event-Driven**: Interrupt-based event handling for optimal performance
- **Modular Design**: Clean separation between host controller and USB interface layers
- **Multi-Backend Support**: Supports both direct hardware access (xHCI) and user-space testing (libusb)

## 🏗️ Architecture

CrabUSB uses an innovative **lock-free design** based on TRB rings where each TRB represents an async task. The future queries the ring to obtain async results without requiring a specific executor, making it highly flexible and performant.

The driver supports multiple backends:
- **xHCI Backend**: Direct hardware access for embedded systems and OS kernels
- **libusb Backend**: User-space testing and development using libusb (enable with `libusb` feature)

```
┌─────────────────┐    ┌──────────────────┐    ┌─────────────────┐
│   Application   │◄──►│   USB Interface  │◄──►│    Backend      │
│                 │    │     (usb-if)     │    │   Selection     │
└─────────────────┘    └──────────────────┘    └─────────────────┘
                              │                          │
                              ▼                          ▼
                       ┌──────────────┐         ┌──────────────┐
                       │ Descriptors  │         │ xHCI / libusb│
                       │ & Transfers  │         │   Drivers    │
                       └──────────────┘         └──────────────┘
```
