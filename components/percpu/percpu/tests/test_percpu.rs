#![cfg(target_os = "linux")]
#![cfg(any(feature = "non-zero-vma", feature = "sp-naive"))]

use percpu::*;

// Initial value is unsupported for testing.

#[def_percpu]
static BOOL: bool = true;

#[def_percpu]
static U8: u8 = 1;

#[def_percpu]
static U16: u16 = 2;

#[def_percpu]
static U32: u32 = 3;

#[def_percpu]
static U64: u64 = 4;

#[def_percpu]
static USIZE: usize = 5;

struct Struct {
    foo: usize,
    bar: u8,
}

#[def_percpu]
static STRUCT: Struct = Struct { foo: 6, bar: 7 };

fn tester_local_ptr(base: usize) {
    println!("bool offset: {:#x}", BOOL.offset());
    println!("u8 offset: {:#x}", U8.offset());
    println!("u16 offset: {:#x}", U16.offset());
    println!("u32 offset: {:#x}", U32.offset());
    println!("u64 offset: {:#x}", U64.offset());
    println!("usize offset: {:#x}", USIZE.offset());
    println!("struct offset: {:#x}", STRUCT.offset());
    println!();

    unsafe {
        assert_eq!(base + BOOL.offset(), BOOL.current_ptr() as usize);
        assert_eq!(base + U8.offset(), U8.current_ptr() as usize);
        assert_eq!(base + U16.offset(), U16.current_ptr() as usize);
        assert_eq!(base + U32.offset(), U32.current_ptr() as usize);
        assert_eq!(base + U64.offset(), U64.current_ptr() as usize);
        assert_eq!(base + USIZE.offset(), USIZE.current_ptr() as usize);
        assert_eq!(base + STRUCT.offset(), STRUCT.current_ptr() as usize);
    }
}

fn tester_local_is_init() {
    println!("bool initial value: {}", BOOL.read_current());
    println!("u8 initial value: {}", U8.read_current());
    println!("u16 initial value: {:#x}", U16.read_current());
    println!("u32 initial value: {:#x}", U32.read_current());
    println!("u64 initial value: {:#x}", U64.read_current());
    println!("usize initial value: {:#x}", USIZE.read_current());
    STRUCT.with_current(|s| {
        println!("struct.foo initial value: {:#x}", s.foo);
        println!("struct.bar initial value: {}", s.bar);
    });
    println!();

    assert!(BOOL.read_current());
    assert_eq!(U8.read_current(), 1);
    assert_eq!(U16.read_current(), 2);
    assert_eq!(U32.read_current(), 3);
    assert_eq!(U64.read_current(), 4);
    assert_eq!(USIZE.read_current(), 5);
    STRUCT.with_current(|s| {
        assert_eq!(s.foo, 6);
        assert_eq!(s.bar, 7);
    });
}

fn tester_local_rw() {
    BOOL.write_current(true);
    U8.write_current(123);
    U16.write_current(0xabcd);
    U32.write_current(0xdead_beef);
    U64.write_current(0xa2ce_a2ce_a2ce_a2ce);
    USIZE.write_current(0xffff_0000);

    STRUCT.with_current(|s| {
        s.foo = 0x2333;
        s.bar = 100;
    });

    println!("bool value: {}", BOOL.read_current());
    println!("u8 value: {}", U8.read_current());
    println!("u16 value: {:#x}", U16.read_current());
    println!("u32 value: {:#x}", U32.read_current());
    println!("u64 value: {:#x}", U64.read_current());
    println!("usize value: {:#x}", USIZE.read_current());

    assert_eq!(U8.read_current(), 123);
    assert_eq!(U16.read_current(), 0xabcd);
    assert_eq!(U32.read_current(), 0xdead_beef);
    assert_eq!(U64.read_current(), 0xa2ce_a2ce_a2ce_a2ce);
    assert_eq!(USIZE.read_current(), 0xffff_0000);

    STRUCT.with_current(|s| {
        println!("struct.foo value: {:#x}", s.foo);
        println!("struct.bar value: {}", s.bar);
        assert_eq!(s.foo, 0x2333);
        assert_eq!(s.bar, 100);
    });

    println!();
}

