use core::{
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

use axklib::{
    BoxedIrqHandler, ConcurrentBoxedIrqHandler, IrqCpuMask, IrqHandle, IrqId, Klib, KlibError,
    KlibResult, PhysAddr, VirtAddr, impl_trait,
};
use fdt_edit::{Fdt, Node, Property};
use rdrive::{Platform, register::DriverRegister};

const TEST_UART_PADDR: usize = 0x1000;
const TEST_UART_MMIO_SIZE: usize = 0x100;
const UART_LCR_INDEX: usize = 0x0c / 4;
const UART_DLF_INDEX: usize = 0xc0 / 4;

static TEST_UART_MMIO: AtomicUsize = AtomicUsize::new(0);

unsafe extern "Rust" {
    #[link_name = "__DRIVER_NS16550_SERIAL"]
    static NS16550_DRIVER: DriverRegister;
}

struct KlibImpl;

impl_trait! {
    impl Klib for KlibImpl {
        fn mem_iomap(addr: PhysAddr, size: usize) -> KlibResult<VirtAddr> {
            assert_eq!(addr.as_usize(), TEST_UART_PADDR);
            assert_eq!(size, TEST_UART_MMIO_SIZE);
            let ptr = TEST_UART_MMIO.load(Ordering::SeqCst);
            assert_ne!(ptr, 0, "test UART MMIO must be initialized before probing");
            Ok(VirtAddr::from_usize(ptr))
        }

        fn mem_virt_to_phys(addr: VirtAddr) -> PhysAddr {
            PhysAddr::from_usize(addr.as_usize())
        }

        fn mem_map_dma_coherent_uncached(
            _addr: core::ptr::NonNull<u8>,
            _size: usize,
        ) -> axklib::DmaCoherentMappingOutcome {
            axklib::DmaCoherentMappingOutcome::NotStarted(KlibError::Unsupported)
        }

        fn mem_unmap_dma_coherent(_addr: core::ptr::NonNull<u8>, _size: usize) -> KlibResult {
            Err(KlibError::Unsupported)
        }

        fn dma_cache_clean(_addr: VirtAddr, _size: usize) {}

        fn dma_cache_invalidate(_addr: VirtAddr, _size: usize) {}

        fn dma_cache_clean_invalidate(_addr: VirtAddr, _size: usize) {}

        fn dma_alloc_pages(
            _dma_mask: u64,
            _num_pages: usize,
            _align: usize,
        ) -> KlibResult<core::ptr::NonNull<u8>> {
            Err(KlibError::Unsupported)
        }

        fn dma_dealloc_pages(_addr: core::ptr::NonNull<u8>, _num_pages: usize) {}

        fn time_busy_wait(_dur: Duration) {}

        fn time_monotonic_nanos() -> u64 {
            0
        }

        fn time_try_init_epoch_offset(_epoch_time_nanos: u64) -> bool {
            false
        }

        fn irq_set_enable(_irq: IrqId, _enabled: bool) -> KlibResult {
            Ok(())
        }

        fn irq_request_shared(
            _irq: IrqId,
            _handler: BoxedIrqHandler,
        ) -> KlibResult<IrqHandle> {
            Err(KlibError::Unsupported)
        }

        fn irq_request_shared_disabled(
            _irq: IrqId,
            _handler: BoxedIrqHandler,
        ) -> KlibResult<IrqHandle> {
            Err(KlibError::Unsupported)
        }

        fn irq_request_percpu(
            _irq: IrqId,
            _cpus: IrqCpuMask,
            _handler: ConcurrentBoxedIrqHandler,
        ) -> KlibResult<IrqHandle> {
            Err(KlibError::Unsupported)
        }

        fn irq_free(_handle: IrqHandle) -> KlibResult {
            Err(KlibError::Unsupported)
        }

        fn irq_enable(_handle: IrqHandle) -> KlibResult {
            Err(KlibError::Unsupported)
        }

        fn irq_disable(_handle: IrqHandle) -> KlibResult {
            Err(KlibError::Unsupported)
        }
    }
}

#[test]
fn rk3588_dw_apb_uart_without_clock_frequency_uses_24_mhz() {
    let regs = Box::leak(Box::new([0u32; TEST_UART_MMIO_SIZE / 4]));
    regs[0] = 13;
    regs[UART_LCR_INDEX] = 0x03;
    regs[UART_DLF_INDEX] = 0;
    TEST_UART_MMIO.store(regs.as_mut_ptr() as usize, Ordering::SeqCst);

    let fdt_data = Box::leak(Box::new(rk3588_uart_fdt().encode()));
    let fdt_addr = NonNull::new(fdt_data.as_ref().as_ptr() as *mut u8).unwrap();
    rdrive::init(Platform::Fdt { addr: fdt_addr }).unwrap();
    rdrive::register_add(unsafe { NS16550_DRIVER.clone() });
    rdrive::probe_all(true).unwrap();

    let devices = ax_driver::serial::take_serial_devices();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].info.initial_baudrate, 115_384);
}

fn rk3588_uart_fdt() -> Fdt {
    let mut fdt = Fdt::new();
    let root = fdt.root_id();
    fdt.node_mut(root)
        .unwrap()
        .set_property(prop_u32s("#address-cells", &[1]));
    fdt.node_mut(root)
        .unwrap()
        .set_property(prop_u32s("#size-cells", &[1]));

    let uart = fdt.add_node(root, Node::new("serial@1000"));
    fdt.node_mut(uart).unwrap().set_property(prop_strs(
        "compatible",
        &["rockchip,rk3588-uart", "snps,dw-apb-uart"],
    ));
    fdt.node_mut(uart).unwrap().set_property(prop_u32s(
        "reg",
        &[TEST_UART_PADDR as u32, TEST_UART_MMIO_SIZE as u32],
    ));

    fdt
}

fn prop_u32s(name: &str, values: &[u32]) -> Property {
    let mut data = Vec::new();
    for value in values {
        data.extend_from_slice(&value.to_be_bytes());
    }
    Property::new(name, data)
}

fn prop_strs(name: &str, values: &[&str]) -> Property {
    let mut data = Vec::new();
    for value in values {
        data.extend_from_slice(value.as_bytes());
        data.push(0);
    }
    Property::new(name, data)
}
