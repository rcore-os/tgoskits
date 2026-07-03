#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_std)]
#![cfg_attr(any(feature = "ax-std", target_os = "none"), no_main)]

extern crate alloc;

#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(all(feature = "ax-std", axtest))]
mod sg2002_usb_msc {
    use alloc::{string::String, vec};
    use core::{fmt, time::Duration};

    use ax_driver::usb::PlatformUsbHost;
    use crab_usb::{
        DeviceInfo, Endpoint,
        err::{TransferError, USBError},
        usb_if::{descriptor::EndpointType, endpoint::TransferRequest, transfer::Direction},
    };
    use rdrive::{DeviceGuard, DriverGeneric, GetDeviceError};

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
    }

    impl fmt::Display for SmokeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::NoHost => write!(f, "SG2002 DWC2 host not found"),
                Self::HostLock(err) => write!(f, "host lock failed: {err:?}"),
                Self::Usb(err) => write!(f, "USB error: {err:?}"),
                Self::Transfer(err) => write!(f, "transfer error: {err:?}"),
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
        host.host_mut().init().await?;
        let devices = host.host_mut().probe_devices().await?;
        for probed in devices {
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
        device
            .claim_interface(msc.interface_number, msc.alternate_setting)
            .await?;
        let mut endpoints = device.take_endpoints_for_interface(msc.interface_number)?;
        let mut bulk_in = endpoints
            .remove(&msc.bulk_in)
            .ok_or(SmokeError::MissingBulkEndpoint)?;
        let mut bulk_out = endpoints
            .remove(&msc.bulk_out)
            .ok_or(SmokeError::MissingBulkEndpoint)?;
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
        bot_read10(&mut bulk_out, &mut bulk_in, &mut tag, 0, &mut block).await?;
        let read_checksum = checksum32(&block);
        axtest::axtest_println!(
            "SG2002_DWC2_MSC_READ_OK lba=0 bytes={} checksum=0x{read_checksum:08x}",
            block.len()
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
        bulk_out: &mut Endpoint,
        bulk_in: &mut Endpoint,
        tag: &mut BotTag,
        data: &mut [u8; 36],
    ) -> Result<(), SmokeError> {
        let command = [0x12, 0, 0, 0, data.len() as u8, 0];
        bot_command_in(bulk_out, bulk_in, tag, &command, data).await?;
        Ok(())
    }

    async fn wait_until_ready(
        bulk_out: &mut Endpoint,
        bulk_in: &mut Endpoint,
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
        bulk_out: &mut Endpoint,
        bulk_in: &mut Endpoint,
        tag: &mut BotTag,
        data: &mut [u8; 18],
    ) -> Result<(), SmokeError> {
        let command = [0x03, 0, 0, 0, data.len() as u8, 0];
        bot_command_in(bulk_out, bulk_in, tag, &command, data).await?;
        Ok(())
    }

    async fn bot_read_capacity(
        bulk_out: &mut Endpoint,
        bulk_in: &mut Endpoint,
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
        bulk_out: &mut Endpoint,
        bulk_in: &mut Endpoint,
        tag: &mut BotTag,
        lba: u32,
        data: &mut [u8],
    ) -> Result<(), SmokeError> {
        let lba = lba.to_be_bytes();
        let command = [0x28, 0, lba[0], lba[1], lba[2], lba[3], 0, 0, 1, 0];
        bot_command_in(bulk_out, bulk_in, tag, &command, data).await?;
        Ok(())
    }

    async fn bot_command_no_data(
        bulk_out: &mut Endpoint,
        bulk_in: &mut Endpoint,
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
        bulk_out: &mut Endpoint,
        bulk_in: &mut Endpoint,
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

    async fn transfer_exact_out(
        endpoint: &mut Endpoint,
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
        endpoint: &mut Endpoint,
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
        bulk_in: &mut Endpoint,
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
