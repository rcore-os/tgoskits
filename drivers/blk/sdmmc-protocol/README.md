# sdmmc-protocol

`sdmmc-protocol` provides `no_std` SD/MMC protocol building blocks for
embedded systems and kernel drivers.

The crate owns protocol-level command construction, response parsing, card
initialization flow, and block I/O sequencing. It does not own board setup,
MMIO mapping, IRQ routing, DMA allocation, or controller clock-tree setup; keep
those in the host-controller crate or OS/platform glue.

It provides:

- SD/MMC command definitions and SPI command packet encoding
- Response types and parsers for common SD, MMC, and SDIO responses
- EXT_CSD helpers for eMMC capacity, bus-width, and timing capability fields
- A SDIO/native-mode host-controller abstraction and driver skeleton
- An optional RDIF block-device bridge for SDIO-backed host crates
- One shared `Error` type with command/phase context for protocol and host errors

The SDIO path is the integration boundary used by the host crates in this
workspace and has been validated end-to-end on the controller / SoC
combinations listed under [Validated host backends](#validated-host-backends).

## Features

```toml
[features]
default = []
sdio = []
rdif = ["sdio", "dep:rdif-block"]
```

- `sdio`: enables the SDIO host abstraction and `SdioSdmmc` driver.
- `rdif`: enables the RDIF block-device adapter for SDIO-backed host crates.

Diagnostics use the `log` crate. Configure a logger in the caller if runtime
messages are needed.

## SDIO Mode

The SDIO path expects the platform to implement `SdioHost`. The driver tracks
the published RCA itself, so hosts no longer need to snoop R6 responses:

```rust
use sdmmc_protocol::{Command, CommandResponsePoll, DataCommandPoll, Error, OperationPoll, Response};
use core::task::Waker;
use sdmmc_protocol::sdio::{
    card::SdioSdmmc,
    host::{BusWidth, ClockSpeed, SdioHost},
    init::SdioInitScratch,
    InitInput, InitPoll, InitIrqWait,
};

struct MySdioHost;
struct MyDataRequest<'a>(&'a mut [u8]);

impl SdioHost for MySdioHost {
    type Event = ();
    type DataRequest<'a> = MyDataRequest<'a>;

    fn submit_command(&mut self, cmd: &Command) -> Result<(), Error> {
        let _ = cmd;
        todo!()
    }

    fn poll_command_response(&mut self) -> Result<CommandResponsePoll, Error> {
        todo!()
    }

    fn submit_read_data<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a mut [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Self::DataRequest<'a>, Error> {
        let _ = (cmd, block_size, block_count);
        Ok(MyDataRequest(buf))
    }

    fn submit_write_data<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Self::DataRequest<'a>, Error> {
        let _ = (cmd, buf, block_size, block_count);
        todo!()
    }

    fn poll_data_request<'a>(
        &mut self,
        request: &mut Self::DataRequest<'a>,
    ) -> Result<DataCommandPoll, Error> {
        let _ = request;
        todo!()
    }

    fn set_bus_width(&mut self, width: BusWidth) -> Result<(), Error> {
        let _ = width;
        todo!()
    }

    fn set_clock(&mut self, speed: ClockSpeed) -> Result<(), Error> {
        let _ = speed;
        todo!()
    }

    fn register_waker(&mut self, waker: &Waker) {
        let _ = waker;
        // Optional: IRQ-driven hosts store this waker and wake it from
        // their OS glue after handle_irq() reports command/data progress.
    }
}

fn example(host: MySdioHost) -> Result<(), Error> {
    let mut card = SdioSdmmc::new(host);
    let mut scratch = SdioInitScratch::new();
    let mut request = card.submit_init(&mut scratch)?;
    let progress = card.poll_init_request(&mut request, InitInput::at(0));
    let InitPoll::Pending(schedule) = progress else { unreachable!() };
    // Invoke again only for `run_again`, after acknowledging the requested
    // controller IRQ, or at the absolute `wake_at_ns` deadline.
    let _waits_for_irq = matches!(schedule.irq, InitIrqWait::Controller);
    Ok(())
}
```

`SdioSdmmc` detects SD versus eMMC during initialization. SD cards are widened
through ACMD6; eMMC cards use EXT_CSD plus CMD6 SWITCH to negotiate bus width
and timing where the host supports those modes.

`SdioHost` follows a submit/event model. Protocol operations such as card
initialization, command status, EXT_CSD reads, MMC switches, switch-function
reads, and block I/O expose request objects that
callers advance from an IRQ worker or an explicitly scheduled initialization
deadline. Card initialization consumes `InitInput { now_ns, irq }`; repeated
calls before its returned activation neither consume retries nor inspect
completion state. Command/data deadlines fail closed when their IRQ is lost.
Hosts with eventless platform sequencing implement `poll_bus_op_at` and return
the current operation's absolute activation from `bus_op_wake_at`; a physical
host2 adapter opts into that contract with `SdioSdmmc::new_host2_timed`.

Long-lived OS discovery code should use `OwnedSdioInit`, which pins scratch
storage and centralizes the host-request lifetime contract. With the `rdif`
feature, `StagedBlockDevice` implements `InitialController`: it publishes the
initialization IRQ source before issuing the first command, updates
`BlockConfig.capacity_blocks` only after `CardInfo` is ready, and then invokes
the host crate's typed lifecycle builder. Normal block queues remain IRQ-only.

### Monotonic initialization time

ACMD41 / CMD1 power-up, MMC `CMD6 SWITCH`, and eventless host operations use
only the caller-provided `InitInput::now_ns`. Retry and timeout behavior is
therefore independent of call frequency. The protocol does not obtain a
global clock, sleep, or translate a number of polls into elapsed time. The
legacy `SdioHost::now_ms()` capability remains source-compatible for hosts
outside initialization, but card initialization does not consume it.

### SDIO module boundaries

The `sdio` feature is split by capability:

- `sdio::host`: host-controller capabilities, IRQ events, and bus operations.
- `sdio::host2`: compatibility adapter for `sdio-host2` physical hosts,
  including request ownership and DMA recovery.
- `sdio::card`: `SdioSdmmc`, card information, and ordinary command/block I/O
  request wrappers.
- `sdio::init`: initialization scratch storage, probe preference, and the
  explicitly scheduled initialization state machine.

The historical `sdmmc_protocol::sdio::*` re-exports remain available for
callers that have not migrated to the capability submodules yet.

## RDIF Block Bridge

The `rdif` feature adapts an initialized `SdioSdmmc` card to `rdif-block`
without pulling OS runtime policy into the protocol crate. Its public modules
match the ownership boundary:

- `rdif::config`: block size constants, `BlockConfig`, queue limits, device
  info, card-address translation, and typed error mapping. `BlockDataPath`
  explicitly distinguishes initialization-only FIFO access, DMA, and owned
  interrupt-driven PIO; activation never silently falls back between them.
- `rdif::host`: the `BlockHost` capability boundary plus the `SdioHost2Adapter`
  request-slot adapter.
- `rdif::device`: `BlockDevice` and `rdif_block::Interface` integration.
- `rdif::queue`: the owned hardware queue. It transfers each CPU buffer into
  either a prepared DMA request or an owned PIO request, advances only from
  acknowledged IRQ event batches, and returns the exact buffer by terminal
  completion or proof-gated shutdown/recovery.
- `rdif::device` also exposes the typed interrupt lifecycle used for bounded
  controller quiescence and reconstruction. Hosts must opt in explicitly;
  missing lifecycle support fails closed rather than fabricating DMA safety.
- `rdif::irq`: the top-half IRQ bridge, which consumes a host IRQ endpoint and
  never enters the shared card core.
- `rdif::shared_core`: the task-context borrow gate shared by device control
  and queues. Acquisition is one-shot and non-blocking: submission contention
  returns the owned request with `Retry`, event service returns `More` while
  retaining the same IRQ snapshot, and lifecycle polling schedules another
  bounded pass. The gate never sleeps or spins.

The `sdmmc_protocol::rdif::*` re-exports contain the same owned/event-driven
contract; borrowed queues and completion polling are intentionally absent.

## Command Helpers

The `cmd` module contains helpers for common commands:

- `CMD0`, `CMD2`, `CMD3_SD`, `CMD12`, `CMD38`, `CMD58`
- `cmd8(voltage, check_pattern)`
- `cmd17(addr)`, `cmd18(addr)`
- `cmd24(addr)`, `cmd25(addr)`
- `cmd55(rca)`, `cmd41(hcs, voltage_window)`
- SDIO helpers such as `cmd52(...)` and `cmd53(...)`

Commands can be encoded for SPI with:

```rust
let bytes = sdmmc_protocol::cmd::CMD0.to_spi_bytes();
assert_eq!(bytes, [0x40, 0x00, 0x00, 0x00, 0x00, 0x95]);
```

## Testing

Run the protocol-only default test suite:

```bash
cargo test
```

Run SDIO-only compilation and tests:

```bash
cargo test --no-default-features --features sdio
```

Run the SDIO plus RDIF block-bridge tests:

```bash
cargo test --no-default-features --features sdio,rdif
```

Run all feature combinations used during development:

```bash
cargo fmt --check
cargo test
cargo test --no-default-features --features sdio
cargo test --no-default-features --features sdio,rdif
cargo test --all-features
```

In this workspace, prefer the project `xtask` flow for final validation when
the crate is part of a larger change:

```bash
cargo fmt
cargo xtask clippy --package sdmmc-protocol
```

## Current Limitations

- No real hardware examples are included yet.
- SDIO/native mode has card init and block I/O plumbing, but advanced eMMC
  mode switching is still incomplete.
- UHS-I and HS200 entry depends on host support for voltage switching and
  tuning. Unsupported host operations return `Error::UnsupportedCommand`.

## Validated host backends

The protocol layer has been exercised on the controller / SoC combinations
below through dedicated host crates in this workspace. Modes not listed are
either unimplemented in the host backend or have not yet been signed off on
real hardware.

| Host crate         | SoC / controller       | Mode                  | Notes                                       |
|--------------------|------------------------|-----------------------|---------------------------------------------|
| `sdhci-host`       | RK3568 (dwcmshc)       | eMMC HS@52, FIFO/DMA  | HS200 path exists; not yet signed off       |
| `sdhci-host`       | RK3588 (dwcmshc)       | eMMC HS@52, FIFO/DMA  | HS200 path exists; not yet signed off       |
| `dwmmc-host`       | RK3568 SD (dw_mshc)    | SD HS, DMA            |                                             |
| `phytium-mci-host` | Phytium MCI            | SD HS, DMA            |                                             |

See `drivers/blk/sdmmc-protocol/docs/REVIEW.md` for the remaining 1.0 roadmap
(non_exhaustive enums, `Display` impls, time-base contract, fuzz coverage,
SDIO IO-card support, etc.).

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
