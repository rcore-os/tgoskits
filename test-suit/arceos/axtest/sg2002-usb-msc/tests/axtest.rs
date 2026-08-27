#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

extern crate alloc;

#[cfg(feature = "ax-std")]
extern crate ax_std as std;

use arceos_axtest_sg2002_usb_msc::{
    BenchConfig, WriteBenchConfig, bench_iterations, blocks_per_transfer, build_read10_command,
    build_write10_command, compile_time_write_bench_config, mib_per_sec_x100,
};

#[cfg(all(feature = "ax-std", axtest))]
mod sg2002_usb_msc {
    use alloc::{string::String, vec};
    use core::{fmt, time::Duration};

    use ax_driver::usb::PlatformUsbHost;
    use ax_hal::time::monotonic_time_nanos;
    use ax_runtime::{
        hal::irq::{IrqError, IrqId, IrqReturn},
        irq::{Registration, resolve_binding_irq},
    };
    use crab_usb::{
        DeviceInfo, EndpointHandle, Event,
        err::{TransferError, USBError},
        usb_if::{descriptor::EndpointType, endpoint::TransferRequest, transfer::Direction},
    };
    use rdrive::{DeviceGuard, DriverGeneric, GetDeviceError};

    use super::{
        BenchConfig, WriteBenchConfig, bench_iterations, blocks_per_transfer, build_read10_command,
        build_write10_command, compile_time_write_bench_config, mib_per_sec_x100,
    };

    const DRIVER_NAME: &str = "usb-sg2002-dwc2";
    const MSC_CLASS: u8 = 0x08;
    const MSC_SUBCLASS_SCSI: u8 = 0x06;
    const MSC_PROTOCOL_BULK_ONLY: u8 = 0x50;
    const CBW_SIGNATURE: u32 = 0x4342_5355;
    const CSW_SIGNATURE: u32 = 0x5342_5355;
    const CBW_LEN: usize = 31;
    const CSW_LEN: usize = 13;
    const MAX_BLOCK_SIZE: u32 = 4096;

    #[derive(Debug)]
    pub struct MscSmokeReport {
        pub vendor_id: u16,
        pub product_id: u16,
        pub inquiry_vendor: String,
        pub inquiry_product: String,
        pub blocks: u64,
        pub block_size: u32,
        pub read_checksum: u32,
    }

    #[derive(Debug)]
    pub enum SmokeError {
        NoHost,
        HostLock(GetDeviceError),
        Usb(USBError),
        Transfer(TransferError),
        MissingIrqBinding,
        IrqResolve(IrqError),
        IrqRegister(IrqError),
        NoMassStorageDevice,
        MissingBulkEndpoint,
        ShortTransfer {
            stage: &'static str,
            expected: usize,
            actual: usize,
        },
        InvalidCswSignature(u32),
        CswTagMismatch {
            expected: u32,
            actual: u32,
        },
        BotCommandFailed {
            opcode: u8,
            status: u8,
            residue: u32,
        },
        InvalidCapacity {
            blocks: u64,
            block_size: u32,
        },
        InvalidWriteBench {
            start_lba: u32,
            blocks: u16,
            capacity_blocks: u64,
        },
        WriteVerifyMismatch {
            expected: u32,
            actual: u32,
        },
        TransferBusyWait {
            iters: usize,
        },
    }

