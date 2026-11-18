use ax_plat::irq::{
    self as host_irq, AutoEnable, HwIrq, IrqError, IrqId, IrqRequest, IrqReturn, ShareMode,
    riscv64_hv as hv,
};
use somehal::irq::{self, IrqAffinity, IrqDomainKind, rdif_intc::IrqTranslation};
use virtio_drivers::{
    device::rng::VirtIORng,
    transport::{DeviceType, InterruptStatus, Transport, mmio::MmioTransport},
};

pub fn run() {
    // QEMU virt leaves these sources unwired. Using the real controller
    // keeps the host configuration and guest activation paths in this test.
    let domain = irq::domain_by_kind_fast(IrqDomainKind::RiscvPlic).unwrap();
    let host = IrqId::new(domain, HwIrq(45));
    let guest = IrqId::new(domain, HwIrq(44));
    let controller = irq::intc_by_domain(domain).unwrap();
    let request = || {
        IrqRequest::new(|_| IrqReturn::Handled)
            .auto_enable(AutoEnable::No)
            .share_mode(ShareMode::Shared)
    };
    {
        let _controller = controller.lock().unwrap();
        assert_eq!(host_irq::request_irq(guest, request()), Err(IrqError::Busy));
    }
    irq::irq_set_enable(guest, false).unwrap();
    controller
        .lock()
        .unwrap()
        .configure(&IrqTranslation::new(host))
        .unwrap();
    let batch = [(guest.hwirq.0, None), (host.hwirq.0, None)];
    assert_eq!(
        hv::activate_guest_external_sources(&batch, 0),
        Err(IrqError::Busy),
        "a configured host IRQ was assigned to the guest"
    );
    let same_source = (guest.hwirq.0, None);
    assert_eq!(
        hv::activate_guest_external_sources(&[same_source, same_source], 0),
        Err(IrqError::InvalidIrq),
        "a batch must not reprogram a source it just enabled"
    );

    let dormant = IrqId::new(domain, HwIrq(46));
    let dormant_route = [(dormant.hwirq.0, None)];
    let handle = host_irq::request_irq(dormant, request()).unwrap();
    let second = host_irq::request_irq(dormant, request()).unwrap();
    assert_eq!(
        host_irq::request_irq(dormant, request().share_mode(ShareMode::Exclusive)),
        Err(IrqError::Busy)
    );
    assert_eq!(
        hv::activate_guest_external_sources(&dormant_route, 0),
        Err(IrqError::Busy),
        "guest took over a registered host IRQ before it was enabled"
    );
    let host_masked = IrqId::new(domain, HwIrq(47));
    let active = host_irq::request_irq(host_masked, request()).unwrap();
    host_irq::enable_irq(active).unwrap();
    host_irq::disable_irq(active).unwrap();
    assert_eq!(
        hv::activate_guest_external_sources(
            &[(guest.hwirq.0, None), (host_masked.hwirq.0, None)],
            0
        ),
        Err(IrqError::Busy),
        "a masked host IRQ was assigned to the guest"
    );
    hv::activate_guest_external_sources(&[(guest.hwirq.0, None)], 0)
        .expect("rejected batches must leave their free source available");
    assert_eq!(host_irq::request_irq(guest, request()), Err(IrqError::Busy));
    assert_eq!(irq::irq_set_enable(guest, false), Err(IrqError::Busy));
    assert_eq!(
        irq::irq_set_affinity(guest, IrqAffinity::Fixed { cpu_id: 1 }),
        Err(IrqError::Busy)
    );
    assert_eq!(
        controller
            .lock()
            .unwrap()
            .configure(&IrqTranslation::new(guest)),
        Err(IrqError::Busy)
    );
    hv::deactivate_guest_external_source(guest.hwirq.0).unwrap();
    host_irq::free_irq(handle).unwrap();
    assert_eq!(
        hv::activate_guest_external_sources(&dormant_route, 0),
        Err(IrqError::Busy),
        "the remaining shared host action still owns its IRQ"
    );
    host_irq::free_irq(second).unwrap();
    hv::activate_guest_external_sources(&dormant_route, 0)
        .expect("the final action must release an otherwise unclaimed IRQ");
    hv::deactivate_guest_external_source(dormant.hwirq.0).unwrap();
    host_irq::free_irq(active).unwrap();
    assert_eq!(
        hv::activate_guest_external_sources(&[(host_masked.hwirq.0, None)], 0),
        Err(IrqError::Busy),
        "a previously enabled host source cannot be reassigned until reset"
    );
    irq::irq_set_enable(host, true).unwrap();
    irq::irq_set_enable(host, false).unwrap();
    verify_retirement(&controller);
}

