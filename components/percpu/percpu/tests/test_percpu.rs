#![cfg(all(target_os = "linux", feature = "host-test"))]

use core::num::{NonZeroU32, NonZeroUsize};

use ax_lazyinit::LazyInit;
use ax_percpu::*;

struct HostCpuLocalPlatform;

#[cpu_local::impl_extern_trait(name = "cpu-local_0_1", abi = "rust")]
impl cpu_local::CpuLocalPlatformV1 for HostCpuLocalPlatform {
    fn current_cpu_binding() -> cpu_local::CpuBindingResultV1 {
        // SAFETY: every host-test access is covered by a thread-local CpuPin;
        // this explicit fake provider is the only layer allowed to interpret
        // the host architecture fixture's raw anchor.
        let pin = unsafe { cpu_local::CpuPin::new_unchecked() };
        // SAFETY: the local pin covers this value-only test-register read.
        let area_base = unsafe { cpu_local::raw::current_area_base_raw(&pin) };
        if area_base == 0 || !area_base.is_multiple_of(core::mem::align_of::<CpuAreaPrefixV2>()) {
            return cpu_local::CpuBindingResultV1::error(cpu_local::CpuLocalStatus::NotInitialized);
        }
        // SAFETY: this test installs only areas from ax-percpu's
        // shutdown-lifetime host allocation.
        let prefix = unsafe { &*(area_base as *const CpuAreaPrefixV2) };
        let binding = prefix.header().binding();
        if binding.area_base != area_base
            || cpu_local::CpuAreaInitV2::from_binding(binding).is_none()
        {
            return cpu_local::CpuBindingResultV1::error(cpu_local::CpuLocalStatus::InvalidBinding);
        }
        cpu_local::CpuBindingResultV1::ok(binding)
    }

    fn get_tp() -> usize {
        0
    }

    unsafe fn set_tp(_value: usize) -> cpu_local::CpuLocalStatus {
        cpu_local::CpuLocalStatus::Unsupported
    }

    fn current_thread() -> usize {
        let result = Self::current_cpu_binding();
        if result.status != cpu_local::CpuLocalStatus::Ok {
            return 0;
        }
        // SAFETY: successful binding validation proves a live PrefixV2.
        let prefix = unsafe { &*(result.binding.area_base as *const CpuAreaPrefixV2) };
        prefix.runtime_anchor().current_thread_raw()
    }
}

unsafe extern "C" {
    static __PERCPU_TEMPLATE_ALIGN_START: u8;
    static __PERCPU_TEMPLATE_ALIGN_END: u8;
}

#[def_percpu]
static BOOL: bool = false;

#[def_percpu]
static U8: u8 = 0;

#[def_percpu]
static U16: u16 = 0;

#[def_percpu]
static U32: u32 = 0;

#[def_percpu]
static U64: u64 = 0;

#[def_percpu]
static USIZE: usize = 0;

#[def_percpu]
static INITIALIZED: usize = 0x5a5a_a5a5;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum BootPhase {
    Ready = 7,
}

#[def_percpu]
static BOOT_PHASE: BootPhase = BootPhase::Ready;

#[def_percpu]
static NON_ZERO: NonZeroUsize = NonZeroUsize::new(0x55aa).expect("constant must be nonzero");

#[def_percpu]
static LAZY_VALUE: LazyInit<usize> = LazyInit::new();

static FINAL_IMAGE_MARKER: u8 = 0x5a;

#[def_percpu]
static FINAL_IMAGE_REFERENCE: &'static u8 = &FINAL_IMAGE_MARKER;

struct Struct {
    foo: usize,
    bar: u8,
}

#[def_percpu]
static STRUCT: Struct = Struct { foo: 0, bar: 0 };

#[derive(Clone, Copy)]
#[repr(C, align(8192))]
struct OverAligned {
    marker: usize,
}

#[def_percpu]
static OVER_ALIGNED: OverAligned = OverAligned {
    marker: 0xfeed_cafe,
};

struct OwnerCpuOnly {
    pointer: *mut u8,
}

#[def_percpu]
static OWNER_CPU_ONLY: OwnerCpuOnly = OwnerCpuOnly {
    pointer: core::ptr::null_mut(),
};

