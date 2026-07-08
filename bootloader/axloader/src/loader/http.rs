extern crate alloc;

use alloc::{vec, vec::Vec};
use core::time::Duration;

use uefi::{
    boot::{self, OpenProtocolAttributes, OpenProtocolParams},
    proto::network::{
        http::{HttpBinding, HttpHelper},
        ip4config2::Ip4Config2,
    },
};
use uefi_raw::protocol::network::http::HttpStatusCode;

const MAX_KERNEL_DOWNLOAD_SIZE: usize = 256 * 1024 * 1024;
const HTTP_RETRY_LIMIT: usize = 8;
const HTTP_RETRY_STALL: Duration = Duration::from_millis(250);
const KERNEL_PROGRESS_STEP_PERCENT: usize = 1;
const KERNEL_PROGRESS_BAR_WIDTH: usize = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadError {
    NoHttpBinding,
    HttpUnavailable,
    ConfigureFailed,
    RequestFailed,
    ResponseFailed,
    BodyTooLarge,
    UnexpectedStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelLoadError {
    ZeroSize,
    SizeTooLarge,
    Download(DownloadError),
    SizeMismatch,
}

pub fn download_sized_body(url: &str, expected_size: u64) -> Result<Vec<u8>, KernelLoadError> {
    let expected_size = checked_kernel_size(expected_size)?;
    crate::logln!("body_download_start: size={}", expected_size);
    let mut body = vec![0; expected_size];
    let received = download_body_to_addr(url, body.as_mut_ptr(), expected_size)
        .map_err(KernelLoadError::Download)?;
    if received != expected_size {
        return Err(KernelLoadError::SizeMismatch);
    }
    Ok(body)
}

fn download_body_to_addr(
    url: &str,
    dst: *mut u8,
    expected_size: usize,
) -> Result<usize, DownloadError> {
    prepare_network();

    let mut client = HttpClient::new()?;
    let mut downloaded = 0usize;
    let mut progress = DownloadProgress::new(expected_size);
    progress.print(downloaded);

    client.request_get(url)?;
    let first = client.response_first()?;
    if first.status != HttpStatusCode::STATUS_200_OK {
        progress.finish_line();
        crate::logln!(
            "http_unexpected_status: {:?} first_body_len={}",
            first.status,
            first.body.len()
        );
        return Err(DownloadError::UnexpectedStatus);
    }
    downloaded = append_download_chunk(dst, expected_size, downloaded, &first.body)?;
    progress.maybe_print(downloaded);

    while downloaded < expected_size {
        let chunk = match retry_http(|| client.response_more_vec()) {
            Ok(chunk) => chunk,
            Err(err) => {
                progress.finish_line();
                crate::logln!(
                    "kernel_download_stopped: offset={} error={err:?}",
                    downloaded
                );
                return Err(err);
            }
        };
        if chunk.is_empty() {
            progress.finish_line();
            crate::logln!("kernel_download_stopped: offset={} zero_chunk", downloaded);
            return Err(DownloadError::ResponseFailed);
        }
        downloaded = append_download_chunk(dst, expected_size, downloaded, &chunk)?;
        progress.maybe_print(downloaded);
    }

    progress.finish_line();
    Ok(downloaded)
}

fn append_download_chunk(
    dst: *mut u8,
    expected_size: usize,
    downloaded: usize,
    chunk: &[u8],
) -> Result<usize, DownloadError> {
    let next = downloaded
        .checked_add(chunk.len())
        .ok_or(DownloadError::BodyTooLarge)?;
    if next > expected_size {
        return Err(DownloadError::BodyTooLarge);
    }
    unsafe {
        core::ptr::copy_nonoverlapping(chunk.as_ptr(), dst.add(downloaded), chunk.len());
    }
    Ok(next)
}

fn prepare_network() {
    let handles = match boot::find_handles::<Ip4Config2>() {
        Ok(handles) => handles,
        Err(err) => {
            crate::logln!("network_ifup_failed: {:?}", err.status());
            return;
        }
    };

    let mut last_error = None;
    for handle in handles.iter().copied() {
        let mut protocol = match unsafe {
            boot::open_protocol::<Ip4Config2>(
                OpenProtocolParams {
                    handle,
                    agent: boot::image_handle(),
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
        } {
            Ok(protocol) => protocol,
            Err(err) => {
                last_error = Some(err.status());
                continue;
            }
        };

        match protocol.ifup() {
            Ok(()) => return,
            Err(err) => last_error = Some(err.status()),
        }
    }

    if let Some(status) = last_error {
        crate::logln!("network_ifup_failed: {status:?}");
    } else {
        crate::logln!("network_ifup_failed: no IPv4 config handle");
    }
}

struct DownloadProgress {
    expected_size: usize,
    next_percent: usize,
}

impl DownloadProgress {
    fn new(expected_size: usize) -> Self {
        Self {
            expected_size,
            next_percent: KERNEL_PROGRESS_STEP_PERCENT,
        }
    }

    fn maybe_print(&mut self, downloaded: usize) {
        let percent = download_percent(downloaded, self.expected_size);
        if percent >= self.next_percent || downloaded == self.expected_size {
            self.print(downloaded);
            while self.next_percent <= percent {
                self.next_percent += KERNEL_PROGRESS_STEP_PERCENT;
            }
        }
    }

    fn print(&self, downloaded: usize) {
        let percent = download_percent(downloaded, self.expected_size);
        let filled = percent.saturating_mul(KERNEL_PROGRESS_BAR_WIDTH) / 100;
        crate::log!("\rdownload: [");
        for index in 0..KERNEL_PROGRESS_BAR_WIDTH {
            crate::log!("{}", if index < filled { "#" } else { "-" });
        }
        crate::log!("] {:>3}% ", percent);
        print_human_size(downloaded);
        crate::log!("/");
        print_human_size(self.expected_size);
        crate::log!("    ");
    }

    fn finish_line(&self) {
        crate::logln!("");
    }
}

fn download_percent(downloaded: usize, expected_size: usize) -> usize {
    downloaded
        .saturating_mul(100)
        .checked_div(expected_size)
        .unwrap_or(0)
}

fn print_human_size(bytes: usize) {
    const KIB: usize = 1024;
    const MIB: usize = 1024 * 1024;

    if bytes >= MIB {
        print_fixed_2(bytes, MIB);
        crate::log!(" MiB");
    } else if bytes >= KIB {
        print_fixed_2(bytes, KIB);
        crate::log!(" KiB");
    } else {
        crate::log!("{} B", bytes);
    }
}

fn print_fixed_2(value: usize, unit: usize) {
    let whole = value / unit;
    let hundredths = value % unit * 100 / unit;
    crate::log!("{}.", whole);
    if hundredths < 10 {
        crate::log!("0");
    }
    crate::log!("{}", hundredths);
}

fn retry_http<T>(mut op: impl FnMut() -> Result<T, DownloadError>) -> Result<T, DownloadError> {
    let mut last_error = None;
    for attempt in 1..=HTTP_RETRY_LIMIT {
        match op() {
            Ok(value) => return Ok(value),
            Err(err) => {
                last_error = Some(err);
                if attempt < HTTP_RETRY_LIMIT {
                    boot::stall(HTTP_RETRY_STALL);
                }
            }
        }
    }
    Err(last_error.expect("retry loop always runs at least once"))
}

struct HttpClient {
    helper: HttpHelper,
}

impl HttpClient {
    fn new() -> Result<Self, DownloadError> {
        let handles =
            boot::find_handles::<HttpBinding>().map_err(|_| DownloadError::HttpUnavailable)?;
        let nic_handle = handles
            .first()
            .copied()
            .ok_or(DownloadError::NoHttpBinding)?;
        let mut helper = HttpHelper::new(nic_handle).map_err(|_| DownloadError::HttpUnavailable)?;
        helper
            .configure()
            .map_err(|_| DownloadError::ConfigureFailed)?;
        Ok(Self { helper })
    }

    fn request_get(&mut self, url: &str) -> Result<(), DownloadError> {
        self.helper
            .request_get(url)
            .map_err(|_| DownloadError::RequestFailed)
    }

    fn response_first(
        &mut self,
    ) -> Result<uefi::proto::network::http::HttpHelperResponse, DownloadError> {
        self.helper
            .response_first(true)
            .map_err(|_| DownloadError::ResponseFailed)
    }

    fn response_more_vec(&mut self) -> Result<Vec<u8>, DownloadError> {
        let mut body = Vec::new();
        self.helper
            .response_more(&mut body)
            .map_err(|_| DownloadError::ResponseFailed)?;
        Ok(body)
    }
}

fn checked_kernel_size(expected_size: u64) -> Result<usize, KernelLoadError> {
    if expected_size == 0 {
        return Err(KernelLoadError::ZeroSize);
    }
    if expected_size > MAX_KERNEL_DOWNLOAD_SIZE as u64 {
        return Err(KernelLoadError::SizeTooLarge);
    }
    Ok(expected_size as usize)
}
