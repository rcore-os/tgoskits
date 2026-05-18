//! xHCI 端口实现
//!
//! 实现 xHCI 控制器的端口操作，遵循 USB 2.0 规范 11.24。

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::time::Duration;
use futures::future::LocalBoxFuture;
use spin::RwLock;

use usb_if::host::hub::{DeviceSpeed, PortStatus, PortStatusChange};

use crate::backend::xhci::reg::XhciRegistersShared;
