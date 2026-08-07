// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// ...

extern crate alloc;

use divan::Bencher;
use paste::paste;

mod common;
mod v1_three_vec;
mod v2_btree_nocache;
mod v3_btree_cache;
mod v4_sorted_vec;

use common::Registry;
use v1_three_vec::V1Registry;
use v2_btree_nocache::V2Registry;
use v3_btree_cache::V3Registry;
use v4_sorted_vec::V4Registry;

fn main() {
    divan::main();
}

// ── Macro: generate 4 version-specific bench fns from one body ──────
//
// Each invocation generates four functions named
//   v1_$fn, v2_$fn, v3_$fn, v4_$fn
// so they never collide across benchmark scenarios.

macro_rules! per_version {
    (
        #[bench($($attr:tt)*)]
        fn $fn:ident($bencher:ident: Bencher, $n:ident: usize) $body:block
    ) => {
        paste! {
            #[divan::bench($($attr)*)]
            fn [<v1_ $fn>]($bencher: Bencher, $n: usize) {
                type R = V1Registry;
                $body
            }
            #[divan::bench($($attr)*)]
            fn [<v2_ $fn>]($bencher: Bencher, $n: usize) {
                type R = V2Registry;
                $body
            }
            #[divan::bench($($attr)*)]
            fn [<v3_ $fn>]($bencher: Bencher, $n: usize) {
                type R = V3Registry;
                $body
            }
            #[divan::bench($($attr)*)]
            fn [<v4_ $fn>]($bencher: Bencher, $n: usize) {
                type R = V4Registry;
                $body
            }
        }
    };
}

// ── MMIO lookup scenarios ───────────────────────────────────────────

#[divan::bench_group]
mod mmio {
    use super::*;

    per_version! {
        #[bench(args = [4usize, 8, 16, 32, 64, 128])]
        fn hit_first(b: Bencher, n: usize) {
            let reg = <R>::new_with_devices(n);
            let addr = common::mmio_addr(0);
            b.bench_local(|| {
                divan::black_box(reg.lookup_mmio(divan::black_box(addr)))
            });
        }
    }

    per_version! {
        #[bench(args = [4usize, 8, 16, 32, 64, 128])]
        fn hit_mid(b: Bencher, n: usize) {
            let reg = <R>::new_with_devices(n);
            let addr = common::mmio_addr(n / 2);
            b.bench_local(|| {
                divan::black_box(reg.lookup_mmio(divan::black_box(addr)))
            });
        }
    }

    per_version! {
        #[bench(args = [4usize, 8, 16, 32, 64, 128])]
        fn hit_last(b: Bencher, n: usize) {
            let reg = <R>::new_with_devices(n);
            let addr = common::mmio_addr(n - 1);
            b.bench_local(|| {
                divan::black_box(reg.lookup_mmio(divan::black_box(addr)))
            });
        }
    }

    per_version! {
        #[bench(args = [4usize, 8, 16, 32, 64, 128])]
        fn miss_before(b: Bencher, n: usize) {
            let reg = <R>::new_with_devices(n);
            let addr = common::MMIO_BASE - 0x100;
            b.bench_local(|| {
                divan::black_box(reg.lookup_mmio(divan::black_box(addr)))
            });
        }
    }

    per_version! {
        #[bench(args = [4usize, 8, 16, 32, 64, 128])]
        fn miss_between(b: Bencher, n: usize) {
            let reg = <R>::new_with_devices(n);
            let addr = common::mmio_addr_between(n / 2);
            b.bench_local(|| {
                divan::black_box(reg.lookup_mmio(divan::black_box(addr)))
            });
        }
    }

    per_version! {
        #[bench(args = [4usize, 8, 16, 32, 64, 128])]
        fn miss_after(b: Bencher, n: usize) {
            let reg = <R>::new_with_devices(n);
            let addr = common::mmio_addr(n - 1) + common::MMIO_STRIDE * 2;
            b.bench_local(|| {
                divan::black_box(reg.lookup_mmio(divan::black_box(addr)))
            });
        }
    }
}

// ── Port I/O lookup scenarios ───────────────────────────────────────

#[divan::bench_group]
mod port {
    use super::*;

    per_version! {
        #[bench(args = [4usize, 8, 16, 32, 64, 128])]
        fn hit_mid(b: Bencher, n: usize) {
            let reg = <R>::new_with_devices(n);
            let addr = common::port_addr(n / 2);
            b.bench_local(|| {
                divan::black_box(reg.lookup_port(divan::black_box(addr)))
            });
        }
    }

    per_version! {
        #[bench(args = [4usize, 8, 16, 32, 64, 128])]
        fn miss(b: Bencher, n: usize) {
            let reg = <R>::new_with_devices(n);
            let addr = common::PORT_BASE - 0x10;
            b.bench_local(|| {
                divan::black_box(reg.lookup_port(divan::black_box(addr)))
            });
        }
    }
}

// ── SysReg lookup scenarios ─────────────────────────────────────────

#[divan::bench_group]
mod sysreg {
    use super::*;

    per_version! {
        #[bench(args = [4usize, 8, 16, 32, 64, 128])]
        fn hit_mid(b: Bencher, n: usize) {
            let reg = <R>::new_with_devices(n);
            let addr = common::sysreg_addr(n / 2);
            b.bench_local(|| {
                divan::black_box(reg.lookup_sysreg(divan::black_box(addr)))
            });
        }
    }

    per_version! {
        #[bench(args = [4usize, 8, 16, 32, 64, 128])]
        fn miss(b: Bencher, n: usize) {
            let reg = <R>::new_with_devices(n);
            let addr = common::SYSREG_BASE - 0x10;
            b.bench_local(|| {
                divan::black_box(reg.lookup_sysreg(divan::black_box(addr)))
            });
        }
    }
}
