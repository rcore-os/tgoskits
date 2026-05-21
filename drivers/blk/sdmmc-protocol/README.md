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
- A SPI-mode SD card driver over a small transport trait
- A SDIO/native-mode host-controller abstraction and driver skeleton
- One shared `Error` type with command/phase context for protocol and host errors

The SPI path has protocol-level unit tests and basic block read/write support.
The SDIO path is the integration boundary used by the host crates in this
workspace and has been validated end-to-end on the controller / SoC
combinations listed under [Validated host backends](#validated-host-backends).

## Features

```toml
[features]
default = ["spi"]
spi = []
sdio = []
```

- `spi`: enables the SPI transport and `SpiSdmmc` driver.
- `sdio`: enables the SDIO host abstraction and `SdioSdmmc` driver.

Diagnostics use the `log` crate. Configure a logger in the caller if runtime
messages are needed.

## SPI Mode

The SPI path is built around `SpiTransport` plus an `embedded_hal::delay::DelayNs`
implementation that the driver uses for wall-clock timeouts:

```rust
use embedded_hal::delay::DelayNs;
use sdmmc_protocol::Error;
use sdmmc_protocol::spi::{SpiSdmmc, SpiTransport};

struct MySpi;

impl SpiTransport for MySpi {
    fn transfer_byte(&mut self, byte: u8) -> Result<u8, Error> {
        // Send one byte on your platform SPI peripheral and return the byte read.
        // Chip-select handling depends on your board/HAL design.
        let _ = byte;
        todo!()
    }
}

fn example<D: DelayNs>(spi: MySpi, delay: D) -> Result<(), Error> {
    let mut card = SpiSdmmc::new(spi, delay);
    let info = card.init()?;

    let mut block = [0u8; 512];
    card.read_block(0, &mut block)?;

    let _is_sdhc_or_sdxc = info.high_capacity;
    let _capacity_blocks = info.capacity_blocks; // Some(blocks) for known CSD versions
    Ok(())
}
```

If your platform already exposes an `embedded-hal` 1.0 `SpiDevice<u8>`, wrap it with `SpiDeviceWrapper`:

```rust
use embedded_hal::delay::DelayNs;
use sdmmc_protocol::spi::{SpiDeviceWrapper, SpiSdmmc};

fn create_driver<SPI, D>(spi: SPI, delay: D) -> SpiSdmmc<SpiDeviceWrapper<SPI>, D>
where
    SPI: embedded_hal::spi::SpiDevice<u8>,
    D: DelayNs,
{
    SpiSdmmc::new(SpiDeviceWrapper::new(spi), delay)
}
```

### SPI Operations

`SpiSdmmc` currently exposes:

- `init()`
- `read_block(addr, &mut [u8; 512])`
- `write_block(addr, &[u8; 512])`
- `read_blocks(addr, count, handler)`
- `write_blocks(addr, blocks)`
- `switch_function(cmd)`
- `switch_to_high_speed()`

For SDHC/SDXC cards, block addresses are passed through directly. For SDSC cards, block addresses are converted to byte addresses internally.
CRC16 verification for read data is enabled by default and can be changed with
`set_verify_data_crc`.

## SDIO Mode

The SDIO path expects the platform to implement `SdioHost`. The driver tracks
the published RCA itself, so hosts no longer need to snoop R6 responses:

```rust
use sdmmc_protocol::{Command, CommandResponsePoll, DataCommandPoll, Error, OperationPoll, Response};
use core::task::Waker;
use sdmmc_protocol::sdio::{BusWidth, ClockSpeed, SdioHost, SdioInitScratch, SdioSdmmc};

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
    let info = loop {
        match card.poll_init_request(&mut request)? {
            OperationPoll::Pending => {
                // Runtime policy belongs here: spin, yield, wait for IRQ, or
                // sleep/timer when request.take_needs_pace() is set.
            }
            OperationPoll::Complete(info) => break info,
        }
    };
    let _rca = info.rca;
    let _capacity_blocks = info.capacity_blocks;
    Ok(())
}
```

`SdioSdmmc` detects SD versus eMMC during initialization. SD cards are widened
through ACMD6; eMMC cards use EXT_CSD plus CMD6 SWITCH to negotiate bus width
and timing where the host supports those modes.

`SdioHost` follows a submit/poll model. Protocol operations such as card
initialization, command status, EXT_CSD reads, MMC switches, switch-function
reads, and block I/O expose request objects that
callers can poll from a blocking loop, an IRQ wakeup path, a worker, or an async
runtime wrapper. `SdioSdmmc` does not choose the waiting policy; the caller owns
whether pending work spins, yields, sleeps, waits for an IRQ, or uses a timer.

### Optional wall-clock timeouts

ACMD41 / CMD1 power-up and MMC `CMD6 SWITCH` busy-waits default to a poll
counter that assumes the caller paces `poll_*` at ~10 ms. Hosts that can
expose a monotonic clock should override `SdioHost::now_ms() -> Option<u64>`:
the protocol layer then enforces wall-clock deadlines (1 s for power-up,
250 ms for CMD6) in addition to the poll budget, so timeouts stay accurate
no matter how fast or slow the caller polls. Hosts that return `None` (the
default) keep the pure poll-counter behavior.

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

Run the default SPI-enabled test suite:

```bash
cargo test
```

Run SDIO-only compilation and tests:

```bash
cargo test --no-default-features --features sdio
```

Run all feature combinations used during development:

```bash
cargo fmt --check
cargo test
cargo test --no-default-features --features sdio
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
- SPI mode targets SD cards; MMC-over-SPI is not a current target.
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
