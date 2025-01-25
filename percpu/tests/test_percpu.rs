#![cfg(not(target_os = "macos"))]

use percpu::*;

// Initial value is unsupported for testing.

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

struct Struct {
    foo: usize,
    bar: u8,
}

#[def_percpu]
static STRUCT: Struct = Struct { foo: 0, bar: 0 };

#[cfg(target_os = "linux")]
#[test]
fn test_percpu() {
    println!("feature = \"sp-naive\": {}", cfg!(feature = "sp-naive"));

    #[cfg(feature = "sp-naive")]
    let base = 0;

    #[cfg(not(feature = "sp-naive"))]
    let base = {
        assert_eq!(init(), 4);
        unsafe { write_percpu_reg(percpu_area_base(0)) };

        let base = read_percpu_reg();
        println!("per-CPU area base = {:#x}", base);
        println!("per-CPU area size = {}", percpu_area_size());
        base
    };

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

    #[cfg(not(feature = "sp-naive"))]
    test_remote_access();
}

#[cfg(all(target_os = "linux", not(feature = "sp-naive")))]
fn test_remote_access() {
    // test remote write
    unsafe {
        *BOOL.remote_ref_mut_raw(1) = false;
        *U8.remote_ref_mut_raw(1) = 222;
        *U16.remote_ref_mut_raw(1) = 0x1234;
        *U32.remote_ref_mut_raw(1) = 0xf00d_f00d;
        *U64.remote_ref_mut_raw(1) = 0xfeed_feed_feed_feed;
        *USIZE.remote_ref_mut_raw(1) = 0x0000_ffff;

        *STRUCT.remote_ref_mut_raw(1) = Struct {
            foo: 0x6666,
            bar: 200,
        };
    }

    // test remote read
    unsafe {
        assert!(!*BOOL.remote_ptr(1));
        assert_eq!(*U8.remote_ptr(1), 222);
        assert_eq!(*U16.remote_ptr(1), 0x1234);
        assert_eq!(*U32.remote_ptr(1), 0xf00d_f00d);
        assert_eq!(*U64.remote_ptr(1), 0xfeed_feed_feed_feed);
        assert_eq!(*USIZE.remote_ptr(1), 0x0000_ffff);

        let s = STRUCT.remote_ref_raw(1);
        assert_eq!(s.foo, 0x6666);
        assert_eq!(s.bar, 200);
    }

    // test read on another CPU
    unsafe { write_percpu_reg(percpu_area_base(1)) }; // we are now on CPU 1

    println!();
    println!("bool value on CPU 1: {}", BOOL.read_current());
    println!("u8 value on CPU 1: {}", U8.read_current());
    println!("u16 value on CPU 1: {:#x}", U16.read_current());
    println!("u32 value on CPU 1: {:#x}", U32.read_current());
    println!("u64 value on CPU 1: {:#x}", U64.read_current());
    println!("usize value on CPU 1: {:#x}", USIZE.read_current());

    assert!(!BOOL.read_current());
    assert_eq!(U8.read_current(), 222);
    assert_eq!(U16.read_current(), 0x1234);
    assert_eq!(U32.read_current(), 0xf00d_f00d);
    assert_eq!(U64.read_current(), 0xfeed_feed_feed_feed);
    assert_eq!(USIZE.read_current(), 0x0000_ffff);

    STRUCT.with_current(|s| {
        println!("struct.foo value on CPU 1: {:#x}", s.foo);
        println!("struct.bar value on CPU 1: {}", s.bar);
        assert_eq!(s.foo, 0x6666);
        assert_eq!(s.bar, 200);
    });
}
