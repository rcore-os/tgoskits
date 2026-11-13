extern crate ax_std as std;

use core::sync::atomic::{AtomicUsize, Ordering};

use ax_plat::irq::{ControllerIrqTrigger, HwIrq, IrqError, IrqId, riscv64_hv as hv};

// QEMU virt leaves source 42 unwired; this test exclusively owns it.
const SOURCE: u32 = 42;

static RECEIVED: AtomicUsize = AtomicUsize::new(0);
static IRQ_CPU: AtomicUsize = AtomicUsize::new(usize::MAX);
struct Ingress;
#[ax_crate_interface::impl_interface]
impl hv::RiscvHvIrqSink for Ingress {
    fn publish_physical_external_claim(source: u32) -> bool {
        if source != SOURCE {
            return false;
        }
        IRQ_CPU.store(ax_hal::percpu::this_cpu_id(), Ordering::Relaxed);
        RECEIVED.fetch_add(1, Ordering::Release);
        true
    }
}
fn wait(expected: usize) {
    let began = std::time::Instant::now();
    while RECEIVED.load(Ordering::Acquire) < expected {
        if began.elapsed() >= std::time::Duration::from_secs(3) {
            panic!("APLIC IRQ not delivered");
        }
        std::thread::yield_now();
    }
    assert_eq!(RECEIVED.load(Ordering::Acquire), expected);
    assert_eq!(
        IRQ_CPU.load(Ordering::Acquire),
        1,
        "APLIC targeted the wrong CPU"
    );
}
pub fn run() {
    assert_eq!(std::os::arceos::modules::ax_hal::cpu_num(), 2);
    use std::os::arceos::api::task::{AxCpuMask, ax_set_current_affinity};
    ax_set_current_affinity(AxCpuMask::one_shot(0)).unwrap();
    while ax_hal::percpu::this_cpu_id() != 0 {
        std::thread::yield_now();
    }
    let address = rdrive::with_fdt(|tree| {
        tree.find_compatible(&["riscv,aplic"])
            .into_iter()
            .find(|node| {
                node.as_node()
                    .get_property("msi-parent")
                    .and_then(|p| p.get_u32())
                    .and_then(|ph| tree.get_by_phandle(rdrive::Phandle::from(ph)))
                    .and_then(|p| p.as_node().get_property("interrupts-extended"))
                    .and_then(|p| p.get_u32_iter().nth(1))
                    == Some(9)
            })
            .unwrap()
            .regs()[0]
            .address as usize
    })
    .unwrap();
    let base = ax_mm::iomap(address.into(), 0x4000).unwrap();
    let inject = |source: u32| {
        // SAFETY: this fixture exclusively owns sources 42, 43 and 44; the retained 16 KiB
        // mapping contains its aligned APLIC SETIPNUM command register.
        unsafe {
            core::arch::asm!("fence iorw, iorw", options(nostack));
            ((base.as_usize() + 0x2000) as *mut u32).write_volatile(source);
            core::arch::asm!("fence iorw, iorw", options(nostack));
        }
    };
    let enabled = |source: u32| {
        // SAFETY: The source SETIE word is aligned and inside the retained APLIC map.
        unsafe {
            ((base.as_usize() + 0x1e00 + (source as usize / 32) * 4) as *const u32).read_volatile()
                & (1 << (source % 32))
                != 0
        }
    };
    for source in [43, 44] {
        hv::activate_guest_external_source(source, 0, Some(ControllerIrqTrigger::EdgeRising))
            .unwrap();
    }
    // Exercise ownership through real supervisor MSI claims. IRQ exclusion
    // prevents the runtime dispatcher from taking these CPU0-targeted sources.
    {
        let _irq = ax_sync::IrqSaveGuard::new();
        let slot = somehal::irq::RiscvExternalIrqClaimSlot::new();
        let claim = |source| {
            inject(source);
            let began = std::time::Instant::now();
            loop {
                if let Some(mut active) = somehal::irq::begin_irq((1usize << 63) | 9) {
                    assert_eq!(active.id().hwirq.0, source);
                    let token = active.defer_riscv_external_completion().unwrap();
                    assert_eq!(token.source(), source);
                    return token;
                }
                assert!(
                    began.elapsed() < std::time::Duration::from_secs(3),
                    "MSI claim timeout"
                );
                core::hint::spin_loop();
            }
        };
        drop(claim(43));
        assert!(enabled(43), "dropping a claim did not complete rollback");
        slot.try_store(claim(43)).unwrap();
        assert!(!enabled(43));
        let rejected = slot.try_store(claim(44)).unwrap_err();
        assert_eq!(rejected.source(), 44);
        assert!(!enabled(44));
        drop(rejected);
        assert!(enabled(44), "rejected claim lost its completion owner");
        assert!(!enabled(43), "slot collision completed the stored claim");
        slot.take().unwrap().complete().unwrap();
        assert!(slot.take().is_none());
        assert!(enabled(43));
        // A fresh claim of the same source still has an independent owner.
        slot.try_store(claim(43)).unwrap();
        assert!(!enabled(43));
        drop(slot.take().unwrap());
        assert!(enabled(43));
        for source in [43, 44] {
            hv::deactivate_guest_external_source(source).unwrap();
        }
    }
    let domain =
        somehal::irq::domain_by_kind_fast(somehal::irq::IrqDomainKind::RiscvAplic).unwrap();
    let irq = IrqId::new(domain, HwIrq(SOURCE));
    hv::activate_guest_external_source(SOURCE, 1, Some(ControllerIrqTrigger::EdgeRising)).unwrap();
    assert!(enabled(SOURCE));
    // A claimed source stays masked even if a remote caller enables it.
    inject(SOURCE);
    wait(1);
    assert!(!enabled(SOURCE));
    somehal::irq::irq_set_enable(irq, false).unwrap();
    somehal::irq::irq_set_enable(irq, true).unwrap();
    assert!(!enabled(SOURCE), "enable bypassed a deferred completion");
    assert!(hv::complete_guest_external_source(SOURCE));
    assert!(
        !hv::complete_guest_external_source(SOURCE),
        "completion consumed twice"
    );
    assert!(enabled(SOURCE));
    // Completing a claim must not undo an explicit disable.
    inject(SOURCE);
    wait(2);
    assert!(!enabled(SOURCE));
    somehal::irq::irq_set_enable(irq, false).unwrap();
    assert!(hv::complete_guest_external_source(SOURCE));
    assert!(!enabled(SOURCE), "EOI undid disable");
    somehal::irq::irq_set_enable(irq, true).unwrap();
    // A late EOI consumes its claim but cannot revive a retired source.
    inject(SOURCE);
    wait(3);
    hv::deactivate_guest_external_source(SOURCE).unwrap();
    assert!(hv::complete_guest_external_source(SOURCE));
    assert!(!enabled(SOURCE), "old EOI undid source retirement");
    assert_eq!(somehal::irq::irq_set_enable(irq, true), Err(IrqError::Busy));
    assert_eq!(
        hv::activate_guest_external_source(SOURCE, 1, Some(ControllerIrqTrigger::EdgeRising)),
        Err(IrqError::Busy)
    );
    std::println!("APLIC_DEFERRED_STATE_OK");
}
