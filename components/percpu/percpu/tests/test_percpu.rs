#![cfg(all(target_os = "linux", feature = "host-test"))]

use core::{
    num::{NonZeroU32, NonZeroUsize},
    ptr::NonNull,
};

use ax_lazyinit::LazyInit;
use ax_percpu::*;

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
fn dynamic_areas_are_scoped_initialized_and_isolated() {
    let area_count = NonZeroU32::new(4).unwrap();
    reject_invalid_regions_before_any_destination_write(area_count);

    let layout = host_test::initialize(area_count).unwrap();
    assert_eq!(host_test::initialize(area_count), Ok(layout));
    assert_eq!(
        host_test::initialize(NonZeroU32::new(3).unwrap()),
        Err(PerCpuError::LayoutAlreadyInitialized)
    );

    let required_alignment = core::mem::align_of::<OverAligned>();
    let linker_alignment = (core::ptr::addr_of!(__PERCPU_TEMPLATE_ALIGN_END) as usize)
        - (core::ptr::addr_of!(__PERCPU_TEMPLATE_ALIGN_START) as usize);
    assert_eq!(linker_alignment, required_alignment);
    assert_eq!(layout.runtime_base() % required_alignment, 0);
    assert_eq!(layout.area_stride() % required_alignment, 0);
    assert!(matches!(
        area(CpuIndex::try_from(layout.area_count() as usize).unwrap()),
        Err(PerCpuError::CpuOutOfRange { .. })
    ));

    let cpu0 = area(CpuIndex::try_from(0).unwrap()).unwrap();
    // SAFETY: this host test thread models offline CPU 0 and the host-test
    // allocation remains live until process shutdown.
    unsafe { cpu_local::install_cpu_area(cpu0.cpu_area().unwrap()) }.unwrap();

    // SAFETY: the modeled host CPU cannot migrate while this callback runs.
    unsafe {
        with_cpu_pin(|pin| {
            assert_eq!(current_area(pin), Ok(cpu0));
            assert_eq!(current_cpu_index(pin), cpu0.cpu_index());
            assert_eq!(pin.area(), cpu0.cpu_area().unwrap());
            exercise_current_area(pin, cpu0);
        })
    }
    .unwrap();

    exercise_remote_area();
}

fn reject_invalid_regions_before_any_destination_write(area_count: NonZeroU32) {
    let alignment = unsafe {
        core::ptr::addr_of!(__PERCPU_TEMPLATE_ALIGN_END)
            .offset_from(core::ptr::addr_of!(__PERCPU_TEMPLATE_ALIGN_START))
    } as usize;
    let area_size = cpu_local::cpu_area_template_size().unwrap();
    let stride = align_up(area_size, alignment);
    let mut scratch = vec![0u8; stride * area_count.get() as usize + alignment * 2];
    let aligned = align_up(scratch.as_mut_ptr() as usize, alignment);

    let misaligned_base = NonNull::new((aligned + 1) as *mut u8).unwrap();
    let region = PerCpuRegion::new(misaligned_base, stride, area_count);
    // SAFETY: the deliberately invalid region is backed by live writable test
    // storage. Validation must reject it before touching those bytes.
    assert!(matches!(
        unsafe { initialize_layout(region) },
        Err(PerCpuError::MisalignedRuntimeBase {
            alignment: rejected_alignment,
            ..
        }) if rejected_alignment == alignment
    ));

    let aligned_base = NonNull::new(aligned as *mut u8).unwrap();
    let region = PerCpuRegion::new(aligned_base, stride + 1, area_count);
    // SAFETY: as above; the stride is intentionally invalid and validation is
    // required to complete before the first destination write.
    assert!(matches!(
        unsafe { initialize_layout(region) },
        Err(PerCpuError::MisalignedStride {
            alignment: rejected_alignment,
            ..
        }) if rejected_alignment == alignment
    ));
    assert!(scratch.iter().all(|byte| *byte == 0));
}

