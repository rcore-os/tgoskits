# arm-scmi-rs

[![crates.io](https://img.shields.io/crates/v/arm-scmi-rs.svg)](https://crates.io/crates/arm-scmi-rs)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

ARM SCMI (System Control and Management Interface) protocol implementation for `no_std` environments.

The crate provides both an agent-side SMC transport and a platform-side,
shared-memory dispatcher. The server copies and validates each request before
calling a backend, so the transport buffer does not remain locked across clock
or reset operations.

## Supported protocols

| Protocol | ID   | Operations |
|----------|------|------------|
| **Base** | 0x10 | discover vendor / sub-vendor, implementation version, list protocols |
| **Clock**| 0x14 | enable/disable, get/set rate, attributes, describe rates |
| **Reset**| 0x16 | domain attributes, assert/deassert, autonomous reset (server) |

The agent API currently supports Base and Clock. The server API supports the
mandatory Base discovery flow plus synchronous Clock v1.0 and Reset v1.0.

## Usage

Add to `Cargo.toml`:

```toml
[dependencies]
arm-scmi-rs = "0.1"
```

```rust
use arm_scmi_rs::{Scmi, Smc, Shmem};

// Create SMC transport
let smc = Smc::new(0x82000010, None);

// Map shared memory from device tree (platform-specific)
let shmem = unsafe { Shmem::new(addr, bus_addr, size) };

// Initialise SCMI agent
let scmi = Scmi::new(smc, shmem);

// Base protocol: discover platform
let mut base = scmi.protocol_base();
let vendor = base.discover_vendor()?;
let protocols = base.discover_list_protocols(0)?;

// Clock protocol: control clocks
let mut clk = scmi.protocol_clk();
clk.clk_enable(0)?;
clk.rate_set(0, 816_000_000)?;
let rate = clk.rate_get(0)?;

// Query individual clock attributes
let attrs = clk.clock_attributes(0)?;
println!("clock 0: enabled={}, name={:?}", attrs.enabled, attrs.name);
```

For a platform-side service, implement `ScmiServerBackend`, decode a copied
request with `ScmiServer::decode_request`, execute it after releasing the
shared-memory lock, and encode the response with
`ScmiServer::encode_response`. Backend resource IDs should already be filtered
to the resources owned by that SCMI agent.

## Architecture

```
                  ┌──────────┐
                  │   Scmi   │  top-level agent handle
                  └────┬─────┘
           ┌───────────┼───────────┐
           ▼           ▼           ▼
       ┌───────┐  ┌───────┐  ┌ ─ ─ ─ ─ ┐
       │ Base  │  │ Clock │  │  future   │  (Power, Sensor, …)
       │ 0x10  │  │ 0x14  │  │ protocols │
       └───┬───┘  └───┬───┘  └ ─ ─ ─ ─ ┘
           │          │
           ▼          ▼
       ┌──────────────────┐
       │    Protocol<T>   │  generic xfer / future-poll
       └────────┬─────────┘
                ▼
       ┌──────────────────┐
       │  Transport trait  │  (SMC, Mailbox, …)
       └────────┬─────────┘
                ▼
       ┌──────────────────┐
       │     Shmem         │  shared-memory window
       └──────────────────┘
```

The crate is transport-agnostic: implement the [`Transport`] trait to add
mailbox or other backends. The built-in [`Smc`] transport issues `smc #0`
with a configurable function ID.

## `no_std`

This crate is fully `no_std` and requires `alloc`.

## Testing

Tests run on real ARM hardware (e.g. OrangePi 5 Plus / RK3588) via
[`bare-test`](https://crates.io/crates/bare-test) and
[`ostool`](https://crates.io/crates/ostool):

```bash
cargo test --test test -- tests --show-output --uboot
```

## License

MIT
