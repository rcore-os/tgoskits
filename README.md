# cpumask

[![Crates.io](https://img.shields.io/crates/v/cpumask)](https://crates.io/crates/cpumask)
[![Docs.rs](https://docs.rs/cpumask/badge.svg)](https://docs.rs/cpumask)
[![CI](https://github.com/arceos-org/cpumask/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/arceos-org/cpumask/actions/workflows/ci.yml)

CPU mask library

Cpumasks provide a bitmap suitable for representing the set of CPUs in a system, one bit position per CPU number.
In general, only nr_cpu_ids (<= NR_CPUS) bits are valid.
Refering to `cpumask_t` in Linux.
Reference:

* <https://man7.org/linux/man-pages/man2/sched_setaffinity.2.html>    
* <https://man7.org/linux/man-pages/man3/CPU_SET.3.html>
* <https://elixir.bootlin.com/linux/v6.11/source/include/linux/cpumask_types.h>

## Examples

```Rust
use cpumask::CpuMask;
const SMP: usize = 32;

let mut cpumask = CpuMask::<SMP>::new();

assert!(cpumask.is_empty());
cpumask.set(0, true);

assert!(!cpumask.is_empty());
assert!(cpumask.get(0));
assert_eq!(cpumask.len(), 1);

assert!(!cpumask.set(1, true));
assert_eq!(cpumask.len(), 2);
assert_eq!(cpumask.first_false_index(), Some(2));

let mut oneshot = CpuMask::<SMP>::one_shot(SMP - 1);
assert!(!oneshot.is_empty());
assert!(oneshot.get(SMP - 1));
assert_eq!(oneshot.first_index(), Some(SMP - 1));
assert_eq!(oneshot.len(), 1);

oneshot.set(0, false);
assert!(!oneshot.is_empty());
oneshot.set(SMP - 1, false);
assert!(oneshot.is_empty());
assert_eq!(oneshot.len(), 0);
assert_eq!(oneshot.first_index(), None);

let mut cpumask_full = CpuMask::<SMP>::full();
assert_eq!(cpumask_full.len(), SMP);
assert_eq!(cpumask_full.first_index(), Some(0));
assert_eq!(cpumask_full.first_false_index(), None);
cpumask_full.set(SMP-1, false);
assert_eq!(cpumask_full.len(), SMP - 1);
assert_eq!(cpumask_full.first_false_index(), Some(SMP-1));
```