#[test]
fn test_percpu() {
    // SAFETY: a host test thread cannot be migrated between modeled CPU-local
    // anchors, and this token never leaves the thread.
    let migration_pin_guard = unsafe { CpuPin::new_unchecked() };
    let migration_pin = &migration_pin_guard;

    let installed_area = {
        let area_count = NonZeroU32::new(4).unwrap();
        let layout = host_test::initialize(area_count).unwrap();
        assert_eq!(host_test::initialize(area_count), Ok(layout));
        let required_alignment = core::mem::align_of::<OverAligned>();
        let linker_alignment = (core::ptr::addr_of!(__PERCPU_TEMPLATE_ALIGN_END) as usize)
            - (core::ptr::addr_of!(__PERCPU_TEMPLATE_ALIGN_START) as usize);
        assert_eq!(linker_alignment, required_alignment);
        assert_eq!(layout.runtime_base % required_alignment, 0);
        assert_eq!(layout.area_stride % required_alignment, 0);

        let misaligned_base = PerCpuLayoutV1 {
            runtime_base: layout.runtime_base + 64,
            ..layout
        };
        assert!(matches!(
            misaligned_base.validate(),
            Err(PerCpuError::MisalignedRuntimeBase { alignment, .. })
                if alignment == required_alignment
        ));
        let misaligned_stride = PerCpuLayoutV1 {
            area_stride: layout.area_stride + 64,
            ..layout
        };
        assert!(matches!(
            misaligned_stride.validate(),
            Err(PerCpuError::MisalignedStride { alignment, .. })
                if alignment == required_alignment
        ));
        // SAFETY: the same raw layout was already consumed by `init`; this call
        // verifies that typed construction cannot run a second time.
        assert!(matches!(
            unsafe {
                initialize_layout(PerCpuLayoutInitV2::new(
                    layout,
                    1,
                    1,
                    cpu_local::image_register_mode(),
                    HostLevelV1::Supervisor,
                ))
            },
            Err(PerCpuError::LayoutAlreadyInitialized)
        ));
        assert!(matches!(
            area(CpuIndex::try_from(layout.area_count as usize).unwrap()),
            Err(PerCpuError::CpuOutOfRange { .. })
        ));

        let area = area(CpuIndex::try_from(0).unwrap()).unwrap();
        assert!(matches!(
            bound_current(migration_pin),
            Err(PerCpuError::CurrentAreaUnbound)
        ));

        // SAFETY: this explicit host platform fixture owns the offline CPU 0
        // binding window and keeps the initialized area live until shutdown.
        unsafe { cpu_local::raw::install_binding(area.binding()) }.unwrap();
        let committed_binding = cpu_local::platform::current_cpu_binding();
        assert_eq!(
            cpu_local::platform::current_cpu_binding(),
            committed_binding
        );
        let header = area.prefix().header();
        assert_eq!(header.cpu_index(), Some(area.cpu_index()));
        assert_eq!(header.self_base(), area.runtime_base());
        assert_eq!(header.generation(), 1);
        assert_ne!(header.cookie(), 0);
        assert_eq!(
            header.register_mode(),
            Some(cpu_local::image_register_mode())
        );
        let bound_pin = bound_current(migration_pin).unwrap();
        assert_eq!(current_cpu_index(&bound_pin), Ok(area.cpu_index()));
        println!(
            "per-CPU area base (calculated) = {:#x}",
            area.runtime_base()
        );
        println!("per-CPU area size = {}", area.area_size());
        area
    };

    let base = installed_area.runtime_base();

    let bound_pin = bound_current(migration_pin).unwrap();
    let pin = &bound_pin;

    println!("bool offset: {:#x}", BOOL.offset());
    println!("u8 offset: {:#x}", U8.offset());
    println!("u16 offset: {:#x}", U16.offset());
    println!("u32 offset: {:#x}", U32.offset());
    println!("u64 offset: {:#x}", U64.offset());
    println!("usize offset: {:#x}", USIZE.offset());
    println!("struct offset: {:#x}", STRUCT.offset());
    println!("over-aligned offset: {:#x}", OVER_ALIGNED.offset());
    println!();

    assert_eq!(base + BOOL.offset(), BOOL.current_ptr(pin) as usize);
    assert_eq!(base + U8.offset(), U8.current_ptr(pin) as usize);
    assert_eq!(base + U16.offset(), U16.current_ptr(pin) as usize);
    assert_eq!(base + U32.offset(), U32.current_ptr(pin) as usize);
    assert_eq!(base + U64.offset(), U64.current_ptr(pin) as usize);
    assert_eq!(base + USIZE.offset(), USIZE.current_ptr(pin) as usize);
    assert_eq!(base + STRUCT.offset(), STRUCT.current_ptr(pin) as usize);
    assert_eq!(
        base + OVER_ALIGNED.offset(),
        OVER_ALIGNED.current_ptr(pin) as usize
    );
    assert_eq!(
        OVER_ALIGNED.current_ptr(pin) as usize % core::mem::align_of::<OverAligned>(),
        0
    );

    BOOL.write_current(pin, true);
    U8.write_current(pin, 123);
    U16.write_current(pin, 0xabcd);
    U32.write_current(pin, 0xdead_beef);
    U64.write_current(pin, 0xa2ce_a2ce_a2ce_a2ce);
    USIZE.write_current(pin, 0xffff_0000);

    // SAFETY: this single-threaded test owns the bound CPU-local area and no
    // IRQ, nested callback, or remote CPU can alias `STRUCT`.
    unsafe {
        STRUCT.with_current_mut_raw(migration_pin, |s| {
            s.foo = 0x2333;
            s.bar = 100;
        });
    }

    // SAFETY: this single-threaded test exclusively owns the bound CPU-local
    // area. This also verifies that owner-only `!Sync` values remain usable.
    unsafe {
        OWNER_CPU_ONLY
            .with_current_mut_raw(migration_pin, |value| assert!(value.pointer.is_null()));
    }

    println!("bool value: {}", BOOL.read_current(pin));
    println!("u8 value: {}", U8.read_current(pin));
    println!("u16 value: {:#x}", U16.read_current(pin));
    println!("u32 value: {:#x}", U32.read_current(pin));
    println!("u64 value: {:#x}", U64.read_current(pin));
    println!("usize value: {:#x}", USIZE.read_current(pin));

    assert_eq!(U8.read_current(pin), 123);
    assert_eq!(U16.read_current(pin), 0xabcd);
    assert_eq!(U32.read_current(pin), 0xdead_beef);
    assert_eq!(U64.read_current(pin), 0xa2ce_a2ce_a2ce_a2ce);
    assert_eq!(USIZE.read_current(pin), 0xffff_0000);
    assert_eq!(INITIALIZED.read_current(pin), 0x5a5a_a5a5);
    BOOT_PHASE.with_current_ref(pin, |phase| assert_eq!(*phase, BootPhase::Ready));
    NON_ZERO.with_current_ref(pin, |value| assert_eq!(value.get(), 0x55aa));
    LAZY_VALUE.with_current_ref(pin, |value| {
        assert_eq!(value.call_once(|| 0x1111), Some(&0x1111));
    });
    FINAL_IMAGE_REFERENCE.with_current_ref(pin, |reference| {
        assert!(core::ptr::eq(*reference, &FINAL_IMAGE_MARKER));
        assert_eq!(**reference, 0x5a);
    });

    STRUCT.with_current_ref(pin, |s| {
        println!("struct.foo value: {:#x}", s.foo);
        println!("struct.bar value: {}", s.bar);
        assert_eq!(s.foo, 0x2333);
        assert_eq!(s.bar, 100);
    });
    OVER_ALIGNED.with_current_ref(pin, |value| {
        assert_eq!(value.marker, 0xfeed_cafe);
    });

    test_remote_access();
}

