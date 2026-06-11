#![no_std]
#![no_main]
#![feature(used_with_arg)]

extern crate alloc;
extern crate bare_test;

#[bare_test::tests]
mod tests {
    use alloc::string::String;
    use core::{
        ptr::NonNull,
        sync::atomic::{AtomicUsize, Ordering},
    };

    use bare_test::{
        hal::al::memory,
        os::{
            irq::register_handler,
            mem::mmio::{MapError, MmioOp, MmioRaw, kernel_mmio_op},
            platform::{PlatformDescriptor, get_platform_descriptor},
        },
    };
    use fdt_edit::{ClockType, Fdt, InterruptRef, NodeType};
    use ax_kspin::SpinNoIrq as Mutex;
    use rdif_intc::Intc;
    use rdif_serial::{BIrqHandler, BReceiver, BSender, BSerial, TransferError};
    use rdrive::fdt_phandle_to_device_id;
    use some_serial::{Config, DataBits, InterruptMask, Parity, StopBits};

    static TX_INTERRUPT_COUNT: AtomicUsize = AtomicUsize::new(0);
    static RX_INTERRUPT_COUNT: AtomicUsize = AtomicUsize::new(0);
    static IRQ_HANDLER_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);
    static IRQ_HANDLER: Mutex<Option<BIrqHandler>> = Mutex::new(None);

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum DriverType {
        Pl011,
        Ns16550Mmio,
    }

    struct TestSerial {
        serial: BSerial,
        mmio: TestMmio,
        irq: rdrive::IrqId,
        driver_type: DriverType,
    }

    enum TestMmio {
        Owned(MmioRaw),
        Borrowed(NonNull<u8>),
    }

    impl TestMmio {
        fn base(&self) -> NonNull<u8> {
            match self {
                Self::Owned(mmio) => mmio.as_nonnull_ptr(),
                Self::Borrowed(base) => *base,
            }
        }
    }

    impl Drop for TestSerial {
        fn drop(&mut self) {
            self.serial
                .disable_interrupts(InterruptMask::TX_EMPTY | InterruptMask::RX_AVAILABLE);
            self.serial.disable_loopback();
            *IRQ_HANDLER.lock() = None;
            if let TestMmio::Owned(mmio) = &self.mmio {
                kernel_mmio_op().iounmap(mmio);
            }
        }
    }

    #[test]
    fn test_serial_basic_loopback() {
        let mut ctx = create_test_serial();
        let serial = &mut ctx.serial;

        let config = Config::new()
            .baudrate(115200)
            .data_bits(DataBits::Eight)
            .stop_bits(StopBits::One)
            .parity(Parity::None);
        serial.set_config(&config).expect("failed to set config");

        let mut tx = serial.take_tx().expect("missing tx");
        let mut rx = serial.take_rx().expect("missing rx");
        clean_rx(&mut rx);

        test_serial_tx_rx_one(serial, &mut tx, &mut rx, b"Hello\n").expect("loopback failed");
    }

    #[test]
    fn test_serial_configuration_roundtrip() {
        let mut ctx = create_test_serial();
        let serial = &mut ctx.serial;

        let configs = [
            (115200, DataBits::Eight, StopBits::One, Parity::None),
            (9600, DataBits::Seven, StopBits::One, Parity::Even),
            (38400, DataBits::Eight, StopBits::Two, Parity::Odd),
        ];

        for (baudrate, data_bits, stop_bits, parity) in configs {
            let config = Config::new()
                .baudrate(baudrate)
                .data_bits(data_bits)
                .stop_bits(stop_bits)
                .parity(parity);

            serial.set_config(&config).expect("failed to set config");

            assert_eq!(serial.data_bits(), data_bits);
            assert_eq!(serial.stop_bits(), stop_bits);
            assert_eq!(serial.parity(), parity);
            assert_ne!(serial.baudrate(), 0, "baudrate should be readable");
        }
    }

    #[test]
    fn test_interrupt_mask_control() {
        let mut ctx = create_test_serial();
        let irq = ctx.irq;
        let driver_type = ctx.driver_type;
        let serial = &mut ctx.serial;

        reset_interrupt_counters();

        serial.enable_interrupts(InterruptMask::TX_EMPTY);
        assert_eq!(
            serial.get_enabled_interrupts().bits(),
            InterruptMask::TX_EMPTY.bits()
        );

        serial.enable_interrupts(InterruptMask::RX_AVAILABLE);
        assert_eq!(
            serial.get_enabled_interrupts().bits(),
            (InterruptMask::TX_EMPTY | InterruptMask::RX_AVAILABLE).bits()
        );

        serial.disable_interrupts(InterruptMask::TX_EMPTY);
        assert_eq!(
            serial.get_enabled_interrupts().bits(),
            InterruptMask::RX_AVAILABLE.bits()
        );

        let mut tx = serial.take_tx().expect("missing tx");
        let mut rx = serial.take_rx().expect("missing rx");

        clean_rx(&mut rx);
        serial.enable_loopback();

        let payload = b"irq-loopback";
        let mut remaining = payload.as_slice();
        while !remaining.is_empty() {
            let written = tx.write_bytes(remaining);
            assert!(written > 0, "failed to write test payload");
            remaining = &remaining[written..];
        }

        assert!(
            wait_for_counter(&RX_INTERRUPT_COUNT),
            "RX interrupt was not observed on irq {:?} for {:?}",
            irq,
            driver_type
        );
        assert!(
            IRQ_HANDLER_CALL_COUNT.load(Ordering::SeqCst) > 0,
            "IRQ handler was never invoked"
        );

        let mut buffer = [0u8; 32];
        let received = rx.read_bytes(&mut buffer).expect("failed to read loopback");
        assert_eq!(&buffer[..received], payload);

        serial.disable_loopback();
        serial.disable_interrupts(InterruptMask::TX_EMPTY | InterruptMask::RX_AVAILABLE);
        assert_eq!(
            serial.get_enabled_interrupts().bits(),
            InterruptMask::empty().bits()
        );
    }

    fn create_test_serial() -> TestSerial {
        let fdt = get_platform_fdt();
        let node = find_test_uart_node(&fdt);
        let driver_type = driver_type_for_node(&node);
        let reg = node.regs().into_iter().next().expect("uart reg missing");
        let size = reg.size.unwrap_or(0x1000).max(0x1000) as usize;
        let mmio = match kernel_mmio_op().ioremap((reg.address as usize).into(), size) {
            Ok(mmio) => TestMmio::Owned(mmio),
            Err(MapError::Busy) => {
                let virt = memory::_io((reg.address as usize).into());
                let base = NonNull::new(virt.raw() as *mut u8).expect("uart virt addr is null");
                TestMmio::Borrowed(base)
            }
            Err(err) => panic!("failed to map uart mmio: {err:?}"),
        };
        let base = mmio.base();
        let clock = clock_frequency(&fdt, &node, driver_type);
        let irq_ref = node
            .interrupts()
            .into_iter()
            .next()
            .expect("uart interrupt missing");

        let mut serial = match driver_type {
            DriverType::Pl011 => some_serial::pl011::Pl011::new_boxed(base, clock),
            DriverType::Ns16550Mmio => {
                some_serial::ns16550::Ns16550::new_mmio_boxed(base, clock, 4)
            }
        };

        let irq_handler = serial.irq_handler().expect("missing irq handler");
        let irq = register_uart_irq(&irq_ref, irq_handler);

        TestSerial {
            serial,
            mmio,
            irq,
            driver_type,
        }
    }

    fn get_platform_fdt() -> Fdt {
        let PlatformDescriptor::DeviceTree(dtb) = get_platform_descriptor() else {
            panic!("device tree not found");
        };

        Fdt::from_bytes(dtb.as_slice()).expect("invalid device tree")
    }

    fn find_test_uart_node<'a>(fdt: &'a Fdt) -> NodeType<'a> {
        if let Some(path) = chosen_stdout_path(fdt) {
            if let Some(node) = fdt.get_by_path(&path) {
                return node;
            }
        }

        fdt.find_compatible(&["arm,pl011", "snps,dw-apb-uart"])
            .into_iter()
            .next()
            .expect("no supported uart node found")
    }

    fn chosen_stdout_path(fdt: &Fdt) -> Option<String> {
        let chosen = fdt.get_by_path("/chosen")?;
        for key in ["stdout-path", "linux,stdout-path"] {
            if let Some(path) = chosen
                .as_node()
                .get_property(key)
                .and_then(|prop| prop.as_str())
            {
                let path = path.split(':').next().unwrap_or(path);
                if !path.is_empty() {
                    return Some(path.into());
                }
            }
        }
        None
    }

    fn driver_type_for_node(node: &NodeType<'_>) -> DriverType {
        for compatible in node.as_node().compatibles() {
            match compatible {
                "arm,pl011" | "arm,primecell" => return DriverType::Pl011,
                "snps,dw-apb-uart" => return DriverType::Ns16550Mmio,
                _ => {}
            }
        }
        panic!("unsupported uart compatible set")
    }

    fn clock_frequency(fdt: &Fdt, node: &NodeType<'_>, driver_type: DriverType) -> u32 {
        node.clocks()
            .into_iter()
            .find_map(|clock_ref| {
                let provider = fdt.get_by_phandle(clock_ref.phandle)?;
                match provider {
                    NodeType::Clock(clock) => match clock.clock_type() {
                        ClockType::Fixed(clock) if clock.frequency != 0 => Some(clock.frequency),
                        _ => None,
                    },
                    _ => None,
                }
            })
            .unwrap_or(match driver_type {
                DriverType::Pl011 => 24_000_000,
                DriverType::Ns16550Mmio => 1_843_200,
            })
    }

    fn register_uart_irq(interrupt: &InterruptRef, handler: BIrqHandler) -> rdrive::IrqId {
        let intc_id = fdt_phandle_to_device_id(interrupt.interrupt_parent)
            .expect("interrupt parent not registered");
        let intc = rdrive::get::<Intc>(intc_id).expect("failed to fetch interrupt controller");
        let irq = intc.lock().unwrap().setup_irq_by_fdt(&interrupt.specifier);

        *IRQ_HANDLER.lock() = Some(handler);

        register_handler(irq.raw().into(), || {
            IRQ_HANDLER_CALL_COUNT.fetch_add(1, Ordering::SeqCst);

            let status = {
                let guard = IRQ_HANDLER.lock();
                let Some(handler) = guard.as_ref() else {
                    return;
                };
                handler.clean_interrupt_status()
            };

            if status.contains(InterruptMask::TX_EMPTY) {
                TX_INTERRUPT_COUNT.fetch_add(1, Ordering::SeqCst);
            }
            if status.contains(InterruptMask::RX_AVAILABLE) {
                RX_INTERRUPT_COUNT.fetch_add(1, Ordering::SeqCst);
            }
        });

        irq
    }

    fn reset_interrupt_counters() {
        TX_INTERRUPT_COUNT.store(0, Ordering::SeqCst);
        RX_INTERRUPT_COUNT.store(0, Ordering::SeqCst);
        IRQ_HANDLER_CALL_COUNT.store(0, Ordering::SeqCst);
    }

    fn wait_for_counter(counter: &AtomicUsize) -> bool {
        for _ in 0..200_000 {
            if counter.load(Ordering::SeqCst) > 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    fn clean_rx(rx: &mut BReceiver) {
        let mut buffer = [0u8; 64];
        loop {
            match rx.read_bytes(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }

    fn test_serial_tx_rx_one(
        serial: &mut BSerial,
        tx: &mut BSender,
        rx: &mut BReceiver,
        expected: &[u8],
    ) -> Result<(), TransferError> {
        serial.enable_loopback();
        clean_rx(rx);

        let mut received = [0u8; 64];
        let mut total = 0usize;

        for &byte in expected {
            let mut written = 0usize;
            while written == 0 {
                written = tx.write_bytes(&[byte]);
                core::hint::spin_loop();
            }

            loop {
                match rx.read_bytes(&mut received[total..total + 1]) {
                    Ok(1) => {
                        total += 1;
                        break;
                    }
                    Ok(0) => core::hint::spin_loop(),
                    Ok(_) => unreachable!(),
                    Err(err) => {
                        serial.disable_loopback();
                        return Err(err.kind);
                    }
                }
            }
        }

        serial.disable_loopback();
        assert_eq!(&received[..total], expected);
        Ok(())
    }
}
