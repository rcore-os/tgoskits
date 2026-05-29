# kbpf-basic

A `no_std` Rust eBPF foundation library for kernel or kernel-like environments.

It mainly provides three kinds of capabilities:

- Rust bindings for the Linux eBPF UAPI
- Unified abstractions for common BPF maps and helpers
- Basic support for program preprocessing, perf events, and raw tracepoints

This crate is better suited as a building block for an eBPF runtime or host-side implementation than as a standalone userspace loader.

## What This Crate Is For

`kbpf-basic` pulls together the parts of an eBPF runtime that are closely related to execution and are otherwise easy to reimplement repeatedly:

- `linux_bpf`: Linux BPF constants, enums, and struct bindings
- `map`: map metadata parsing, map creation, and common map operations
- `helper`: host-side entry points for common BPF helpers
- `prog`: program metadata and verifier log information parsing
- `perf`: perf event structures and buffer support
- `raw_tracepoint`: raw tracepoint attach argument parsing
- `EBPFPreProcessor`: map fd / map value relocation before program loading

## Current Capabilities

The current codebase implements or exposes the following:

- Supported map types
  - `ARRAY`
  - `PERCPU_ARRAY`
  - `PERF_EVENT_ARRAY`
  - `HASH`
  - `PERCPU_HASH`
  - `LRU_HASH`
  - `LRU_PERCPU_HASH`
  - `QUEUE`
  - `STACK`
  - `RINGBUF`
- Common map operations
  - `lookup`
  - `update`
  - `delete`
  - `get_next_key`
  - `lookup_and_delete`
  - `for_each`
  - queue/stack `push` / `pop` / `peek`
- Helper and runtime support
  - `bpf_trace_printk`-style output
  - `bpf_perf_event_output`
  - `bpf_probe_read`
  - `bpf_ktime_get_ns`
  - ring buffer helper
  - raw tracepoint argument parsing
  - perf event mmap/ring page support

## What The Host Must Implement

This crate does not perform all kernel-specific work by itself. The host environment is expected to implement two key traits:

- `KernelAuxiliaryOps`
  - Handles map fd/pointer resolution, user-kernel memory copy, time, output, page allocation, and virtual mapping
- `PerCpuVariantsOps`
  - Handles per-CPU data creation and CPU count discovery

If these traits are not properly wired into your host, many APIs will still compile but will fail at runtime.

## Basic Integration Flow

A typical integration flow looks like this:

1. Implement `KernelAuxiliaryOps` and `PerCpuVariantsOps` in your host environment.
2. Build map metadata from `BpfMapMeta` or `bpf_attr`.
3. Create a `UnifiedMap` with `bpf_map_create`.
4. Run `EBPFPreProcessor` before loading the program to relocate map references.
5. Initialize the helper dispatch table with `init_helper_functions`.
6. Parse program metadata, perf arguments, or raw tracepoint arguments as needed.

## Minimal Sketch

The following example only shows how the pieces fit together. It is not a complete runnable implementation:

```rust,ignore
use kbpf_basic::{
    EBPFPreProcessor, KernelAuxiliaryOps,
    map::{BpfMapMeta, PerCpuVariantsOps, bpf_map_create},
};

struct KernelOps;
struct PerCpuOps;

impl KernelAuxiliaryOps for KernelOps { /* host-side implementation */ }
impl PerCpuVariantsOps for PerCpuOps { /* per-cpu implementation */ }

fn setup(map_meta: BpfMapMeta, insns: Vec<u8>) {
    let _map = bpf_map_create::<KernelOps, PerCpuOps>(map_meta, None).unwrap();
    let _relocated = EBPFPreProcessor::preprocess::<KernelOps>(insns).unwrap();
}
```

## Development Notes

- This is a `no_std` crate.
- The code uses `#![feature(c_variadic)]`, so it currently requires nightly Rust.
- The repository passes a basic `cargo check`.

## Good Fit For

This crate is a good fit if you are:

- building an eBPF subsystem inside your own kernel
- integrating eBPF into a unikernel, exokernel, or teaching kernel
- implementing Linux-like eBPF runtime behavior in a non-Linux environment

If your goal is to load and manage eBPF programs directly from Linux userspace, you will usually still need a loader, verifier, object parsing, and attach flow around this crate. Covering the full userspace experience is not its goal.
