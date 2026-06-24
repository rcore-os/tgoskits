use alloc::{string::String, sync::Arc};
use core::cell::UnsafeCell;

use ax_kspin::SpinNoIrq;
use axklib::irq::{CpuId, IrqError, run_on_cpu_sync};
use rdif_serial::{
    Config, ConfigError, OwnerId, OwnerLease, RawUart, RxItem, RxQueue, SerialCounters,
    SerialIrqHandler, SerialIrqOutcome, SerialSoftWork, TSerialIrqHandler, TxQueue,
};

pub type BInterruptSerial = Arc<dyn InterruptSerial>;

pub trait InterruptSerial: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn base_addr(&self) -> usize;
    fn owner_cpu(&self) -> usize;

    fn baudrate(&self) -> u32;
    fn startup(&self, config: &Config) -> Result<SerialIrqOutcome, ConfigError>;
    fn shutdown(&self) -> Result<(), IrqError>;
    fn set_config(&self, config: &Config) -> Result<(), ConfigError>;

    fn try_write(&self, bytes: &[u8]) -> usize;
    fn write_room(&self) -> usize;
    fn chars_in_buffer(&self) -> usize;
    fn tx_idle(&self) -> bool;

    fn drain_rx(&self, out: &mut [RxItem]) -> usize;
    fn rx_pending(&self) -> bool;

    fn handle_irq_on_owner(&self, cpu: CpuId) -> SerialIrqOutcome;
    fn service_on_owner(&self, work: SerialSoftWork) -> SerialIrqOutcome;

    fn counters(&self) -> SerialCounters;
}

pub struct KernelSerialPort<T: RawUart> {
    name: String,
    base_addr: usize,
    owner: OwnerId,
    tx: SpinNoIrq<TxQueue>,
    rx: SpinNoIrq<RxQueue>,
    irq: Arc<SerialIrqHandler<T>>,
}

impl<T: RawUart> KernelSerialPort<T> {
    pub fn new(raw: T) -> Self {
        Self::new_with_owner(raw, 0)
    }

    pub fn new_with_owner(raw: T, owner_cpu: usize) -> Self {
        let name = raw.name().into();
        let base_addr = raw.base_addr();
        let owner = OwnerId(owner_cpu);
        let parts = SerialIrqHandler::split(raw, owner);
        Self {
            name,
            base_addr,
            owner,
            tx: SpinNoIrq::new(parts.tx),
            rx: SpinNoIrq::new(parts.rx),
            irq: parts.irq,
        }
    }

    pub fn new_dyn(raw: T) -> BInterruptSerial {
        Arc::new(Self::new(raw))
    }

    fn run_on_owner<F, R>(&self, op: F) -> Result<R, IrqError>
    where
        F: FnOnce(&SerialIrqHandler<T>, OwnerLease<'_>) -> R,
    {
        struct OwnerCall<'a, T: RawUart, F, R> {
            port: &'a KernelSerialPort<T>,
            op: UnsafeCell<Option<F>>,
            result: UnsafeCell<Option<R>>,
        }

        unsafe fn thunk<T, F, R>(arg: *mut ())
        where
            T: RawUart,
            F: FnOnce(&SerialIrqHandler<T>, OwnerLease<'_>) -> R,
        {
            let call = unsafe { &*(arg as *const OwnerCall<'_, T, F, R>) };
            let op = unsafe { &mut *call.op.get() }
                .take()
                .expect("serial owner call entered twice");
            let lease = unsafe { OwnerLease::new_unchecked(call.port.owner) };
            let result = op(&call.port.irq, lease);
            unsafe { *call.result.get() = Some(result) };
        }

        let call = OwnerCall {
            port: self,
            op: UnsafeCell::new(Some(op)),
            result: UnsafeCell::new(None),
        };
        unsafe {
            run_on_cpu_sync(
                CpuId(self.owner.0),
                thunk::<T, F, R>,
                (&call as *const OwnerCall<'_, T, F, R> as *mut ()).cast(),
            )?;
        }
        Ok(unsafe { &mut *call.result.get() }
            .take()
            .expect("serial owner call did not complete"))
    }

    fn owner_lease_for_cpu(&self, cpu: CpuId) -> Option<OwnerLease<'static>> {
        (cpu.0 == self.owner.0).then(|| unsafe { OwnerLease::new_unchecked(self.owner) })
    }
}

impl<T: RawUart> InterruptSerial for KernelSerialPort<T> {
    fn name(&self) -> &str {
        &self.name
    }

    fn base_addr(&self) -> usize {
        self.base_addr
    }

    fn owner_cpu(&self) -> usize {
        self.owner.0
    }

    fn baudrate(&self) -> u32 {
        self.run_on_owner(|irq, lease| irq.baudrate(lease))
            .unwrap_or(0)
    }

    fn startup(&self, config: &Config) -> Result<SerialIrqOutcome, ConfigError> {
        self.run_on_owner(|irq, lease| irq.startup(lease, config))
            .map_err(|_| ConfigError::RegisterError)?
    }

    fn shutdown(&self) -> Result<(), IrqError> {
        self.run_on_owner(|irq, lease| irq.shutdown(lease))
    }

    fn set_config(&self, config: &Config) -> Result<(), ConfigError> {
        self.run_on_owner(|irq, lease| irq.set_config(lease, config))
            .map_err(|_| ConfigError::RegisterError)?
    }

    fn try_write(&self, bytes: &[u8]) -> usize {
        let submit = self.tx.lock().submit(bytes);
        if submit.needs_kick {
            let _ = self.service_on_owner(SerialSoftWork::TX_KICK);
        }
        submit.accepted
    }

    fn write_room(&self) -> usize {
        self.tx.lock().write_room()
    }

    fn chars_in_buffer(&self) -> usize {
        self.tx.lock().chars_in_buffer()
    }

    fn tx_idle(&self) -> bool {
        self.run_on_owner(|irq, lease| irq.tx_idle(lease))
            .unwrap_or(false)
    }

    fn drain_rx(&self, out: &mut [RxItem]) -> usize {
        self.rx.lock().drain(out)
    }

    fn rx_pending(&self) -> bool {
        self.rx.lock().rx_pending()
    }

    fn handle_irq_on_owner(&self, cpu: CpuId) -> SerialIrqOutcome {
        let Some(lease) = self.owner_lease_for_cpu(cpu) else {
            return SerialIrqOutcome::default();
        };
        self.irq.handle(lease)
    }

    fn service_on_owner(&self, work: SerialSoftWork) -> SerialIrqOutcome {
        self.run_on_owner(|irq, lease| irq.service(lease, work))
            .unwrap_or_default()
    }

    fn counters(&self) -> SerialCounters {
        self.irq.counters()
    }
}