fn tester_reset_to_init() {
    BOOL.reset_to_init();
    U8.reset_to_init();
    U16.reset_to_init();
    U32.reset_to_init();
    U64.reset_to_init();
    USIZE.reset_to_init();
    STRUCT.reset_to_init();
}

#[cfg(not(feature = "sp-naive"))]
fn tester_remote_is_init(remote_id: usize) {
    unsafe {
        println!(
            "bool initial value on CPU {}: {}",
            remote_id,
            *BOOL.remote_ptr(remote_id)
        );
        println!(
            "u8 initial value on CPU {}: {}",
            remote_id,
            *U8.remote_ptr(remote_id)
        );
        println!(
            "u16 initial value on CPU {}: {:#x}",
            remote_id,
            *U16.remote_ptr(remote_id)
        );
        println!(
            "u32 initial value on CPU {}: {:#x}",
            remote_id,
            *U32.remote_ptr(remote_id)
        );
        println!(
            "u64 initial value on CPU {}: {:#x}",
            remote_id,
            *U64.remote_ptr(remote_id)
        );
        println!(
            "usize initial value on CPU {}: {:#x}",
            remote_id,
            *USIZE.remote_ptr(remote_id)
        );
        let remote_struct = STRUCT.remote_ptr(remote_id);
        println!(
            "struct.foo initial value on CPU {}: {:#x}",
            remote_id,
            (*remote_struct).foo
        );
        println!(
            "struct.bar initial value on CPU {}: {}",
            remote_id,
            (*remote_struct).bar
        );
        println!();

        assert!(*BOOL.remote_ptr(remote_id));
        assert_eq!(*U8.remote_ptr(remote_id), 1);
        assert_eq!(*U16.remote_ptr(remote_id), 2);
        assert_eq!(*U32.remote_ptr(remote_id), 3);
        assert_eq!(*U64.remote_ptr(remote_id), 4);
        assert_eq!(*USIZE.remote_ptr(remote_id), 5);
        let remote_struct = STRUCT.remote_ptr(remote_id);
        assert_eq!((*remote_struct).foo, 6);
        assert_eq!((*remote_struct).bar, 7);
    }
}

#[cfg(not(feature = "sp-naive"))]
fn tester_remote_rw(remote_id: usize) {
    unsafe {
        *BOOL.remote_ref_mut_raw(remote_id) = false;
        *U8.remote_ref_mut_raw(remote_id) = 222;
        *U16.remote_ref_mut_raw(remote_id) = 0x1234;
        *U32.remote_ref_mut_raw(remote_id) = 0xf00d_f00d;
        *U64.remote_ref_mut_raw(remote_id) = 0xfeed_feed_feed_feed;
        *USIZE.remote_ref_mut_raw(remote_id) = 0x0000_ffff;
        *STRUCT.remote_ref_mut_raw(remote_id) = Struct {
            foo: 0x2333,
            bar: 100,
        };

        println!(
            "bool value on CPU {}: {}",
            remote_id,
            *BOOL.remote_ptr(remote_id)
        );
        println!(
            "u8 value on CPU {}: {}",
            remote_id,
            *U8.remote_ptr(remote_id)
        );
        println!(
            "u16 value on CPU {}: {:#x}",
            remote_id,
            *U16.remote_ptr(remote_id)
        );
        println!(
            "u32 value on CPU {}: {:#x}",
            remote_id,
            *U32.remote_ptr(remote_id)
        );
        println!(
            "u64 value on CPU {}: {:#x}",
            remote_id,
            *U64.remote_ptr(remote_id)
        );
        println!(
            "usize value on CPU {}: {:#x}",
            remote_id,
            *USIZE.remote_ptr(remote_id)
        );
        let remote_struct = STRUCT.remote_ptr(remote_id);
        println!(
            "struct.foo value on CPU {}: {:#x}",
            remote_id,
            (*remote_struct).foo
        );
        println!(
            "struct.bar value on CPU {}: {}",
            remote_id,
            (*remote_struct).bar
        );
        println!();

        assert!(!*BOOL.remote_ptr(remote_id));
        assert_eq!(*U8.remote_ptr(remote_id), 222);
        assert_eq!(*U16.remote_ptr(remote_id), 0x1234);
        assert_eq!(*U32.remote_ptr(remote_id), 0xf00d_f00d);
        assert_eq!(*U64.remote_ptr(remote_id), 0xfeed_feed_feed_feed);
        assert_eq!(*USIZE.remote_ptr(remote_id), 0x0000_ffff);
        let remote_struct = STRUCT.remote_ptr(remote_id);
        assert_eq!((*remote_struct).foo, 0x2333);
        assert_eq!((*remote_struct).bar, 100);
    }
}

