# Starry eBPF Programs

These programs are comprehensive eBPF test applications migrated from the original Starry `user/musl` directory. Since they are complex and require significant execution time, they are placed in `apps/starry/ebpf` rather than `test-suit` to serve as integration/application examples.

## Contents

- `ebpf-basics`: Basic eBPF instruction tests
- `ebpf-advanced`: Advanced eBPF instruction tests (ALU32, JMP32, etc.)
- `ebpf-attach`: eBPF attach and perf_event_open tests

## Build and Run

These programs can be built as part of the rootfs and run interactively or via an app script to comprehensively test the eBPF subsystem.
