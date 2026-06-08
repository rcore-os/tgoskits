extern crate alloc;

use alloc::{string::String, vec, vec::Vec};
use core::{ffi::c_void, num::NonZeroUsize, ptr, ptr::NonNull, time::Duration};

use uefi::{
    Handle, Status,
    boot::{self, AllocateType, OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol},
    mem::memory_map::MemoryType,
    proto::network::http::{Http, HttpBinding, HttpHelper},
};
use uefi_raw::protocol::network::http::{
    HttpHeader, HttpMessage, HttpMethod, HttpRequestData, HttpResponseData, HttpStatusCode,
    HttpToken,
};

const MAX_KERNEL_DOWNLOAD_SIZE: usize = 256 * 1024 * 1024;
const KERNEL_RANGE_CHUNK_SIZE: usize = 1024;
const HTTP_RETRY_LIMIT: usize = 8;
const HTTP_RETRY_STALL: Duration = Duration::from_millis(250);
const UEFI_PAGE_SIZE: usize = 4096;
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
    InvalidUrl,
    RangeHeaderTooLarge,
    UnexpectedStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelLoadError {
    ZeroSize,
    SizeTooLarge,
    LoadAddressTooLarge,
    LoadAddressNotAligned,
    PageCountOverflow,
    AllocateFailed,
    Download(DownloadError),
    SizeMismatch,
}

pub fn download_body(url: &str, max_len: usize) -> Result<Vec<u8>, DownloadError> {
    retry_http(|| download_body_once(url, max_len))
}

pub fn get_json_body(url: &str, max_len: usize) -> Result<Vec<u8>, DownloadError> {
    let mut client = HttpClient::new()?;
    let response = retry_http(|| client.get(url, max_len))?;
    if response.status != HttpStatusCode::STATUS_200_OK {
        return Err(DownloadError::UnexpectedStatus);
    }
    Ok(response.body)
}

pub fn post_json_body(url: &str, body: &str, max_len: usize) -> Result<Vec<u8>, DownloadError> {
    let mut client = HttpClient::new()?;
    let response = retry_http(|| client.post_json(url, body, max_len))?;
    if response.status != HttpStatusCode::STATUS_200_OK {
        return Err(DownloadError::UnexpectedStatus);
    }
    Ok(response.body)
}

fn download_body_once(url: &str, max_len: usize) -> Result<Vec<u8>, DownloadError> {
    let handles =
        boot::find_handles::<HttpBinding>().map_err(|_| DownloadError::HttpUnavailable)?;
    let handle = handles
        .first()
        .copied()
        .ok_or(DownloadError::NoHttpBinding)?;

    let mut http = HttpHelper::new(handle).map_err(|_| DownloadError::HttpUnavailable)?;
    http.configure()
        .map_err(|_| DownloadError::ConfigureFailed)?;
    http.request_get(url)
        .map_err(|_| DownloadError::RequestFailed)?;

    let response = http
        .response_first(true)
        .map_err(|_| DownloadError::ResponseFailed)?;
    if response.body.len() > max_len {
        return Err(DownloadError::BodyTooLarge);
    }

    Ok(response.body)
}

pub struct LoadedKernel {
    pub ptr: NonNull<u8>,
    pub page_count: usize,
    pub size: usize,
}

pub fn download_kernel(
    url: &str,
    load_addr: u64,
    expected_size: u64,
) -> Result<LoadedKernel, KernelLoadError> {
    let expected_size = checked_kernel_size(expected_size)?;
    let page_count = kernel_page_count(load_addr, expected_size)?;
    crate::logln!(
        "kernel_download_start: load={:#x} pages={} size={}",
        load_addr,
        page_count,
        expected_size
    );
    let target = boot::allocate_pages(
        AllocateType::Address(load_addr),
        MemoryType::LOADER_DATA,
        page_count,
    )
    .map_err(|_| KernelLoadError::AllocateFailed)?;

    let received = match download_body_to_addr(url, target.as_ptr(), expected_size) {
        Ok(received) => received,
        Err(err) => {
            unsafe {
                let _ = boot::free_pages(target, page_count);
            }
            return Err(KernelLoadError::Download(err));
        }
    };
    if received != expected_size {
        unsafe {
            let _ = boot::free_pages(target, page_count);
        }
        return Err(KernelLoadError::SizeMismatch);
    }

    Ok(LoadedKernel {
        ptr: target,
        page_count,
        size: expected_size,
    })
}

