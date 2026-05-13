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

The crate is currently early-stage. The SPI path has protocol-level unit tests
and basic block read/write support. The SDIO path is the integration boundary
used by host crates such as `sdhci-host` and `dwmmc-host`, and needs
platform-specific validation before use on hardware.

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
use embedded_hal::delay::DelayNs;
use sdmmc_protocol::{Command, Error, Response};
use sdmmc_protocol::sdio::{BusWidth, ClockSpeed, SdioHost, SdioSdmmc};

struct MySdioHost;

impl SdioHost for MySdioHost {
    fn send_command(&mut self, cmd: &Command) -> Result<Response, Error> {
        let _ = cmd;
        todo!()
    }

    fn read_data(&mut self, buf: &mut [u8], block_size: u32) -> Result<(), Error> {
        let _ = (buf, block_size);
        todo!()
    }

    fn write_data(&mut self, buf: &[u8], block_size: u32) -> Result<(), Error> {
        let _ = (buf, block_size);
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
}

fn example<D: DelayNs>(host: MySdioHost, delay: D) -> Result<(), Error> {
    let mut card = SdioSdmmc::new(host, delay);
    let info = card.init()?;
    let _rca = info.rca;
    let _capacity_blocks = info.capacity_blocks;
    Ok(())
}
```

`SdioSdmmc` detects SD versus eMMC during initialization. SD cards are widened
through ACMD6; eMMC bus widening is intentionally left to host/platform code
until the CMD6 SWITCH plus EXT_CSD flow is wired into the protocol layer.

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

## License

Licensed under the Apache License, Version 2.0. See [LICENSE](LICENSE).
