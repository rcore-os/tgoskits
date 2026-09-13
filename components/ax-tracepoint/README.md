# ax-tracepoint

A Rust tracepoint library for kernel scenarios, designed with goals similar to Linux tracepoints:

- Define events and fields with macros
- Manage tracepoints by unique event ID at runtime
- Support enable/disable, filter expressions, and callbacks
- Provide both raw event buffering and human-readable output
- no_std compatible

Repository: <https://github.com/rcore-os/tgoskits/tree/dev/components/ax-tracepoint>

## Core Capabilities

- Event definition: use `define_event_trace!` to generate event metadata, call functions, and register functions in one place
- Event management: `TracePointMap` indexed by tracepoint ID
- Event control: enable/disable, format/id/filter
- Filter expressions: compiled and evaluated against schema via `tp-lexer`
- Output pipeline: `TracePipeRaw` + `TraceEntryParser`

## Quick Start

### 1. Add dependencies

```toml
[dependencies]
ax-tracepoint = "*"
```

### 2. Keep the `.tracepoint` section in your linker script

This library scans event metadata through `__start_tracepoint / __stop_tracepoint`.
Merge the content of `my_section.ld` into your linker script and ensure the `.tracepoint` section is kept with `KEEP`.
StarryOS already carries this contract in its kernel linker scripts; other
hosts must add the equivalent section themselves.

### 3. Implement `KernelTraceOps`

You need to provide:

- `current_pid`
- `trace_pipe_push_raw_record`
- `trace_cmdline_push`
- tracepoint state registry hooks: `read_tracepoint_state`, `write_tracepoint_state`

The state registry hooks let your OS choose its own synchronization strategy for callbacks and filters.
Tracepoint calls use a runtime atomic callback gate, so changing registrations
never patches executable text while other CPUs are running.
The writer owns publication ordering: publish a non-empty callback state before
setting the gate, and clear the gate before retiring the last callback state.

### Callback restrictions

`read_tracepoint_state` may hold a read-side lock while the tracing fast path executes callbacks. If your implementation uses a non-reentrant lock such as `RwLock`, callbacks must not:

- register or unregister tracepoint callbacks
- update tracepoint filters
- call other APIs that require `write_tracepoint_state`
- recursively trigger tracepoints backed by the same state registry

Violating these rules may deadlock. Hosts that implement `read_tracepoint_state` with RCU, snapshots, or another non-blocking read-side mechanism may provide weaker restrictions.

Cooked event callbacks receive `&mut [u8]` for a freshly generated record.
Each callback owns a distinct record for the duration of the call, so BPF
adapters can update their context without casting away a shared reference.

### 4. Define and invoke events

```rust
use ax_tracepoint::{define_event_trace, KernelTraceOps};

define_event_trace!(
    TEST,
    TP_kops(Kops),
    TP_system(tracepoint_test),
    TP_PROTO(a: u32, b: u32),
    TP_STRUCT__entry { a: u32, b: u32 },
    TP_fast_assign { a: a, b: b },
    TP_ident(__entry),
    TP_printk(format_args!("a={}, b={}", __entry.a, __entry.b))
);

// Generated functions: trace_TEST / register_trace_TEST / unregister_trace_TEST
trace_TEST(1, 2);
```

Note: `TP_STRUCT__entry` participates in byte layout. Fields must implement
`TraceField`; the built-in implementations cover integer primitives and arrays
of trace fields.

### 5. Initialize the manager

```rust
use ax_tracepoint::global_init_events;

let (tracepoints, ext_tracepoints) = global_init_events::<Kops>()?;

// Install `ext_tracepoints` into the registry used by
// Kops::read_tracepoint_state and Kops::write_tracepoint_state.
```

### 6. Enable, filter, and consume output

```rust
use ax_tracepoint::{TraceFilterFile, TracePointEnableFile, TracePointFormatFile, TracePointIdFile};

let event_id = 0;
Kops::write_tracepoint_state(event_id, |event| {
    TracePointEnableFile::new().write(event, '1');
    let mut filter = TraceFilterFile::new();
    filter.write(event, "a > 8 && b > 5").unwrap();
});

// Read format and ID
let tracepoint = tracepoints.get(&event_id).unwrap();
let fmt = TracePointFormatFile::new(tracepoint).read();
let id = TracePointIdFile::new(tracepoint).read();
```

## Run the example

```bash
cargo run -p ax-tracepoint --example usage
```

Example code is in `examples/usage.rs`, covering:

- Event definition and triggering
- Event enabling and filtering
- Registering event/raw callbacks
- Reading `TracePipeRaw` snapshots and parsing them into text

## Main Public Types

- `KernelTraceOps`
- `TracePoint` / `ExtTracePoint` / `TracePointMap`
- `TracePipeRaw` / `TracePipeSnapshot` / `TracePipeOps`
- `TraceCmdLineCache` / `TraceEntryParser`
- `TraceFilterError` / `TraceParseError` / `TraceInitError`

Malformed trace records and rejected filter updates return typed errors. A
failed filter compilation preserves the previously active compiled filter;
write exactly `0` (surrounding whitespace is allowed) to clear it.

## Reference Projects

- DragonOS: <https://github.com/DragonOS-Community/DragonOS/blob/master/kernel/src/debug/tracing/mod.rs>
- TGOSKits StarryOS: <https://github.com/rcore-os/tgoskits/tree/dev/os/StarryOS>