fn test_remote_access() {
    let cpu1 = CpuIndex::try_from(1).unwrap();
    // Every typed initializer constructs an independent CPU-owned value. CPU
    // zero's mutation above must not affect CPU one before any remote write.
    unsafe {
        assert!(!*BOOL.remote_ptr(cpu1).unwrap());
        assert_eq!(*U8.remote_ptr(cpu1).unwrap(), 0);
        assert_eq!(*BOOT_PHASE.remote_ptr(cpu1).unwrap(), BootPhase::Ready);
        assert_eq!((*NON_ZERO.remote_ptr(cpu1).unwrap()).get(), 0x55aa);
        assert!(!(*LAZY_VALUE.remote_ptr(cpu1).unwrap()).is_inited());
        assert!(core::ptr::eq(
            *FINAL_IMAGE_REFERENCE.remote_ptr(cpu1).unwrap(),
            &FINAL_IMAGE_MARKER,
        ));
    }
    // test remote write
    unsafe {
        *BOOL.remote_ref_mut_raw(cpu1).unwrap() = false;
        *U8.remote_ref_mut_raw(cpu1).unwrap() = 222;
        *U16.remote_ref_mut_raw(cpu1).unwrap() = 0x1234;
        *U32.remote_ref_mut_raw(cpu1).unwrap() = 0xf00d_f00d;
        *U64.remote_ref_mut_raw(cpu1).unwrap() = 0xfeed_feed_feed_feed;
        *USIZE.remote_ref_mut_raw(cpu1).unwrap() = 0x0000_ffff;

        *STRUCT.remote_ref_mut_raw(cpu1).unwrap() = Struct {
            foo: 0x6666,
            bar: 200,
        };
    }

    // test remote read
    unsafe {
        assert!(!*BOOL.remote_ptr(cpu1).unwrap());
        assert_eq!(*U8.remote_ptr(cpu1).unwrap(), 222);
        assert_eq!(*U16.remote_ptr(cpu1).unwrap(), 0x1234);
        assert_eq!(*U32.remote_ptr(cpu1).unwrap(), 0xf00d_f00d);
        assert_eq!(*U64.remote_ptr(cpu1).unwrap(), 0xfeed_feed_feed_feed);
        assert_eq!(*USIZE.remote_ptr(cpu1).unwrap(), 0x0000_ffff);

        let s = STRUCT.remote_ref_raw(cpu1).unwrap();
        assert_eq!(s.foo, 0x6666);
        assert_eq!(s.bar, 200);
    }

    // A physical CPU binds exactly once. Model CPU 1 with a distinct host
    // thread instead of rebinding the CPU 0 thread's architecture anchor.
    std::thread::spawn(move || {
        let cpu_area = area(cpu1).unwrap();
        // SAFETY: this dedicated host thread is the offline CPU 1 platform
        // binder and the initialized area remains live until process shutdown.
        unsafe { cpu_local::raw::install_binding(cpu_area.binding()) }.unwrap();
        // SAFETY: this host thread remains the modeled CPU 1 for its lifetime.
        let migration_pin_guard = unsafe { CpuPin::new_unchecked() };
        let migration_pin = &migration_pin_guard;
        let bound_pin = bound_current(migration_pin).unwrap();
        let pin = &bound_pin;
        assert_eq!(current_cpu_index(pin), Ok(cpu1));

        println!();
        println!("bool value on CPU 1: {}", BOOL.read_current(pin));
        println!("u8 value on CPU 1: {}", U8.read_current(pin));
        println!("u16 value on CPU 1: {:#x}", U16.read_current(pin));
        println!("u32 value on CPU 1: {:#x}", U32.read_current(pin));
        println!("u64 value on CPU 1: {:#x}", U64.read_current(pin));
        println!("usize value on CPU 1: {:#x}", USIZE.read_current(pin));

        assert!(!BOOL.read_current(pin));
        assert_eq!(U8.read_current(pin), 222);
        assert_eq!(U16.read_current(pin), 0x1234);
        assert_eq!(U32.read_current(pin), 0xf00d_f00d);
        assert_eq!(U64.read_current(pin), 0xfeed_feed_feed_feed);
        assert_eq!(USIZE.read_current(pin), 0x0000_ffff);
        assert_eq!(INITIALIZED.read_current(pin), 0x5a5a_a5a5);
        BOOT_PHASE.with_current_ref(pin, |phase| assert_eq!(*phase, BootPhase::Ready));
        NON_ZERO.with_current_ref(pin, |value| assert_eq!(value.get(), 0x55aa));
        LAZY_VALUE.with_current_ref(pin, |value| {
            assert_eq!(value.call_once(|| 0x2222), Some(&0x2222));
        });
        FINAL_IMAGE_REFERENCE.with_current_ref(pin, |reference| {
            assert!(core::ptr::eq(*reference, &FINAL_IMAGE_MARKER));
            assert_eq!(**reference, 0x5a);
        });

        STRUCT.with_current_ref(pin, |s| {
            println!("struct.foo value on CPU 1: {:#x}", s.foo);
            println!("struct.bar value on CPU 1: {}", s.bar);
            assert_eq!(s.foo, 0x6666);
            assert_eq!(s.bar, 200);
        });
    })
    .join()
    .unwrap();
}
