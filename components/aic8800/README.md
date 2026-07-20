# aic8800

OS-independent AIC8800-family SDIO Wi-Fi driver core. The crate owns hardware
and protocol state only. It does not create tasks, register OS IRQ actions,
sleep, yield, store wakers, or install a process-global runtime.

## Ownership model

Discovery assembles one move-only device owner containing:

- the controller, protocol, TX, and RX state;
- a destructive IRQ endpoint, used only to capture and acknowledge stable
  hardware events;
- a generation-checked IRQ source-control endpoint.

The OS moves the complete owner into one CPU-pinned maintenance thread. That
thread transfers the IRQ endpoint into its local IRQ action and is the only
context that calls the SDIO transaction engine, controller state machine, TX
queue, or RX queue. The hard handler converts the host's callback-free
`HostEventSummary` into a stable network event and wakes that owner; board glue
does not inject a mapper or scheduling callback.

Initialization is an explicit bounded state machine:

```text
Discovered
  -> ControllerInit
  -> FirmwareLoad
  -> FirmwareBoot
  -> Configure
  -> StartLink
  -> Ready | Failed
```

Each pending result names an IRQ source, an absolute deadline, or an immediate
bounded transition. There is no periodic completion polling fallback.

## Data and control planes

TX and RX advertise `QueueMemoryMode::OwnerCopy`. The network runtime therefore
keeps packet buffers in CPU ownership; the maintenance owner copies TX bytes
into its owned SDIO transfer and copies decoded RX bytes back into a submitted
runtime buffer. This is required for non-coherent systems and avoids pretending
that a PIO/copy device directly owns the runtime DMA buffer.

The current control state machine supports an open SoftAP. Board glue supplies
the initial `SoftApPolicy`; subsequent changes use the generic
`rdif_eth::WifiCommand` owner mailbox. Station association is retained as an
explicit unsupported command until its LMAC transitions and WPA2 key-install
sequence are moved into the same bounded owner state machine. The pure WPA2
cryptographic implementation remains available in `aic8800::crypto`.

## Integration sketch

```rust,ignore
let config = AicDiscoveryConfig::new(mac, Some(link_policy))
    .with_chip(ChipVariant::Aic8800DC)
    .with_soft_ap(SoftApPolicy::try_new(b"PicoClaw-Car", 6)?);

// Side-effect-free discovery: no IRQ action and no worker is created here.
let device = AicWifiNetDev::discover(host, device_dma, config)?;

// OS glue registers `device` with its CPU-pinned network maintenance owner.
register_network_device(device, resolved_irq_binding);
```

## Source layout

```text
src/
|- common/       chip identifiers, SDIO registers, and CRC
|- crypto/       pure WPA2-PSK primitives and handshake state
|- data.rs       pure Ethernet/firmware packet conversion
|- firmware.rs   digest-verified blobs and the generic debug request machine
|- firmware/     bounded DC syscfg, calibration, RF, and patch-table sequence
|- owner.rs      bounded controller/firmware/link/data state machines
|- softap.rs     pure SoftAP LMAC request builders
|- transport.rs  one-outstanding-request owned SDIO engine
`- wire.rs       pure LMAC framing and exact confirmation parsing
```

## Firmware

Vendor firmware blobs are not committed or packaged. `build.rs` resolves every
blob from `$AIC8800_FIRMWARE_DIR`, the in-tree cache populated by `cargo xtask`,
or a pinned upstream commit, then verifies its SHA-256 before copying it into
`OUT_DIR/firmware`. See [`build.rs`](build.rs) and
[`firmware/README.md`](firmware/README.md) for the pinned manifest.

The AIC8800DC path does not treat firmware as one opaque upload. Its owner FSM
first reads the chip and sub-revision, applies the matching syscfg and masked
PMIC writes, uploads the normal or H patch, performs the required misc-RAM/DPD
calibration path, installs LDPC/AGC/TX-gain data and the code patch table, and
only then starts the FMAC entry. Every debug request requires its exact CFM;
memory confirmations are also checked against the requested address.