fn exercise_current_area(pin: &CpuPin<'_>, cpu0: PerCpuArea) {
    let base = cpu0.runtime_base();
    for (offset, pointer) in [
        (BOOL.offset(), BOOL.current_ptr(pin).as_ptr() as usize),
        (U8.offset(), U8.current_ptr(pin).as_ptr() as usize),
        (U16.offset(), U16.current_ptr(pin).as_ptr() as usize),
        (U32.offset(), U32.current_ptr(pin).as_ptr() as usize),
        (U64.offset(), U64.current_ptr(pin).as_ptr() as usize),
        (USIZE.offset(), USIZE.current_ptr(pin).as_ptr() as usize),
        (STRUCT.offset(), STRUCT.current_ptr(pin).as_ptr() as usize),
        (
            OVER_ALIGNED.offset(),
            OVER_ALIGNED.current_ptr(pin).as_ptr() as usize,
        ),
    ] {
        assert_eq!(base + offset, pointer);
    }
    assert_eq!(
        OVER_ALIGNED.current_ptr(pin).as_ptr() as usize % core::mem::align_of::<OverAligned>(),
        0
    );

    BOOL.write_current(pin, true);
    U8.write_current(pin, 123);
    U16.write_current(pin, 0xabcd);
    U32.write_current(pin, 0xdead_beef);
    U64.write_current(pin, 0xa2ce_a2ce_a2ce_a2ce);
    USIZE.write_current(pin, 0xffff_0000);

    // SAFETY: this single-threaded fixture excludes migration, IRQ/re-entry,
    // and remote access for both mutation callbacks.
    unsafe {
        with_exclusive_cpu(pin, |exclusive| {
            STRUCT.with_current_mut(exclusive, |value| {
                value.foo = 0x2333;
                value.bar = 100;
            });
            OWNER_CPU_ONLY.with_current_mut(exclusive, |value| {
                assert!(value.pointer.is_null());
            });
        });
    }

    assert!(BOOL.read_current(pin));
    assert_eq!(U8.read_current(pin), 123);
    assert_eq!(U16.read_current(pin), 0xabcd);
    assert_eq!(U32.read_current(pin), 0xdead_beef);
    assert_eq!(U64.read_current(pin), 0xa2ce_a2ce_a2ce_a2ce);
    assert_eq!(USIZE.read_current(pin), 0xffff_0000);
    assert_eq!(INITIALIZED.read_current(pin), 0x5a5a_a5a5);
    BOOT_PHASE.with_current(pin, |phase| assert_eq!(*phase, BootPhase::Ready));
    NON_ZERO.with_current(pin, |value| assert_eq!(value.get(), 0x55aa));
    LAZY_VALUE.with_current(pin, |value| {
        assert_eq!(value.call_once(|| 0x1111), Some(&0x1111));
    });
    FINAL_IMAGE_REFERENCE.with_current(pin, |reference| {
        assert!(core::ptr::eq(*reference, &FINAL_IMAGE_MARKER));
    });
    STRUCT.with_current(pin, |value| {
        assert_eq!(value.foo, 0x2333);
        assert_eq!(value.bar, 100);
    });
    OVER_ALIGNED.with_current(pin, |value| {
        assert_eq!(value.marker, 0xfeed_cafe);
    });
}

fn exercise_remote_area() {
    let cpu1 = area(CpuIndex::try_from(1).unwrap()).unwrap();

    // SAFETY: CPU 1 is offline, so this test has exclusive remote ownership.
    unsafe {
        assert!(!*BOOL.remote_ptr(cpu1).as_ptr());
        assert_eq!(*U8.remote_ptr(cpu1).as_ptr(), 0);
        assert_eq!(*BOOT_PHASE.remote_ptr(cpu1).as_ptr(), BootPhase::Ready);
        assert_eq!((*NON_ZERO.remote_ptr(cpu1).as_ptr()).get(), 0x55aa);
        assert!(!(*LAZY_VALUE.remote_ptr(cpu1).as_ptr()).is_inited());

        *BOOL.remote_ptr(cpu1).as_ptr() = false;
        *U8.remote_ptr(cpu1).as_ptr() = 222;
        *U16.remote_ptr(cpu1).as_ptr() = 0x1234;
        *U32.remote_ptr(cpu1).as_ptr() = 0xf00d_f00d;
        *U64.remote_ptr(cpu1).as_ptr() = 0xfeed_feed_feed_feed;
        *USIZE.remote_ptr(cpu1).as_ptr() = 0x0000_ffff;
        *STRUCT.remote_ptr(cpu1).as_ptr() = Struct {
            foo: 0x6666,
            bar: 200,
        };
    }

    std::thread::spawn(move || {
        // SAFETY: this dedicated thread models offline CPU 1 and owns its area.
        unsafe { cpu_local::install_cpu_area(cpu1.cpu_area().unwrap()) }.unwrap();
        // SAFETY: the modeled CPU remains fixed for the callback.
        unsafe {
            with_cpu_pin(|pin| {
                assert_eq!(current_area(pin), Ok(cpu1));
                assert!(!BOOL.read_current(pin));
                assert_eq!(U8.read_current(pin), 222);
                assert_eq!(U16.read_current(pin), 0x1234);
                assert_eq!(U32.read_current(pin), 0xf00d_f00d);
                assert_eq!(U64.read_current(pin), 0xfeed_feed_feed_feed);
                assert_eq!(USIZE.read_current(pin), 0x0000_ffff);
                assert_eq!(INITIALIZED.read_current(pin), 0x5a5a_a5a5);
                BOOT_PHASE.with_current(pin, |phase| assert_eq!(*phase, BootPhase::Ready));
                NON_ZERO.with_current(pin, |value| assert_eq!(value.get(), 0x55aa));
                LAZY_VALUE.with_current(pin, |value| {
                    assert_eq!(value.call_once(|| 0x2222), Some(&0x2222));
                });
                STRUCT.with_current(pin, |value| {
                    assert_eq!(value.foo, 0x6666);
                    assert_eq!(value.bar, 200);
                });
            })
        }
        .unwrap();
    })
    .join()
    .unwrap();
}

fn align_up(value: usize, alignment: usize) -> usize {
    let mask = alignment - 1;
    value.checked_add(mask).unwrap() & !mask
}
