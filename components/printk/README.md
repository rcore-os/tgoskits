<h1 align="center">ax-printk</h1>

<p align="center">Lockless multi-writer record ring buffer for kernel logs</p>

<div align="center">

[![Crates.io](https://img.shields.io/crates/v/ax-printk.svg)](https://crates.io/crates/ax-printk)
[![Docs.rs](https://docs.rs/ax-printk/badge.svg)](https://docs.rs/ax-printk)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](./LICENSE)

</div>

English | [中文](README_CN.md)

# Introduction

`ax-printk` is a lockless, multi-writer ring buffer for variable-length log
records. It is maintained as part of the TGOSKits component set and is intended
for Rust projects that integrate with ArceOS, AxVisor, or related low-level
systems software.

The design uses two rings:

- a **descriptor ring** of fixed-size records, each a `state_var` state machine
  plus metadata (sequence number, timestamp, priority, and the data-block
  location);
- a **text data ring** holding the variable-length messages, written zero-copy.

There are no locks: writers reserve a record by advancing the ring heads with a
CAS and publish it through the descriptor state machine; readers stay consistent
by re-reading `state_var` after copying. This keeps logging usable from IRQ, NMI,
and panic context, where a lock could deadlock.

Rings carry their geometry (`count_bits` / `size_bits`) per instance, so the
same code serves buffers of any size, and multiple independent instances may
coexist. A static instance can be defined for use before an allocator is
available; a runtime instance can be built over caller-provided buffers.

> **Note:** this crate uses `unsafe` and lockless memory ordering. It MUST be
> validated under SMP stress before being relied upon.

# License

Licensed under the [Apache-2.0](./LICENSE) license.
