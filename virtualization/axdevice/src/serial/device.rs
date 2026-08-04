//! Unified bus adapters for virtual UART cores.

use alloc::{boxed::Box, sync::Arc};

use axdevice_base::{
    AccessWidth, BusAccess, BusKind, BusResponse, Device, DeviceAccess, DeviceError,
    InterruptTriggerMode, IrqLine, Resource,
};

use crate::{
    DeviceBundle, DeviceManagerResult, DeviceRegistration, PollableDeviceOps,
    serial::{Pl011, SerialBackend, Uart16550},
};

struct Uart16550PortDevice {
    core: Uart16550,
    base: u16,
    resources: Box<[Resource]>,
}

impl Uart16550PortDevice {
    fn new(
        base: u16,
        length: u16,
        irq_id: usize,
        backend: Arc<dyn SerialBackend>,
        irq: IrqLine,
    ) -> Self {
        Self {
            core: Uart16550::new(backend, irq),
            base,
            resources: alloc::vec![
                Resource::PortRange { base, size: length },
                irq_resource(irq_id),
            ]
            .into_boxed_slice(),
        }
    }
}

impl Device for Uart16550PortDevice {
    fn name(&self) -> &str {
        "uart16550-port"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Port {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let offset = u16::try_from(access.addr)
            .ok()
            .and_then(|port| port.checked_sub(self.base))
            .ok_or(DeviceError::OutOfRange { addr: access.addr })? as usize;
        if access.width != AccessWidth::Byte {
            return Err(DeviceError::InvalidWidth {
                expected: AccessWidth::Byte,
                actual: access.width,
            });
        }
        handle_16550(&self.core, offset, access)
    }
}

impl PollableDeviceOps for Uart16550PortDevice {
    fn poll(&self, _now_ns: u64) -> DeviceManagerResult {
        self.core.poll().map_err(Into::into)
    }
}

struct Uart16550MmioDevice {
    core: Uart16550,
    base: u64,
    register_shift: u8,
    resources: Box<[Resource]>,
}

impl Uart16550MmioDevice {
    fn new(
        base: usize,
        length: usize,
        register_shift: u8,
        irq_id: usize,
        backend: Arc<dyn SerialBackend>,
        irq: IrqLine,
    ) -> Self {
        Self {
            core: Uart16550::new(backend, irq),
            base: base as u64,
            register_shift,
            resources: alloc::vec![
                Resource::MmioRange {
                    base: base as u64,
                    size: length as u64,
                },
                irq_resource(irq_id),
            ]
            .into_boxed_slice(),
        }
    }
}

impl Device for Uart16550MmioDevice {
    fn name(&self) -> &str {
        "uart16550-mmio"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let offset = access
            .addr
            .checked_sub(self.base)
            .ok_or(DeviceError::OutOfRange { addr: access.addr })?;
        let register = usize::try_from(offset >> self.register_shift)
            .map_err(|_| DeviceError::OutOfRange { addr: access.addr })?;
        handle_16550(&self.core, register, access)
    }
}

impl PollableDeviceOps for Uart16550MmioDevice {
    fn poll(&self, _now_ns: u64) -> DeviceManagerResult {
        self.core.poll().map_err(Into::into)
    }
}

struct Pl011MmioDevice {
    core: Pl011,
    base: u64,
    resources: Box<[Resource]>,
}

impl Pl011MmioDevice {
    fn new(
        base: usize,
        length: usize,
        irq_id: usize,
        backend: Arc<dyn SerialBackend>,
        irq: IrqLine,
    ) -> Self {
        Self {
            core: Pl011::new(backend, irq),
            base: base as u64,
            resources: alloc::vec![
                Resource::MmioRange {
                    base: base as u64,
                    size: length as u64,
                },
                irq_resource(irq_id),
            ]
            .into_boxed_slice(),
        }
    }
}

impl Device for Pl011MmioDevice {
    fn name(&self) -> &str {
        "pl011"
    }

    fn resources(&self) -> &[Resource] {
        &self.resources
    }

