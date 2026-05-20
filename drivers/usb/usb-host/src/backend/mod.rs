use alloc::boxed::Box;
#[cfg(any(kmod, umod))]
use alloc::vec::Vec;
use core::any::Any;

use futures::future::{BoxFuture, LocalBoxFuture};
use usb_if::err::USBError;

#[cfg(any(kmod, umod))]
use crate::backend::ty::ProbedDeviceInfoOp;
use crate::backend::ty::{DeviceInfoOp, DeviceOp};

#[cfg(umod)]
pub mod umod;

#[cfg(kmod)]
pub mod kmod;

pub(crate) mod ty;

define_int_type!(Dci, u8);
define_int_type!(PortId, usize);
define_int_type!(DeviceId, u32);

impl Dci {
    pub const CTRL: Self = Self(1);

    pub fn as_u8(&self) -> u8 {
        self.0
    }

    pub fn as_usize(&self) -> usize {
        self.0 as usize
    }
}

pub(crate) trait BackendOp: Send + Any + 'static {
    /// 初始化后端
    fn init<'a>(&'a mut self) -> BoxFuture<'a, Result<(), USBError>>;

    #[cfg(any(kmod, umod))]
    /// 探测已连接的设备
    fn device_list<'a>(&'a mut self) -> BoxFuture<'a, Result<Vec<ProbedDeviceInfoOp>, USBError>>;

    fn open_device<'a>(
        &'a mut self,
        dev: &'a dyn DeviceInfoOp,
    ) -> LocalBoxFuture<'a, Result<Box<dyn DeviceOp>, USBError>>;

    #[cfg(kmod)]
    fn create_event_handler(&mut self) -> Box<dyn crate::backend::ty::EventHandlerOp>;
}