fn download_body_to_addr(
    url: &str,
    dst: *mut u8,
    expected_size: usize,
) -> Result<usize, DownloadError> {
    let mut client = HttpClient::new()?;
    let mut downloaded = 0usize;
    let mut progress = DownloadProgress::new(expected_size);
    progress.print(downloaded);

    while downloaded < expected_size {
        let chunk_len = (expected_size - downloaded).min(KERNEL_RANGE_CHUNK_SIZE);
        let range_start = downloaded;
        let range_end = downloaded + chunk_len - 1;
        let chunk = match retry_http(|| client.get_range(url, range_start, range_end)) {
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
        if chunk.len() > chunk_len {
            progress.finish_line();
            crate::logln!(
                "kernel_download_stopped: offset={} chunk_too_large={}",
                downloaded,
                chunk.len()
            );
            return Err(DownloadError::BodyTooLarge);
        }
        if chunk.is_empty() {
            progress.finish_line();
            crate::logln!("kernel_download_stopped: offset={} zero_chunk", downloaded);
            return Err(DownloadError::ResponseFailed);
        }
        unsafe {
            core::ptr::copy_nonoverlapping(chunk.as_ptr(), dst.add(downloaded), chunk.len());
        }
        downloaded += chunk.len();
        progress.maybe_print(downloaded);
    }

    progress.finish_line();
    Ok(downloaded)
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
    if expected_size == 0 {
        0
    } else {
        downloaded.saturating_mul(100) / expected_size
    }
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

pub fn retry_limit() -> usize {
    HTTP_RETRY_LIMIT
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
    child_handle: Handle,
    binding: ScopedProtocol<HttpBinding>,
    protocol: Option<ScopedProtocol<Http>>,
}

impl HttpClient {
    fn new() -> Result<Self, DownloadError> {
        let handles =
            boot::find_handles::<HttpBinding>().map_err(|_| DownloadError::HttpUnavailable)?;
        let nic_handle = handles
            .first()
            .copied()
            .ok_or(DownloadError::NoHttpBinding)?;
        let mut binding = unsafe {
            boot::open_protocol::<HttpBinding>(
                OpenProtocolParams {
                    handle: nic_handle,
                    agent: boot::image_handle(),
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
        }
        .map_err(|_| DownloadError::HttpUnavailable)?;

        let child_handle = binding
            .create_child()
            .map_err(|_| DownloadError::HttpUnavailable)?;
        let protocol = match unsafe {
            boot::open_protocol::<Http>(
                OpenProtocolParams {
                    handle: child_handle,
                    agent: boot::image_handle(),
                    controller: None,
                },
                OpenProtocolAttributes::GetProtocol,
            )
        } {
            Ok(protocol) => protocol,
            Err(_) => {
                let _ = binding.destroy_child(child_handle);
                return Err(DownloadError::HttpUnavailable);
            }
        };

        let mut client = Self {
            child_handle,
            binding,
            protocol: Some(protocol),
        };
        client.configure()?;
        Ok(client)
    }

    fn configure(&mut self) -> Result<(), DownloadError> {
        let mut helper = HttpHelperProxy {
            protocol: self.protocol.take(),
        };
        let result = helper.configure();
        self.protocol = helper.protocol;
        result
    }

    fn get_range(&mut self, url: &str, start: usize, end: usize) -> Result<Vec<u8>, DownloadError> {
        let range = range_header_value(start, end)?;
        self.request_get(url, Some(range.as_str()))?;
        let response = self.response_first(KERNEL_RANGE_CHUNK_SIZE)?;
        if response.status != HttpStatusCode::STATUS_206_PARTIAL_CONTENT {
            return Err(DownloadError::UnexpectedStatus);
        }
        Ok(response.body)
    }

    fn get(&mut self, url: &str, max_len: usize) -> Result<HttpResponse, DownloadError> {
        self.request_get(url, None)?;
        self.response_first(max_len)
    }

    fn post_json(
        &mut self,
        url: &str,
        body: &str,
        max_len: usize,
    ) -> Result<HttpResponse, DownloadError> {
        self.request_post(url, body)?;
        self.response_first(max_len)
    }

    fn request_get(&mut self, url: &str, range: Option<&str>) -> Result<(), DownloadError> {
        let url16 = uefi::CString16::try_from(url).map_err(|_| DownloadError::InvalidUrl)?;
        let host = url.split('/').nth(2).ok_or(DownloadError::InvalidUrl)?;
        let mut host = String::from(host);
        host.push('\0');

        let mut request = HttpRequestData {
            method: HttpMethod::GET,
            url: url16.as_ptr().cast::<u16>(),
        };
        let mut headers = vec![HttpHeader {
            field_name: c"Host".as_ptr().cast::<u8>(),
            field_value: host.as_ptr(),
        }];

        let range = range.map(|range| {
            let mut range = String::from(range);
            range.push('\0');
            range
        });
        if let Some(range) = range.as_ref() {
            headers.push(HttpHeader {
                field_name: c"Range".as_ptr().cast::<u8>(),
                field_value: range.as_ptr(),
            });
        }

        let mut message = HttpMessage::default();
        message.data.request = &mut request;
        message.header_count = headers.len();
        message.header = headers.as_mut_ptr();

        let mut token = HttpToken {
            status: Status::NOT_READY,
            message: &mut message,
            ..Default::default()
        };

        let protocol = self.protocol.as_mut().unwrap();
        protocol
            .request(&mut token)
            .map_err(|_| DownloadError::RequestFailed)?;
        wait_for_http_token(protocol, &mut token).map_err(|_| DownloadError::RequestFailed)
    }

    fn request_post(&mut self, url: &str, body: &str) -> Result<(), DownloadError> {
        let url16 = uefi::CString16::try_from(url).map_err(|_| DownloadError::InvalidUrl)?;
        let host = url.split('/').nth(2).ok_or(DownloadError::InvalidUrl)?;
        let mut host = String::from(host);
        host.push('\0');
        let mut content_type = String::from("application/json");
        content_type.push('\0');
        let mut content_length = String::new();
        push_usize(&mut content_length, body.len());
        content_length.push('\0');

        let mut request = HttpRequestData {
            method: HttpMethod::POST,
            url: url16.as_ptr().cast::<u16>(),
        };
        let mut headers = [
            HttpHeader {
                field_name: c"Host".as_ptr().cast::<u8>(),
                field_value: host.as_ptr(),
            },
            HttpHeader {
                field_name: c"Content-Type".as_ptr().cast::<u8>(),
                field_value: content_type.as_ptr(),
            },
            HttpHeader {
                field_name: c"Content-Length".as_ptr().cast::<u8>(),
                field_value: content_length.as_ptr(),
            },
        ];

        let mut message = HttpMessage::default();
        message.data.request = &mut request;
        message.header_count = headers.len();
        message.header = headers.as_mut_ptr();
        message.body_length = body.len();
        message.body = body.as_ptr().cast::<c_void>().cast_mut();

        let mut token = HttpToken {
            status: Status::NOT_READY,
            message: &mut message,
            ..Default::default()
        };

        let protocol = self.protocol.as_mut().unwrap();
        protocol
            .request(&mut token)
            .map_err(|_| DownloadError::RequestFailed)?;
        wait_for_http_token(protocol, &mut token).map_err(|_| DownloadError::RequestFailed)
    }

    fn response_first(&mut self, max_len: usize) -> Result<HttpResponse, DownloadError> {
        let mut response_data = HttpResponseData {
            status_code: HttpStatusCode::STATUS_UNSUPPORTED,
        };
        let mut body = vec![0; max_len];
        let mut message = HttpMessage::default();
        message.data.response = &mut response_data;
        message.body_length = body.len();
        message.body = if body.is_empty() {
            ptr::null_mut()
        } else {
            body.as_mut_ptr().cast::<c_void>()
        };

        let mut token = HttpToken {
            status: Status::NOT_READY,
            message: &mut message,
            ..Default::default()
        };

        let protocol = self.protocol.as_mut().unwrap();
        protocol
            .response(&mut token)
            .map_err(|_| DownloadError::ResponseFailed)?;
        wait_for_http_token(protocol, &mut token).map_err(|_| DownloadError::ResponseFailed)?;

        body.truncate(message.body_length);
        Ok(HttpResponse {
            status: response_data.status_code,
            body,
        })
    }
}

impl Drop for HttpClient {
    fn drop(&mut self) {
        self.protocol = None;
        let _ = self.binding.destroy_child(self.child_handle);
    }
}

struct HttpHelperProxy {
    protocol: Option<ScopedProtocol<Http>>,
}

impl HttpHelperProxy {
    fn configure(&mut self) -> Result<(), DownloadError> {
        use uefi_raw::protocol::network::http::{
            HttpAccessPoint, HttpConfigData, HttpV4AccessPoint, HttpVersion,
        };

        let ip4 = HttpV4AccessPoint {
            use_default_addr: true.into(),
            ..Default::default()
        };
        let config = HttpConfigData {
            http_version: HttpVersion::HTTP_VERSION_10,
            time_out_millisec: 10_000,
            local_addr_is_ipv6: false.into(),
            access_point: HttpAccessPoint { ipv4_node: &ip4 },
        };

        self.protocol
            .as_mut()
            .unwrap()
            .configure(&config)
            .map_err(|_| DownloadError::ConfigureFailed)
    }
}

struct HttpResponse {
    status: HttpStatusCode,
    body: Vec<u8>,
}

fn wait_for_http_token(protocol: &mut Http, token: &mut HttpToken) -> Result<(), Status> {
    loop {
        if token.status != Status::NOT_READY {
            break;
        }
        protocol.poll().map_err(|err| err.status())?;
    }

    if token.status == Status::SUCCESS || token.status == Status::HTTP_ERROR {
        Ok(())
    } else {
        Err(token.status)
    }
}

fn range_header_value(start: usize, end: usize) -> Result<String, DownloadError> {
    let mut range = String::from("bytes=");
    push_usize(&mut range, start);
    range.push('-');
    push_usize(&mut range, end);
    if range.len() >= 64 {
        return Err(DownloadError::RangeHeaderTooLarge);
    }
    Ok(range)
}

fn push_usize(output: &mut String, mut value: usize) {
    let mut digits = [0u8; 20];
    let mut len = 0usize;
    if value == 0 {
        output.push('0');
        return;
    }
    while value > 0 && len < digits.len() {
        digits[len] = b'0' + (value % 10) as u8;
        value /= 10;
        len += 1;
    }
    while len > 0 {
        len -= 1;
        output.push(char::from(digits[len]));
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

fn kernel_page_count(load_addr: u64, expected_size: usize) -> Result<usize, KernelLoadError> {
    if load_addr as usize as u64 != load_addr {
        return Err(KernelLoadError::LoadAddressTooLarge);
    }
    if load_addr as usize % UEFI_PAGE_SIZE != 0 {
        return Err(KernelLoadError::LoadAddressNotAligned);
    }

    expected_size
        .checked_add(UEFI_PAGE_SIZE - 1)
        .map(|size| size / UEFI_PAGE_SIZE)
        .and_then(NonZeroUsize::new)
        .map(NonZeroUsize::get)
        .ok_or(KernelLoadError::PageCountOverflow)
}