    fn access(
        &self,
        access: &BusAccess,
        _context: &mut dyn DeviceAccess,
    ) -> Result<BusResponse, DeviceError> {
        if access.kind != BusKind::Mmio {
            return Err(DeviceError::OutOfRange { addr: access.addr });
        }
        let offset = access
            .addr
            .checked_sub(self.base)
            .ok_or(DeviceError::OutOfRange { addr: access.addr })?;
        let offset =
            usize::try_from(offset).map_err(|_| DeviceError::OutOfRange { addr: access.addr })?;
        if access.is_read {
            self.core
                .read(offset, access.width)
                .map(|value| BusResponse::Read { value })
        } else {
            self.core.write(offset, access.width, access.data)?;
            Ok(BusResponse::Write)
        }
    }
}

impl PollableDeviceOps for Pl011MmioDevice {
    fn poll(&self, _now_ns: u64) -> DeviceManagerResult {
        self.core.poll().map_err(Into::into)
    }
}

/// Builds a port-mapped 16550 UART bundle.
pub fn build_16550_port(
    base: u16,
    length: u16,
    irq_id: usize,
    backend: Arc<dyn SerialBackend>,
    irq: IrqLine,
) -> DeviceBundle {
    bundle(Arc::new(Uart16550PortDevice::new(
        base, length, irq_id, backend, irq,
    )))
}

/// Builds a memory-mapped 16550 UART bundle.
pub fn build_16550_mmio(
    base: usize,
    length: usize,
    register_shift: u8,
    irq_id: usize,
    backend: Arc<dyn SerialBackend>,
    irq: IrqLine,
) -> DeviceBundle {
    bundle(Arc::new(Uart16550MmioDevice::new(
        base,
        length,
        register_shift,
        irq_id,
        backend,
        irq,
    )))
}

/// Builds a memory-mapped PL011 UART bundle.
pub fn build_pl011_mmio(
    base: usize,
    length: usize,
    irq_id: usize,
    backend: Arc<dyn SerialBackend>,
    irq: IrqLine,
) -> DeviceBundle {
    bundle(Arc::new(Pl011MmioDevice::new(
        base, length, irq_id, backend, irq,
    )))
}

fn bundle<D>(device: Arc<D>) -> DeviceBundle
where
    D: Device + PollableDeviceOps + 'static,
{
    DeviceBundle::new()
        .with_registration(DeviceRegistration::Device(device.clone()))
        .with_registration(DeviceRegistration::Pollable(device))
}

fn handle_16550(
    core: &Uart16550,
    register: usize,
    access: &BusAccess,
) -> Result<BusResponse, DeviceError> {
    // QEMU's serial-mm adapter uses the access address only to select the
    // register and always transfers the low byte to the 16550 core.
    if access.is_read {
        core.read(register, AccessWidth::Byte)
            .map(|value| BusResponse::Read { value })
    } else {
        core.write(register, AccessWidth::Byte, access.data)?;
        Ok(BusResponse::Write)
    }
}

fn irq_resource(irq_id: usize) -> Resource {
    Resource::IrqLine {
        line: u32::try_from(irq_id).expect("machine-profile IRQ must fit u32"),
        trigger: InterruptTriggerMode::LevelTriggered,
    }
}

#[allow(dead_code)]
fn _assert_access_width_is_exhaustive(width: AccessWidth) {
    match width {
        AccessWidth::Byte | AccessWidth::Word | AccessWidth::Dword | AccessWidth::Qword => {}
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec::Vec};
    use std::sync::Mutex;

    use axdevice_base::{
        ControllerInputId, InterruptControllerId, IrqResult, WiredIrqInput, WiredIrqSink,
    };

    use super::*;

    #[derive(Debug, Default)]
    struct RecordingBackend {
        output: Mutex<Vec<u8>>,
    }

    impl SerialBackend for RecordingBackend {
        fn write(&self, bytes: &[u8]) {
            self.output.lock().unwrap().extend_from_slice(bytes);
        }

        fn read(&self, _buffer: &mut [u8]) -> usize {
            0
        }
    }

    struct NoopIrqSink;

    impl WiredIrqSink for NoopIrqSink {
        fn set_level(&self, _input: ControllerInputId, _asserted: bool) -> IrqResult {
            Ok(())
        }

        fn pulse(&self, _input: ControllerInputId) -> IrqResult {
            Ok(())
        }
    }

    fn level_irq(line: usize) -> IrqLine {
        WiredIrqInput::new(
            InterruptControllerId::new(0),
            ControllerInputId::new(line),
            InterruptTriggerMode::LevelTriggered,
            Arc::new(NoopIrqSink),
        )
        .connect()
        .unwrap()
    }

    #[test]
    fn mmio_16550_preserves_register_stride_and_bus_width_semantics() {
        const BASE: u64 = 0xfeb5_0000;

        let backend = Arc::new(RecordingBackend::default());
        let device = Uart16550MmioDevice::new(
            BASE as usize,
            0x100,
            2,
            365,
            backend.clone(),
            level_irq(365),
        );
        let mut context = axdevice_base::NoopDeviceAccess::new(axdevice_base::DeviceId::new(0));

        for width in [
            AccessWidth::Byte,
            AccessWidth::Word,
            AccessWidth::Dword,
            AccessWidth::Qword,
        ] {
            device
                .access(
                    &BusAccess {
                        kind: BusKind::Mmio,
                        is_read: false,
                        addr: BASE,
                        width,
                        data: 0xfeed_0000_0000_005a,
                    },
                    &mut context,
                )
                .unwrap();
        }
        assert_eq!(backend.output.lock().unwrap().as_slice(), b"ZZZZ");

        device
            .access(
                &BusAccess {
                    kind: BusKind::Mmio,
                    is_read: false,
                    addr: BASE + 4,
                    width: AccessWidth::Byte,
                    data: 1,
                },
                &mut context,
            )
            .unwrap();
        assert!(matches!(
            device
                .access(
                    &BusAccess {
                        kind: BusKind::Mmio,
                        is_read: true,
                        addr: BASE + 4,
                        width: AccessWidth::Byte,
                        data: 0,
                    },
                    &mut context,
                )
                .unwrap(),
            BusResponse::Read { value: 1 }
        ));

        assert!(matches!(
            device
                .access(
                    &BusAccess {
                        kind: BusKind::Mmio,
                        is_read: true,
                        addr: BASE + 0x88,
                        width: AccessWidth::Dword,
                        data: 0,
                    },
                    &mut context,
                )
                .unwrap(),
            BusResponse::Read { value: 0 }
        ));
    }
}
