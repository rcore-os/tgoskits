use alloc::{string::String, sync::Arc};

use ax_kspin::SpinNoIrq;
use rdif_serial::{
    Config, ConfigError, RawUart, RxItem, SerialCore, SerialCounters, SerialIrqOutcome,
};

pub type BInterruptSerial = Arc<dyn InterruptSerial>;

pub trait InterruptSerial: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn base_addr(&self) -> usize;
    fn baudrate(&self) -> u32;

    fn startup(&self, config: &Config) -> Result<(), ConfigError>;
    fn shutdown(&self);
    fn set_config(&self, config: &Config) -> Result<(), ConfigError>;

    fn try_write(&self, bytes: &[u8]) -> usize;
    fn write_room(&self) -> usize;
    fn chars_in_buffer(&self) -> usize;
    fn flush_tx_buffer(&self);
    fn tx_idle(&self) -> bool;

    fn drain_rx(&self, out: &mut [RxItem]) -> usize;
    fn rx_pending(&self) -> bool;

    fn handle_irq(&self) -> SerialIrqOutcome;
    fn startup_catch_up(&self) -> SerialIrqOutcome;

    fn counters(&self) -> SerialCounters;
}

pub struct KernelSerialPort<T: RawUart> {
    name: String,
    base_addr: usize,
    inner: SpinNoIrq<SerialCore<T>>,
}

impl<T: RawUart> KernelSerialPort<T> {
    pub fn new(raw: T) -> Self {
        let name = raw.name().into();
        let base_addr = raw.base_addr();
        Self {
            name,
            base_addr,
            inner: SpinNoIrq::new(SerialCore::new(raw)),
        }
    }

    pub fn new_dyn(raw: T) -> BInterruptSerial {
        Arc::new(Self::new(raw))
    }
}

impl<T: RawUart> InterruptSerial for KernelSerialPort<T> {
    fn name(&self) -> &str {
        &self.name
    }

    fn base_addr(&self) -> usize {
        self.base_addr
    }

    fn baudrate(&self) -> u32 {
        self.inner.lock().baudrate()
    }

    fn startup(&self, config: &Config) -> Result<(), ConfigError> {
        self.inner.lock().startup(config)
    }

    fn shutdown(&self) {
        self.inner.lock().shutdown();
    }

    fn set_config(&self, config: &Config) -> Result<(), ConfigError> {
        self.inner.lock().set_config(config)
    }

    fn try_write(&self, bytes: &[u8]) -> usize {
        self.inner.lock().enqueue_tx(bytes).accepted
    }

    fn write_room(&self) -> usize {
        self.inner.lock().write_room()
    }

    fn chars_in_buffer(&self) -> usize {
        self.inner.lock().chars_in_buffer()
    }

    fn flush_tx_buffer(&self) {
        self.inner.lock().flush_tx_buffer();
    }

    fn tx_idle(&self) -> bool {
        self.inner.lock().tx_idle()
    }

    fn drain_rx(&self, out: &mut [RxItem]) -> usize {
        self.inner.lock().drain_rx(out)
    }

    fn rx_pending(&self) -> bool {
        self.inner.lock().rx_pending()
    }

    fn handle_irq(&self) -> SerialIrqOutcome {
        self.inner.lock().handle_irq()
    }

    fn startup_catch_up(&self) -> SerialIrqOutcome {
        self.inner.lock().startup_catch_up()
    }

    fn counters(&self) -> SerialCounters {
        self.inner.lock().counters()
    }
}
