use alloc::{string::String, sync::Arc};
use core::cell::UnsafeCell;

use ax_kspin::SpinNoIrq;
use axklib::irq::{CpuId, IrqError, run_on_cpu_sync};
use rdif_serial::{
    Config, ConfigError, OwnerId, OwnerLease, RawUart, RxItem, RxQueue, SerialCounters,
    SerialIrqHandler, SerialIrqOutcome, SerialSoftWork, TSerialIrqHandler, TxQueue,
};

pub struct SerialPort {
    name: String,
    base_addr: usize,
    owner: OwnerId,
    tx: SpinNoIrq<TxQueue>,
    rx: SpinNoIrq<RxQueue>,
    irq: Arc<SerialIrqHandler>,
}

impl SerialPort {
    pub fn new(raw: impl RawUart) -> Self {
        Self::new_with_owner(raw, 0)
    }

    pub fn new_with_owner(raw: impl RawUart, owner_cpu: usize) -> Self {
        let name = raw.name().into();
        let base_addr = raw.base_addr();
        let owner = OwnerId(owner_cpu);
        let parts: rdif_serial::SerialParts = SerialIrqHandler::split(raw, owner);
        Self {
            name,
            base_addr,
            owner,
            tx: SpinNoIrq::new(parts.tx),
            rx: SpinNoIrq::new(parts.rx),
            irq: parts.irq,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn base_addr(&self) -> usize {
        self.base_addr
    }

    pub fn owner_cpu(&self) -> usize {
        self.owner.0
    }

    pub fn baudrate(&self) -> u32 {
        self.run_on_owner(|irq, lease| irq.baudrate(lease))
            .unwrap_or(0)
    }

    pub fn startup(&self, config: &Config) -> Result<SerialIrqOutcome, ConfigError> {
        self.run_on_owner(|irq, lease| irq.startup(lease, config))
            .map_err(|_| ConfigError::RegisterError)?
    }

    pub fn shutdown(&self) -> Result<(), IrqError> {
        self.run_on_owner(|irq, lease| irq.shutdown(lease))
    }

    pub fn set_config(&self, config: &Config) -> Result<(), ConfigError> {
        self.run_on_owner(|irq, lease| irq.set_config(lease, config))
            .map_err(|_| ConfigError::RegisterError)?
    }

    pub fn submit_tx(&self, bytes: &[u8]) -> (usize, SerialIrqOutcome) {
        let submit = self.tx.lock().submit(bytes);
        let outcome = if submit.needs_kick {
            self.service_on_owner(SerialSoftWork::TX_KICK)
        } else {
            SerialIrqOutcome::default()
        };
        (submit.accepted, outcome)
    }

    pub fn write_room(&self) -> usize {
        self.tx.lock().write_room()
    }

    pub fn chars_in_buffer(&self) -> usize {
        self.tx.lock().chars_in_buffer()
    }

    pub fn tx_idle(&self) -> bool {
        self.run_on_owner(|irq, lease| irq.tx_idle(lease))
            .unwrap_or(false)
    }

    pub fn drain_rx(&self, out: &mut [RxItem]) -> usize {
        self.rx.lock().drain(out)
    }

    pub fn rx_pending(&self) -> bool {
        self.rx.lock().rx_pending()
    }

    pub fn handle_irq_on_owner(&self, cpu: CpuId) -> SerialIrqOutcome {
        let Some(lease) = self.owner_lease_for_cpu(cpu) else {
            return SerialIrqOutcome::default();
        };
        self.irq.handle(lease)
    }

    pub fn service_on_owner(&self, work: SerialSoftWork) -> SerialIrqOutcome {
        self.run_on_owner(|irq, lease| irq.service(lease, work))
            .unwrap_or_default()
    }

    pub fn counters(&self) -> SerialCounters {
        self.irq.counters()
    }

    fn run_on_owner<F, R>(&self, op: F) -> Result<R, IrqError>
    where
        F: FnOnce(&SerialIrqHandler, OwnerLease<'_>) -> R,
    {
        struct OwnerCall<'a, F, R> {
            port: &'a SerialPort,
            op: UnsafeCell<Option<F>>,
            result: UnsafeCell<Option<R>>,
        }

        unsafe fn thunk<F, R>(arg: *mut ())
        where
            F: FnOnce(&SerialIrqHandler, OwnerLease<'_>) -> R,
        {
            let call = unsafe { &*(arg as *const OwnerCall<'_, F, R>) };
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
                thunk::<F, R>,
                (&call as *const OwnerCall<'_, F, R> as *mut ()).cast(),
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