fn test_percpu_local(base: usize) {
    tester_local_ptr(base);
    tester_local_is_init();
    tester_local_rw();
    tester_reset_to_init();
    tester_local_is_init();
}

#[cfg(not(feature = "sp-naive"))]
fn test_percpu_remote(remote_id: usize) {
    tester_remote_is_init(remote_id);
    tester_remote_rw(remote_id);
}

#[cfg(feature = "sp-naive")]
#[test]
fn test_percpu_sp_naive() {
    println!("Testing single-threaded mode (sp-naive)...");

    // Test init_static()
    assert_eq!(init_in_place(), Ok(1));
    init_percpu_reg(0);
    test_percpu_local(0);

    // Test init() with parameters (should be ignored in sp-naive mode)
    assert_eq!(init(std::ptr::null_mut(), 1), Ok(1));
}

#[cfg(not(feature = "sp-naive"))]
#[test]
fn test_percpu_multi_core_in_place() {
    // Test init_static() - uses .percpu section

    println!("\nTesting multi-threaded mode with init_static()...");

    assert_eq!(init_in_place(), Ok(4));
    init_percpu_reg(0);

    let base_from_reg = read_percpu_reg();
    let base_calculated = percpu_area_base(0);
    assert_eq!(base_from_reg, base_calculated);

    println!("per-CPU area base (calculated) = {:#x}", base_calculated);
    println!("per-CPU area base (read) = {:#x}", base_from_reg);
    println!("per-CPU area size = {}", percpu_area_size());

    test_percpu_local(base_from_reg);
    test_percpu_remote(1);
    test_percpu_remote(2);
    test_percpu_remote(3);
}

#[cfg(not(feature = "sp-naive"))]
#[test]
fn test_percpu_multi_core_dynamic() {
    println!("\nTesting multi-threaded mode with init()...");

    let layout = percpu_area_layout_expected(4);
    let base = unsafe { std::alloc::alloc(layout) as usize };

    assert_eq!(init(base as *mut u8, 4), Ok(4));
    init_percpu_reg(0);

    let base_from_reg = read_percpu_reg();
    let base_calculated = percpu_area_base(0);
    assert_eq!(base_from_reg, base);
    assert_eq!(base_calculated, base);

    println!("per-CPU area base (allocated) = {:#x}", base);
    println!("per-CPU area base (read) = {:#x}", base_from_reg);
    println!("per-CPU area size = {}", percpu_area_size());

    test_percpu_local(base);
    test_percpu_remote(1);
    test_percpu_remote(2);
    test_percpu_remote(3);
}

#[cfg(not(feature = "sp-naive"))]
#[test]
fn test_percpu_init_rejects_invalid_base() {
    assert!(init(std::ptr::null_mut(), 1).is_err());

    let layout = percpu_area_layout_expected(1);
    let base = unsafe { std::alloc::alloc(layout) };
    assert!(!base.is_null());
    assert!(init(unsafe { base.add(1) }, 1).is_err());
    unsafe { std::alloc::dealloc(base, layout) };
}