    impl fmt::Display for SmokeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::NoHost => write!(f, "SG2002 DWC2 host not found"),
                Self::HostLock(err) => write!(f, "host lock failed: {err:?}"),
                Self::Usb(err) => write!(f, "USB error: {err:?}"),
                Self::Transfer(err) => write!(f, "transfer error: {err:?}"),
                Self::MissingIrqBinding => write!(f, "SG2002 DWC2 host has no binding IRQ"),
                Self::IrqResolve(err) => write!(f, "failed to resolve USB binding IRQ: {err:?}"),
                Self::IrqRegister(err) => write!(f, "failed to register USB IRQ: {err:?}"),
                Self::NoMassStorageDevice => write!(f, "USB mass-storage device not found"),
                Self::MissingBulkEndpoint => write!(f, "mass-storage bulk endpoint missing"),
                Self::ShortTransfer {
                    stage,
                    expected,
                    actual,
                } => write!(
                    f,
                    "{stage} short transfer: expected {expected}, actual {actual}"
                ),
                Self::InvalidCswSignature(signature) => {
                    write!(f, "invalid CSW signature {signature:#010x}")
                }
                Self::CswTagMismatch { expected, actual } => write!(
                    f,
                    "CSW tag mismatch: expected {expected:#010x}, actual {actual:#010x}"
                ),
                Self::BotCommandFailed {
                    opcode,
                    status,
                    residue,
                } => write!(
                    f,
                    "BOT command {opcode:#04x} failed: status={status} residue={residue}"
                ),
                Self::InvalidCapacity { blocks, block_size } => {
                    write!(
                        f,
                        "invalid capacity: blocks={blocks} block_size={block_size}"
                    )
                }
                Self::InvalidWriteBench {
                    start_lba,
                    blocks,
                    capacity_blocks,
                } => write!(
                    f,
                    "invalid write bench range: lba={start_lba} blocks={blocks} \
                     capacity_blocks={capacity_blocks}"
                ),
                Self::WriteVerifyMismatch { expected, actual } => write!(
                    f,
                    "write bench readback checksum mismatch: expected=0x{expected:08x} \
                     actual=0x{actual:08x}"
                ),
                Self::TransferBusyWait { iters } => {
                    write!(f, "DWC2 transfer path still busy-waited: iters={iters}")
                }
            }
        }
    }

    impl From<USBError> for SmokeError {
        fn from(value: USBError) -> Self {
            Self::Usb(value)
        }
    }

    impl From<TransferError> for SmokeError {
        fn from(value: TransferError) -> Self {
            Self::Transfer(value)
        }
    }

    impl From<GetDeviceError> for SmokeError {
        fn from(value: GetDeviceError) -> Self {
            Self::HostLock(value)
        }
    }

    #[derive(Clone, Copy)]
    struct MscInterface {
        interface_number: u8,
        alternate_setting: u8,
        bulk_in: u8,
        bulk_out: u8,
    }

    struct BotTag(u32);

    impl BotTag {
        fn next(&mut self) -> u32 {
            let tag = self.0;
            self.0 = self.0.wrapping_add(1).max(1);
            tag
        }
    }

    pub async fn run() -> Result<MscSmokeReport, SmokeError> {
        let devices = rdrive::get_list::<PlatformUsbHost>();
        for device in devices {
            let mut host = device.lock()?;
            if host.name() != DRIVER_NAME {
                continue;
            }
            axtest::axtest_println!("SG2002_DWC2_HOST_FOUND name={}", host.name());
            return run_on_host(&mut host).await;
        }
        Err(SmokeError::NoHost)
    }

    async fn run_on_host(
        host: &mut DeviceGuard<PlatformUsbHost>,
    ) -> Result<MscSmokeReport, SmokeError> {
        let _irq = install_usb_irq(host)?;
        axtest::axtest_println!("SG2002_DWC2_INIT_START");
        host.host_mut().init().await?;
        host.enable_irq()?;
        axtest::axtest_println!("SG2002_DWC2_INIT_DONE");
        axtest::axtest_println!("SG2002_DWC2_PROBE_START");
        let devices = host.host_mut().probe_devices().await?;
        axtest::axtest_println!("SG2002_DWC2_PROBE_DONE devices={}", devices.connected.len());
        for probed in devices.connected {
            let Some(info) = probed.as_device_info() else {
                continue;
            };
            let Some(msc) = find_msc_interface(info) else {
                continue;
            };
            let vendor_id = probed.vendor_id();
            let product_id = probed.product_id();
            axtest::axtest_println!(
                "SG2002_DWC2_MSC_FOUND vid={vendor_id:04x} pid={product_id:04x} interface={} \
                 alt={} bulk_in=0x{:02x} bulk_out=0x{:02x}",
                msc.interface_number,
                msc.alternate_setting,
                msc.bulk_in,
                msc.bulk_out
            );

            let Some(info) = probed.into_device_info() else {
                continue;
            };
            return run_on_device(host, info, msc, vendor_id, product_id).await;
        }
        Err(SmokeError::NoMassStorageDevice)
    }

    async fn run_on_device(
        host: &mut DeviceGuard<PlatformUsbHost>,
        info: DeviceInfo,
        msc: MscInterface,
        vendor_id: u16,
        product_id: u16,
    ) -> Result<MscSmokeReport, SmokeError> {
        let mut device = host.host_mut().open_device(&info).await?;
        let interface_session = device
            .claim_interface(msc.interface_number, msc.alternate_setting)
            .await?;
        let mut bulk_in = interface_session
            .endpoint(msc.bulk_in)
            .map_err(|_| SmokeError::MissingBulkEndpoint)?;
        let mut bulk_out = interface_session
            .endpoint(msc.bulk_out)
            .map_err(|_| SmokeError::MissingBulkEndpoint)?;
        let mut tag = BotTag(1);

        let mut inquiry = [0u8; 36];
        bot_inquiry(&mut bulk_out, &mut bulk_in, &mut tag, &mut inquiry).await?;
        let inquiry_vendor = ascii_field(&inquiry[8..16]);
        let inquiry_product = ascii_field(&inquiry[16..32]);
        axtest::axtest_println!(
            "SG2002_DWC2_MSC_INQUIRY vendor=\"{}\" product=\"{}\"",
            inquiry_vendor,
            inquiry_product
        );

        wait_until_ready(&mut bulk_out, &mut bulk_in, &mut tag).await?;

        let capacity = bot_read_capacity(&mut bulk_out, &mut bulk_in, &mut tag).await?;
        axtest::axtest_println!(
            "SG2002_DWC2_MSC_CAPACITY blocks={} block_size={}",
            capacity.blocks,
            capacity.block_size
        );

        if capacity.blocks == 0 || capacity.block_size == 0 || capacity.block_size > MAX_BLOCK_SIZE
        {
            return Err(SmokeError::InvalidCapacity {
                blocks: capacity.blocks,
                block_size: capacity.block_size,
            });
        }

        let mut block = vec![0u8; capacity.block_size as usize];
        bot_read10(&mut bulk_out, &mut bulk_in, &mut tag, 0, 1, &mut block).await?;
        let read_checksum = checksum32(&block);
        axtest::axtest_println!(
            "SG2002_DWC2_MSC_READ_OK lba=0 bytes={} checksum=0x{read_checksum:08x}",
            block.len()
        );

        host.host().reset_dwc2_transfer_stats();
        let read_summary = run_read_bench(
            &mut bulk_out,
            &mut bulk_in,
            &mut tag,
            capacity,
            BenchConfig::default(),
        )
        .await?;
        let write_summary = if let Some(config) = compile_time_write_bench_config() {
            Some(run_write_bench(&mut bulk_out, &mut bulk_in, &mut tag, capacity, config).await?)
        } else {
            None
        };
        if let Some(stats) = host.host().dwc2_transfer_stats() {
            axtest::axtest_println!(
                "SG2002_DWC2_STATS transfers={} stages={} dma_allocs={} bounce_to_device_bytes={} \
                 bounce_from_device_bytes={} naks={} xact_errors={} timeouts={} wait_iters={} \
                 init_wait_iters={} transfer_busy_wait_iters={} irq_events={} \
                 channel_completions={}",
                stats.transfers,
                stats.stages,
                stats.dma_allocs,
                stats.bounce_to_device_bytes,
                stats.bounce_from_device_bytes,
                stats.naks,
                stats.xact_errors,
                stats.timeouts,
                stats.wait_iters,
                stats.init_wait_iters,
                stats.transfer_busy_wait_iters,
                stats.irq_events,
                stats.channel_completions
            );
            if stats.transfer_busy_wait_iters != 0 {
                return Err(SmokeError::TransferBusyWait {
                    iters: stats.transfer_busy_wait_iters,
                });
            }
            axtest::axtest_println!(
                "SG2002_DWC2_BUSYWAIT_CHECK transfer_busy_wait_iters={} irq_events={} \
                 channel_completions={}",
                stats.transfer_busy_wait_iters,
                stats.irq_events,
                stats.channel_completions
            );
        }
        axtest::axtest_println!(
            "SG2002_DWC2_MSC_BENCH_SUMMARY read_bytes={} read_ns={} read_checksum=0x{:08x} \
             write_bytes={} write_ns={} write_checksum=0x{:08x}",
            read_summary.bytes,
            read_summary.nanos,
            read_summary.checksum,
            write_summary.map_or(0, |summary| summary.bytes),
            write_summary.map_or(0, |summary| summary.nanos),
            write_summary.map_or(0, |summary| summary.checksum)
        );

        Ok(MscSmokeReport {
            vendor_id,
            product_id,
            inquiry_vendor,
            inquiry_product,
            blocks: capacity.blocks,
            block_size: capacity.block_size,
            read_checksum,
        })
    }

    struct UsbIrqRegistration {
        _irq: IrqId,
        _registration: Registration,
    }

    fn install_usb_irq(
        host: &mut DeviceGuard<PlatformUsbHost>,
    ) -> Result<UsbIrqRegistration, SmokeError> {
        let (binding, handler) = host
            .take_binding_irq_handler()
            .ok_or(SmokeError::MissingIrqBinding)?;
        let irq = resolve_binding_irq(binding).map_err(SmokeError::IrqResolve)?;
        let registration =
            Registration::register_shared(DRIVER_NAME, irq, move |_ctx| match handler.handle() {
                Event::Nothing => IrqReturn::Unhandled,
                Event::TransferActivity { .. } | Event::PortChange { .. } => IrqReturn::Wake,
                Event::Stopped => IrqReturn::Handled,
            })
            .map_err(SmokeError::IrqRegister)?;
        axtest::axtest_println!("SG2002_DWC2_IRQ_MODE mode=irq irq={irq:?}");
        Ok(UsbIrqRegistration {
            _irq: irq,
            _registration: registration,
        })
    }

    #[derive(Clone, Copy)]
    struct BenchSummary {
        bytes: usize,
        nanos: u64,
        checksum: u32,
    }

    async fn run_read_bench(
        bulk_out: &mut EndpointHandle,
        bulk_in: &mut EndpointHandle,
        tag: &mut BotTag,
        capacity: Capacity,
        config: BenchConfig,
    ) -> Result<BenchSummary, SmokeError> {
        let mut total_bytes = 0usize;
        let mut total_nanos = 0u64;
        let mut total_checksum = 0u32;

        for size in config.read_sizes {
            let block_size = capacity.block_size as usize;
            let transfer_blocks = blocks_per_transfer(size, block_size);
            if transfer_blocks == 0 {
                continue;
            }
            let transfer_blocks = u64::from(transfer_blocks).min(capacity.blocks);
            if transfer_blocks == 0 {
                continue;
            }
            let transfer_bytes = transfer_blocks as usize * block_size;
            let max_iters = (capacity.blocks / transfer_blocks).max(1) as usize;
            let iters = bench_iterations(transfer_bytes).min(max_iters);
            let mut buffer = vec![0u8; transfer_bytes];
            let mut lba = 0u32;
            let start = monotonic_time_nanos();
            let mut bytes = 0usize;
            let mut checksum = 0u32;
            for _ in 0..iters {
                bot_read10(
                    bulk_out,
                    bulk_in,
                    tag,
                    lba,
                    transfer_blocks as u16,
                    &mut buffer,
                )
                .await?;
                checksum = checksum.wrapping_add(checksum32(&buffer));
                bytes = bytes.saturating_add(buffer.len());
                lba = lba.saturating_add(transfer_blocks as u32);
            }
            let nanos = monotonic_time_nanos().saturating_sub(start).max(1);
            total_bytes = total_bytes.saturating_add(bytes);
            total_nanos = total_nanos.saturating_add(nanos);
            total_checksum = total_checksum.wrapping_add(checksum);
            axtest::axtest_println!(
                "SG2002_DWC2_MSC_READ_PERF size={} blocks={} iters={} bytes={} ns={} \
                 mib_s_x100={} checksum=0x{checksum:08x}",
                transfer_bytes,
                transfer_blocks,
                iters,
                bytes,
                nanos,
                mib_per_sec_x100(bytes, nanos)
            );
        }

        Ok(BenchSummary {
            bytes: total_bytes,
            nanos: total_nanos,
            checksum: total_checksum,
        })
    }

    async fn run_write_bench(
        bulk_out: &mut EndpointHandle,
        bulk_in: &mut EndpointHandle,
        tag: &mut BotTag,
        capacity: Capacity,
        config: WriteBenchConfig,
    ) -> Result<BenchSummary, SmokeError> {
        let end_lba = u64::from(config.start_lba) + u64::from(config.blocks);
        if end_lba > capacity.blocks {
            return Err(SmokeError::InvalidWriteBench {
                start_lba: config.start_lba,
                blocks: config.blocks,
                capacity_blocks: capacity.blocks,
            });
        }

        let bytes = usize::from(config.blocks) * capacity.block_size as usize;
        let mut pattern = vec![0u8; bytes];
        fill_write_pattern(config.start_lba, &mut pattern);
        let expected_checksum = checksum32(&pattern);

        let start = monotonic_time_nanos();
        bot_write10(
            bulk_out,
            bulk_in,
            tag,
            config.start_lba,
            config.blocks,
            &pattern,
        )
        .await?;
        let nanos = monotonic_time_nanos().saturating_sub(start).max(1);

        let mut readback = vec![0u8; bytes];
        bot_read10(
            bulk_out,
            bulk_in,
            tag,
            config.start_lba,
            config.blocks,
            &mut readback,
        )
        .await?;
        let actual_checksum = checksum32(&readback);
        if actual_checksum != expected_checksum || readback != pattern {
            return Err(SmokeError::WriteVerifyMismatch {
                expected: expected_checksum,
                actual: actual_checksum,
            });
        }

        axtest::axtest_println!(
            "SG2002_DWC2_MSC_WRITE_PERF lba={} blocks={} size={} bytes={} ns={} mib_s_x100={} \
             checksum=0x{expected_checksum:08x}",
            config.start_lba,
            config.blocks,
            bytes,
            bytes,
            nanos,
            mib_per_sec_x100(bytes, nanos)
        );

        Ok(BenchSummary {
            bytes,
            nanos,
            checksum: expected_checksum,
        })
    }

    fn find_msc_interface(info: &DeviceInfo) -> Option<MscInterface> {
        for config in info.configurations() {
            for interface in &config.interfaces {
                for alt in &interface.alt_settings {
                    if alt.class != MSC_CLASS
                        || alt.subclass != MSC_SUBCLASS_SCSI
                        || alt.protocol != MSC_PROTOCOL_BULK_ONLY
                    {
                        continue;
                    }
                    let mut bulk_in = None;
                    let mut bulk_out = None;
                    for endpoint in &alt.endpoints {
                        if endpoint.transfer_type != EndpointType::Bulk {
                            continue;
                        }
                        match endpoint.direction {
                            Direction::In => bulk_in = Some(endpoint.address),
                            Direction::Out => bulk_out = Some(endpoint.address),
                        }
                    }
                    if let (Some(bulk_in), Some(bulk_out)) = (bulk_in, bulk_out) {
                        return Some(MscInterface {
                            interface_number: alt.interface_number,
                            alternate_setting: alt.alternate_setting,
                            bulk_in,
                            bulk_out,
                        });
                    }
                }
            }
        }
        None
    }

    #[derive(Clone, Copy)]
    struct Capacity {
        blocks: u64,
        block_size: u32,
    }

    async fn bot_inquiry(
        bulk_out: &mut EndpointHandle,
        bulk_in: &mut EndpointHandle,
        tag: &mut BotTag,
        data: &mut [u8; 36],
    ) -> Result<(), SmokeError> {
        let command = [0x12, 0, 0, 0, data.len() as u8, 0];
        bot_command_in(bulk_out, bulk_in, tag, &command, data).await?;
        Ok(())
    }

    async fn wait_until_ready(
        bulk_out: &mut EndpointHandle,
        bulk_in: &mut EndpointHandle,
        tag: &mut BotTag,
    ) -> Result<(), SmokeError> {
        let command = [0x00, 0, 0, 0, 0, 0];
        for _ in 0..10 {
            match bot_command_no_data(bulk_out, bulk_in, tag, &command).await {
                Ok(()) => return Ok(()),
                Err(SmokeError::BotCommandFailed { .. }) => {
                    let mut sense = [0u8; 18];
                    let _ = bot_request_sense(bulk_out, bulk_in, tag, &mut sense).await;
                    axtest::axtest_println!(
                        "SG2002_DWC2_MSC_NOT_READY sense_key=0x{:02x} asc=0x{:02x} ascq=0x{:02x}",
                        sense[2] & 0x0f,
                        sense[12],
                        sense[13]
                    );
                    ax_task::sleep(Duration::from_millis(100));
                }
                Err(err) => return Err(err),
            }
        }
        bot_command_no_data(bulk_out, bulk_in, tag, &command).await
    }

    async fn bot_request_sense(
        bulk_out: &mut EndpointHandle,
        bulk_in: &mut EndpointHandle,
        tag: &mut BotTag,
        data: &mut [u8; 18],
    ) -> Result<(), SmokeError> {
        let command = [0x03, 0, 0, 0, data.len() as u8, 0];
        bot_command_in(bulk_out, bulk_in, tag, &command, data).await?;
        Ok(())
    }

    async fn bot_read_capacity(
        bulk_out: &mut EndpointHandle,
        bulk_in: &mut EndpointHandle,
        tag: &mut BotTag,
    ) -> Result<Capacity, SmokeError> {
        let command = [0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut data = [0u8; 8];
        bot_command_in(bulk_out, bulk_in, tag, &command, &mut data).await?;
        let last_lba = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let block_size = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        Ok(Capacity {
            blocks: u64::from(last_lba) + 1,
            block_size,
        })
    }

    async fn bot_read10(
        bulk_out: &mut EndpointHandle,
        bulk_in: &mut EndpointHandle,
        tag: &mut BotTag,
        lba: u32,
        blocks: u16,
        data: &mut [u8],
    ) -> Result<(), SmokeError> {
        let command = build_read10_command(lba, blocks);
        bot_command_in(bulk_out, bulk_in, tag, &command, data).await?;
        Ok(())
    }

    async fn bot_write10(
        bulk_out: &mut EndpointHandle,
        bulk_in: &mut EndpointHandle,
        tag: &mut BotTag,
        lba: u32,
        blocks: u16,
        data: &[u8],
    ) -> Result<(), SmokeError> {
        let command = build_write10_command(lba, blocks);
        bot_command_out(bulk_out, bulk_in, tag, &command, data).await?;
        Ok(())
    }

    async fn bot_command_no_data(
        bulk_out: &mut EndpointHandle,
        bulk_in: &mut EndpointHandle,
        tag: &mut BotTag,
        command: &[u8],
    ) -> Result<(), SmokeError> {
        let opcode = command.first().copied().unwrap_or(0);
        let tag = tag.next();
        let cbw = build_cbw(tag, 0, Direction::Out, command);
        transfer_exact_out(bulk_out, &cbw, "cbw").await?;
        read_csw(bulk_in, tag, opcode).await?;
        Ok(())
    }

    async fn bot_command_in(
        bulk_out: &mut EndpointHandle,
        bulk_in: &mut EndpointHandle,
        tag: &mut BotTag,
        command: &[u8],
        data: &mut [u8],
    ) -> Result<(), SmokeError> {
        let opcode = command.first().copied().unwrap_or(0);
        let tag = tag.next();
        let cbw = build_cbw(tag, data.len() as u32, Direction::In, command);
        transfer_exact_out(bulk_out, &cbw, "cbw").await?;
        transfer_exact_in(bulk_in, data, "data").await?;
        read_csw(bulk_in, tag, opcode).await?;
        Ok(())
    }

    async fn bot_command_out(
        bulk_out: &mut EndpointHandle,
        bulk_in: &mut EndpointHandle,
        tag: &mut BotTag,
        command: &[u8],
        data: &[u8],
    ) -> Result<(), SmokeError> {
        let opcode = command.first().copied().unwrap_or(0);
        let tag = tag.next();
        let cbw = build_cbw(tag, data.len() as u32, Direction::Out, command);
        transfer_exact_out(bulk_out, &cbw, "cbw").await?;
        transfer_exact_out(bulk_out, data, "data").await?;
        read_csw(bulk_in, tag, opcode).await?;
        Ok(())
    }

    async fn transfer_exact_out(
        endpoint: &mut EndpointHandle,
        data: &[u8],
        stage: &'static str,
    ) -> Result<(), SmokeError> {
        let completion = endpoint.wait(TransferRequest::bulk_out(data)).await?;
        if completion.actual_length != data.len() {
            return Err(SmokeError::ShortTransfer {
                stage,
                expected: data.len(),
                actual: completion.actual_length,
            });
        }
        Ok(())
    }

    async fn transfer_exact_in(
        endpoint: &mut EndpointHandle,
        data: &mut [u8],
        stage: &'static str,
    ) -> Result<(), SmokeError> {
        let completion = endpoint.wait(TransferRequest::bulk_in(data)).await?;
        if completion.actual_length != data.len() {
            return Err(SmokeError::ShortTransfer {
                stage,
                expected: data.len(),
                actual: completion.actual_length,
            });
        }
        Ok(())
    }

    async fn read_csw(
        bulk_in: &mut EndpointHandle,
        expected_tag: u32,
        opcode: u8,
    ) -> Result<(), SmokeError> {
        let mut csw = [0u8; CSW_LEN];
        transfer_exact_in(bulk_in, &mut csw, "csw").await?;
        let signature = u32::from_le_bytes([csw[0], csw[1], csw[2], csw[3]]);
        if signature != CSW_SIGNATURE {
            return Err(SmokeError::InvalidCswSignature(signature));
        }
        let actual_tag = u32::from_le_bytes([csw[4], csw[5], csw[6], csw[7]]);
        if actual_tag != expected_tag {
            return Err(SmokeError::CswTagMismatch {
                expected: expected_tag,
                actual: actual_tag,
            });
        }
        let residue = u32::from_le_bytes([csw[8], csw[9], csw[10], csw[11]]);
        let status = csw[12];
        if status != 0 {
            return Err(SmokeError::BotCommandFailed {
                opcode,
                status,
                residue,
            });
        }
        Ok(())
    }

    fn build_cbw(tag: u32, data_len: u32, direction: Direction, command: &[u8]) -> [u8; CBW_LEN] {
        let mut cbw = [0u8; CBW_LEN];
        cbw[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        cbw[4..8].copy_from_slice(&tag.to_le_bytes());
        cbw[8..12].copy_from_slice(&data_len.to_le_bytes());
        cbw[12] = if matches!(direction, Direction::In) {
            0x80
        } else {
            0
        };
        cbw[14] = command.len().min(16) as u8;
        let command_len = command.len().min(16);
        cbw[15..15 + command_len].copy_from_slice(&command[..command_len]);
        cbw
    }

    fn ascii_field(bytes: &[u8]) -> String {
        let mut start = 0usize;
        let mut end = bytes.len();
        while start < end && bytes[start] == b' ' {
            start += 1;
        }
        while end > start && bytes[end - 1] == b' ' {
            end -= 1;
        }
        bytes[start..end]
            .iter()
            .map(|byte| {
                if byte.is_ascii_graphic() || *byte == b' ' {
                    *byte as char
                } else {
                    '.'
                }
            })
            .collect()
    }

    fn checksum32(data: &[u8]) -> u32 {
        data.iter()
            .fold(0u32, |acc, byte| acc.wrapping_add(u32::from(*byte)))
    }

    fn fill_write_pattern(start_lba: u32, data: &mut [u8]) {
        for (index, byte) in data.iter_mut().enumerate() {
            *byte = (index as u8)
                .wrapping_add((start_lba & 0xff) as u8)
                .wrapping_mul(31)
                ^ 0xa5;
        }
    }
}

#[cfg(all(feature = "ax-std", axtest))]
#[axtest::tests]
mod tests {
    use axtest::prelude::*;

    #[test]
    fn sg2002_dwc2_usb_msc_read_smoke() -> axtest::AxTestResult {
        match ax_task::future::block_on(super::sg2002_usb_msc::run()) {
            Ok(report) => {
                axtest_println!(
                    "SG2002_DWC2_MSC_REPORT vid={:04x} pid={:04x} vendor=\"{}\" product=\"{}\" \
                     blocks={} block_size={} checksum=0x{:08x}",
                    report.vendor_id,
                    report.product_id,
                    report.inquiry_vendor,
                    report.inquiry_product,
                    report.blocks,
                    report.block_size,
                    report.read_checksum
                );
                axtest::AxTestResult::Ok
            }
            Err(err) => {
                axtest_println!("SG2002_DWC2_MSC_FAIL error={err}");
                axtest::AxTestResult::Failed
            }
        }
    }
}

#[cfg(not(feature = "ax-std"))]
fn main() {
    eprintln!("arceos-axtest-sg2002-usb-msc is only meaningful as an ArceOS axtest target");
}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[unsafe(no_mangle)]
pub extern "C" fn _start() {}

#[cfg(all(target_os = "none", not(feature = "ax-std")))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
