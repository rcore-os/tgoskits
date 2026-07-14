#![cfg(all(target_os = "linux", feature = "host-test", feature = "non-zero-vma"))]

use ax_percpu::*;

unsafe extern "C" {
    static __AX_PERCPU_LINKER_ALIGNMENT_START: u8;
    static __AX_PERCPU_LINKER_ALIGNMENT_END: u8;
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

#[cfg(feature = "custom-base")]
struct OwnerCpuOnly {
    pointer: *mut u8,
}

#[cfg(feature = "custom-base")]
#[def_percpu]
static OWNER_CPU_ONLY: OwnerCpuOnly = OwnerCpuOnly {
    pointer: core::ptr::null_mut(),
};

#[test]
fn test_percpu() {
    println!("feature = \"sp-naive\": {}", cfg!(feature = "sp-naive"));

    // SAFETY: a host test thread cannot be migrated between modeled CPU-local
    // anchors, and this token never leaves the thread.
    let migration_pin_guard = unsafe { CpuPin::new_unchecked() };
    let migration_pin = &migration_pin_guard;

    #[cfg(not(feature = "sp-naive"))]
    let installed = {
        assert_eq!(init(), 4);
        assert_eq!(init(), 0, "CPU-local storage initialization is one-shot");
        let layout = layout().unwrap();
        let required_alignment = core::mem::align_of::<OverAligned>();
        let linker_alignment = (core::ptr::addr_of!(__AX_PERCPU_LINKER_ALIGNMENT_END) as usize)
            - (core::ptr::addr_of!(__AX_PERCPU_LINKER_ALIGNMENT_START) as usize);
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
        // SAFETY: `init` owns the complete aligned host fixture until process
        // exit and copied the linked template into every area.
        assert_eq!(unsafe { install_layout(layout) }, Ok(()));
        let conflicting_layout = PerCpuLayoutV1 {
            runtime_base: layout.runtime_base + layout.area_stride,
            ..layout
        };
        // SAFETY: the request is rejected against the already installed live
        // fixture before it can publish a different address range.
        assert!(matches!(
            unsafe { install_layout(conflicting_layout) },
            Err(PerCpuError::LayoutAlreadyInstalled { .. })
        ));
        assert!(matches!(
            area(CpuIndex::try_from(layout.area_count as usize).unwrap()),
            Err(PerCpuError::CpuOutOfRange { .. })
        ));

        let area = area(CpuIndex::try_from(0).unwrap()).unwrap();
        let foreign_prefix = Box::leak(Box::new(CpuAreaPrefix::TEMPLATE));
        let foreign_base = core::ptr::from_mut(foreign_prefix).addr();
        let foreign_anchor = CpuLocalAnchor::new(
            foreign_base,
            PerCpuRelocation::from_bases(foreign_base, area.runtime_base()),
        );
        // SAFETY: the leaked prefix is aligned, initialized, writable, and
        // remains mapped. It models an early-boot anchor outside the layout.
        unsafe { ax_cpu_local::install_current(foreign_anchor) };
        assert!(matches!(
            bound_current(migration_pin),
            Err(PerCpuError::CurrentAreaOutsideLayout { runtime_base })
                if runtime_base == foreign_base
        ));

        let installed = unsafe { bind_current(area) }.unwrap();
        // SAFETY: the test keeps its migration pin live across both reads.
        let committed_base = unsafe { ax_cpu_local::current_area_base_raw(migration_pin) };
        assert!(matches!(
            unsafe { bind_current(area) },
            Err(PerCpuError::AreaAlreadyBound { cpu_index }) if cpu_index == area.cpu_index()
        ));
        // SAFETY: a recoverable second-bind rejection must leave the committed
        // architecture anchor untouched while the same migration pin is live.
        assert_eq!(
            unsafe { ax_cpu_local::current_area_base_raw(migration_pin) },
            committed_base
        );
        assert_eq!(installed.header().cpu_index(), Some(area.cpu_index()));
        assert_eq!(installed.header().self_base(), area.runtime_base());
        assert_eq!(installed.header().relocation(), area.relocation());
        assert_eq!(installed.header().generation(), 1);
        assert_ne!(installed.header().cookie(), 0);
        assert_eq!(installed.verify(migration_pin), Ok(()));
        let bound_pin = bound_current(migration_pin).unwrap();
        assert_eq!(current_cpu_index(&bound_pin), Ok(area.cpu_index()));
        println!(
            "per-CPU area base (calculated) = {:#x}",
            area.runtime_base()
        );
        println!("per-CPU area size = {}", area.area_size());
        installed
    };

    #[cfg(feature = "sp-naive")]
    let base = 0;

    #[cfg(not(feature = "sp-naive"))]
    let base = installed.area().runtime_base();

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

    #[cfg(feature = "custom-base")]
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

    STRUCT.with_current_ref(pin, |s| {
        println!("struct.foo value: {:#x}", s.foo);
        println!("struct.bar value: {}", s.bar);
        assert_eq!(s.foo, 0x2333);
        assert_eq!(s.bar, 100);
    });
    OVER_ALIGNED.with_current_ref(pin, |value| {
        assert_eq!(value.marker, 0xfeed_cafe);
    });

    #[cfg(not(feature = "sp-naive"))]
    test_remote_access();
}

#[cfg(not(feature = "sp-naive"))]
fn test_remote_access() {
    let cpu1 = CpuIndex::try_from(1).unwrap();
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
        let installed = unsafe { bind_current(area(cpu1).unwrap()) }.unwrap();
        // SAFETY: this host thread remains the modeled CPU 1 for its lifetime.
        let migration_pin_guard = unsafe { CpuPin::new_unchecked() };
        let migration_pin = &migration_pin_guard;
        assert_eq!(installed.verify(migration_pin), Ok(()));
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