fn verify_retirement(controller: &rdrive::Device<irq::rdif_intc::Intc>) {
    let (address, banks, source, transport) = rdrive::with_fdt(|tree| {
        let plics = tree.find_compatible(&["riscv,plic0", "sifive,plic-1.0.0"]);
        let plic = plics.first()?;
        let address = plic.regs()[0].address as usize;
        let sources = plic.as_node().get_property("riscv,ndev")?.get_u32()? as usize + 1;
        tree.find_compatible(&["virtio,mmio"])
            .into_iter()
            .find_map(|node| {
                let reg = node.regs().into_iter().next()?;
                let size = reg.size? as usize;
                let base = ax_mm::iomap((reg.address as usize).into(), size).ok()?;
                let header = core::ptr::NonNull::new(base.as_mut_ptr())?;
                // SAFETY: The QEMU DTB provides the mapped VirtIO transport window.
                let transport = unsafe { MmioTransport::new(header.cast(), size) }.ok()?;
                if transport.device_type() != DeviceType::EntropySource {
                    return None;
                }
                let source = node.as_node().get_property("interrupts")?.get_u32()?;
                Some((address, sources.div_ceil(32), source, transport))
            })
    })
    .unwrap()
    .unwrap();
    let mut rng = VirtIORng::<ax_driver::virtio::VirtIoHalImpl, _>::new(transport).unwrap();
    let base = ax_mm::iomap(address.into(), 0x210000).unwrap().as_usize();
    rng.enable_interrupts();
    let _irq = ax_sync::IrqSaveGuard::new();
    let cpu = ax_hal::percpu::this_cpu_id();
    hv::activate_guest_external_sources(&[(source, None)], cpu).unwrap();

    let word = source as usize / 32 * 4;
    let bit = 1 << (source % 32);
    let pending = (base + 0x1000 + word) as *const u32;
    let wait_pending = || {
        let began = std::time::Instant::now();
        // SAFETY: The mapped PLIC pending register is read-only.
        while unsafe { pending.read_volatile() } & bit == 0 {
            assert!(
                began.elapsed() < std::time::Duration::from_secs(3),
                "device did not raise a PLIC interrupt"
            );
            core::hint::spin_loop();
        }
    };
    assert_eq!(rng.request_entropy(&mut [0]).unwrap(), 1);
    wait_pending();
    let mut active = irq::begin_irq((1usize << 63) | 9).unwrap();
    assert_eq!(active.id().hwirq.0, source);
    let outstanding = active.defer_riscv_external_completion().unwrap();
    assert!(
        rng.ack_interrupt()
            .contains(InterruptStatus::QUEUE_INTERRUPT)
    );

    let device = controller.lock().unwrap();
    let enable_reg = |context: usize| (base + 0x2000 + context * 0x80) as *mut u32;
    // SAFETY: The fixture is the only user of this RNG source and holds the
    // controller device guard. QEMU virt has four M/S contexts for two harts.
    let context = (0..4)
        .find(|&index| unsafe { enable_reg(index).byte_add(word).read_volatile() & bit != 0 })
        .unwrap();
    drop(device);
    let enable = enable_reg(context);
    let claim = (base + 0x200004 + context * 0x1000) as *mut u32;
    let isolate = || {
        let device = controller.lock().unwrap();
        let mut saved = [0; 32];
        // SAFETY: The controller guard excludes updates. Local IRQ exclusion
        // prevents other claims and this source is disabled for other contexts.
        unsafe {
            for (bank, value) in saved[..banks].iter_mut().enumerate() {
                *value = enable.add(bank).read_volatile();
                enable
                    .add(bank)
                    .write_volatile(if bank == word / 4 { bit } else { 0 });
            }
        }
        (device, saved)
    };
    let restore = |saved: [u32; 32]| {
        // SAFETY: The fixture holds the matching device guard for this snapshot.
        unsafe {
            core::arch::asm!("fence o, o", options(nostack));
            for (bank, &value) in saved[..banks].iter().enumerate() {
                enable.add(bank).write_volatile(value);
            }
            core::arch::asm!("fence o, rw", options(nostack));
        }
    };
    let read_claim = || {
        // SAFETY: The fixture isolates the RNG interrupt on this mapped context.
        unsafe {
            core::arch::asm!("fence iorw, iorw", options(nostack));
            claim.read_volatile()
        }
    };
    assert_eq!(rng.request_entropy(&mut [0]).unwrap(), 1);
    wait_pending();
    {
        let (_device, saved) = isolate();
        assert_eq!(
            read_claim(),
            0,
            "the first claim completed before retirement"
        );
        restore(saved);
    }
    hv::deactivate_guest_external_source(source).unwrap();
    for index in 0..4 {
        // SAFETY: Read the original enable bits before isolating the source again.
        assert_eq!(
            unsafe { enable_reg(index).byte_add(word).read_volatile() } & bit,
            0
        );
    }
    let (device, saved) = isolate();
    assert_eq!(
        read_claim(),
        source,
        "retirement left the first hardware claim outstanding"
    );
    assert!(
        rng.ack_interrupt()
            .contains(InterruptStatus::QUEUE_INTERRUPT)
    );
    outstanding.complete().unwrap();
    assert_eq!(rng.request_entropy(&mut [0]).unwrap(), 1);
    wait_pending();
    assert_eq!(
        read_claim(),
        0,
        "a stale completion released a later hardware claim"
    );
    assert!(
        rng.ack_interrupt()
            .contains(InterruptStatus::QUEUE_INTERRUPT)
    );
    let finish_claim = || {
        // SAFETY: The fixture owns the current claim in this context.
        unsafe { claim.write_volatile(source) };
    };
    finish_claim();
    assert_eq!(read_claim(), source);
    finish_claim();
    restore(saved);
    drop(device);
    assert_eq!(
        hv::activate_guest_external_sources(&[(source, None)], cpu),
        Err(IrqError::Busy)
    );
}
