extern crate alloc;

use alloc::{
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::{
    ffi::{CStr, c_char, c_void},
    ptr::{self, NonNull},
    time::Duration,
};

use uefi::{
    CString16, Handle, Status, boot,
    boot::{OpenProtocolAttributes, OpenProtocolParams, ScopedProtocol},
    proto::network::http::{Http, HttpBinding, HttpHelperResponse},
};
use uefi_raw::protocol::network::http::{
    HttpAccessPoint, HttpConfigData, HttpHeader, HttpMessage, HttpMethod, HttpRequestData,
    HttpResponseData, HttpStatusCode, HttpToken, HttpV4AccessPoint, HttpVersion,
};

const MAX_KERNEL_DOWNLOAD_SIZE: usize = 256 * 1024 * 1024;
const HTTP_RETRY_LIMIT: usize = 8;
const HTTP_RETRY_STALL: Duration = Duration::from_millis(250);
const KERNEL_PROGRESS_STEP_PERCENT: usize = 1;
const KERNEL_PROGRESS_BAR_WIDTH: usize = 50;
const MAX_CONTROL_RESPONSE_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadError {
    NoHttpBinding,
    HttpUnavailable,
    ConfigureFailed,
    RequestFailed,
    ResponseFailed,
    BodyTooLarge,
    ContentLengthMismatch,
    UnexpectedStatus,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelLoadError {
    ZeroSize,
    SizeTooLarge,
    Download(DownloadError),
    SizeMismatch,
}

pub fn download_sized_body(
    nic_handle: Handle,
    url: &str,
    expected_size: u64,
) -> Result<Vec<u8>, KernelLoadError> {
    let expected_size = checked_kernel_size(expected_size)?;
    crate::logln!("body_download_start: size={}", expected_size);
    let mut body = vec![0; expected_size];
    let received = download_body_to_addr(nic_handle, url, body.as_mut_ptr(), expected_size)
        .map_err(KernelLoadError::Download)?;
    if received != expected_size {
        return Err(KernelLoadError::SizeMismatch);
    }
    Ok(body)
}

fn download_body_to_addr(
    nic_handle: Handle,
    url: &str,
    dst: *mut u8,
    expected_size: usize,
) -> Result<usize, DownloadError> {
    let mut client = HttpClient::new(nic_handle)?;
    let mut downloaded = 0usize;
    let mut progress = DownloadProgress::new(expected_size);
    progress.print(downloaded);

    client.request_get(url)?;
    let first = client.response_first(true)?;
    if first.status != HttpStatusCode::STATUS_200_OK {
        progress.finish_line();
        crate::logln!(
            "http_unexpected_status: {:?} first_body_len={}",
            first.status,
            first.body.len()
        );
        return Err(DownloadError::UnexpectedStatus);
    }
    if response_content_length(&first.headers) != Some(expected_size) {
        progress.finish_line();
        crate::logln!(
            "kernel_download_content_length_mismatch: expected={} actual={:?}",
            expected_size,
            response_content_length(&first.headers)
        );
        return Err(DownloadError::ContentLengthMismatch);
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

fn response_content_length(headers: &[(String, String)]) -> Option<usize> {
    let mut values = headers
        .iter()
        .filter(|(name, _)| name == "content-length")
        .map(|(_, value)| value.parse::<usize>().ok());
    let length = values.next()??;
    if values.next().is_some() {
        return None;
    }
    Some(length)
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

pub struct HttpClient {
    child: Handle,
    binding: ScopedProtocol<HttpBinding>,
    protocol: Option<ScopedProtocol<Http>>,
}

impl HttpClient {
    pub fn new(nic_handle: Handle) -> Result<Self, DownloadError> {
        let mut binding =
            open_protocol::<HttpBinding>(nic_handle).map_err(|_| DownloadError::HttpUnavailable)?;
        let child = binding
            .create_child()
            .map_err(|_| DownloadError::HttpUnavailable)?;
        let protocol = match open_protocol::<Http>(child) {
            Ok(protocol) => protocol,
            Err(_) => {
                let _ = binding.destroy_child(child);
                return Err(DownloadError::HttpUnavailable);
            }
        };
        let mut client = Self {
            child,
            binding,
            protocol: Some(protocol),
        };
        client.configure()?;
        Ok(client)
    }

    pub fn post_json<Request: serde::Serialize, Response: serde::de::DeserializeOwned>(
        &mut self,
        url: &str,
        request: &Request,
    ) -> Result<Response, DownloadError> {
        let mut body = serde_json::to_vec(request).map_err(|_| DownloadError::Json)?;
        self.request(HttpMethod::POST, url, Some(&mut body), true)?;
        let response = self.response_body()?;
        serde_json::from_slice(&response).map_err(|_| DownloadError::Json)
    }

    pub fn post_status<Request: serde::Serialize>(
        &mut self,
        url: &str,
        request: &Request,
    ) -> Result<(), DownloadError> {
        let mut body = serde_json::to_vec(request).map_err(|_| DownloadError::Json)?;
        self.request(HttpMethod::POST, url, Some(&mut body), true)?;
        let response = self.response_first(false)?;
        if response.status != HttpStatusCode::STATUS_204_NO_CONTENT {
            crate::logln!(
                "http_status_unexpected_status: {:?} body_len={}",
                response.status,
                response.body.len()
            );
            return Err(DownloadError::UnexpectedStatus);
        }
        Ok(())
    }

    fn response_body(&mut self) -> Result<Vec<u8>, DownloadError> {
        let first = self.response_first(true)?;
        if first.status != HttpStatusCode::STATUS_200_OK {
            crate::logln!(
                "http_control_unexpected_status: {:?} body_len={}",
                first.status,
                first.body.len()
            );
            return Err(DownloadError::UnexpectedStatus);
        }
        let content_length = response_content_length(&first.headers);
        if content_length.is_some_and(|length| length > MAX_CONTROL_RESPONSE_SIZE) {
            return Err(DownloadError::BodyTooLarge);
        }
        let mut body = first.body;
        if content_length.is_some_and(|length| body.len() > length) {
            return Err(DownloadError::ResponseFailed);
        }
        while body.len() < MAX_CONTROL_RESPONSE_SIZE {
            if content_length.is_some_and(|length| body.len() == length) {
                return Ok(body);
            }
            let previous_len = body.len();
            self.response_more(&mut body)?;
            if body.len() == previous_len {
                return Ok(body);
            }
        }
        if content_length == Some(body.len()) {
            Ok(body)
        } else {
            Err(DownloadError::BodyTooLarge)
        }
    }

    fn request_get(&mut self, url: &str) -> Result<(), DownloadError> {
        self.request(HttpMethod::GET, url, None, false)
    }

    fn configure(&mut self) -> Result<(), DownloadError> {
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
        self.protocol_mut()
            .configure(&config)
            .map_err(|_| DownloadError::ConfigureFailed)
    }

    fn request(
        &mut self,
        method: HttpMethod,
        url: &str,
        body: Option<&mut [u8]>,
        json: bool,
    ) -> Result<(), DownloadError> {
        let host = url_host(url)?;
        let url = CString16::try_from(url).map_err(|_| DownloadError::RequestFailed)?;
        let host = nul_terminated(host.as_bytes())?;
        let content_length = json
            .then(|| body.as_ref().map_or(0, |body| body.len()).to_string())
            .map(|length| nul_terminated(length.as_bytes()))
            .transpose()?;
        let mut headers = vec![HttpHeader {
            field_name: c"Host".as_ptr().cast::<u8>(),
            field_value: host.as_ptr(),
        }];
        if let Some(content_length) = &content_length {
            headers.push(HttpHeader {
                field_name: c"Content-Type".as_ptr().cast::<u8>(),
                field_value: c"application/json".as_ptr().cast::<u8>(),
            });
            headers.push(HttpHeader {
                field_name: c"Content-Length".as_ptr().cast::<u8>(),
                field_value: content_length.as_ptr(),
            });
        }

        let mut request = HttpRequestData {
            method,
            url: url.as_ptr().cast::<u16>(),
        };
        let mut message = HttpMessage::default();
        message.data.request = &mut request;
        message.header_count = headers.len();
        message.header = headers.as_mut_ptr();
        if let Some(body) = body {
            message.body_length = body.len();
            message.body = body.as_mut_ptr().cast::<c_void>();
        }
        let mut token = HttpToken {
            status: Status::NOT_READY,
            message: &mut message,
            ..Default::default()
        };
        let protocol = self.protocol_mut();
        protocol
            .request(&mut token)
            .map_err(|_| DownloadError::RequestFailed)?;
        while token.status == Status::NOT_READY {
            protocol.poll().map_err(|_| DownloadError::RequestFailed)?;
        }
        if token.status == Status::SUCCESS {
            Ok(())
        } else {
            Err(DownloadError::RequestFailed)
        }
    }

    fn response_first(&mut self, expect_body: bool) -> Result<HttpHelperResponse, DownloadError> {
        let mut response = HttpResponseData {
            status_code: HttpStatusCode::STATUS_UNSUPPORTED,
        };
        let mut body = vec![0; if expect_body { 16 * 1024 } else { 0 }];
        let mut message = HttpMessage::default();
        message.data.response = &mut response;
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
        let protocol = self.protocol_mut();
        protocol
            .response(&mut token)
            .map_err(|_| DownloadError::ResponseFailed)?;
        while token.status == Status::NOT_READY {
            protocol.poll().map_err(|_| DownloadError::ResponseFailed)?;
        }
        let response_header_allocation =
            FirmwareResponseHeaders::new(message.header, message.header_count);
        if token.status != Status::SUCCESS && token.status != Status::HTTP_ERROR {
            return Err(DownloadError::ResponseFailed);
        }
        if message.body_length > body.len()
            || (message.header_count != 0 && message.header.is_null())
        {
            return Err(DownloadError::ResponseFailed);
        }
        let mut response_headers = Vec::with_capacity(message.header_count);
        for index in 0..message.header_count {
            // SAFETY: the HTTP response completed successfully, the firmware
            // returned a non-null header table for `header_count` entries, and
            // `response_header_allocation` keeps all firmware allocations alive
            // until their strings have been copied below.
            let header = unsafe { &*message.header.add(index) };
            if header.field_name.is_null() || header.field_value.is_null() {
                return Err(DownloadError::ResponseFailed);
            }
            // SAFETY: EFI_HTTP_HEADER requires both validated non-null pointers
            // to address NUL-terminated ASCII strings for the response lifetime.
            let name = unsafe { CStr::from_ptr(header.field_name.cast::<c_char>()) }
                .to_str()
                .map_err(|_| DownloadError::ResponseFailed)?;
            // SAFETY: the same EFI_HTTP_HEADER string contract applies to the
            // value pointer checked above.
            let value = unsafe { CStr::from_ptr(header.field_value.cast::<c_char>()) }
                .to_str()
                .map_err(|_| DownloadError::ResponseFailed)?;
            response_headers.push((name.to_ascii_lowercase(), String::from(value)));
        }
        body.truncate(message.body_length);
        drop(response_header_allocation);
        Ok(HttpHelperResponse {
            status: response.status_code,
            headers: response_headers,
            body,
        })
    }

    fn response_more(&mut self, body: &mut Vec<u8>) -> Result<(), DownloadError> {
        let mut chunk = vec![0; 16 * 1024];
        let mut message = HttpMessage {
            body_length: chunk.len(),
            body: chunk.as_mut_ptr().cast::<c_void>(),
            ..Default::default()
        };
        let mut token = HttpToken {
            status: Status::NOT_READY,
            message: &mut message,
            ..Default::default()
        };
        let protocol = self.protocol_mut();
        protocol
            .response(&mut token)
            .map_err(|_| DownloadError::ResponseFailed)?;
        while token.status == Status::NOT_READY {
            protocol.poll().map_err(|_| DownloadError::ResponseFailed)?;
        }
        let _response_header_allocation =
            FirmwareResponseHeaders::new(message.header, message.header_count);
        if token.status != Status::SUCCESS || message.body_length > chunk.len() {
            return Err(DownloadError::ResponseFailed);
        }
        body.extend_from_slice(&chunk[..message.body_length]);
        Ok(())
    }

    fn response_more_vec(&mut self) -> Result<Vec<u8>, DownloadError> {
        let mut body = Vec::new();
        self.response_more(&mut body)?;
        Ok(body)
    }

    fn protocol_mut(&mut self) -> &mut Http {
        self.protocol.as_mut().expect("HTTP protocol is open")
    }
}

struct FirmwareResponseHeaders {
    headers: Option<NonNull<HttpHeader>>,
    count: usize,
}

impl FirmwareResponseHeaders {
    fn new(headers: *mut HttpHeader, count: usize) -> Self {
        Self {
            headers: NonNull::new(headers),
            count,
        }
    }
}

impl Drop for FirmwareResponseHeaders {
    fn drop(&mut self) {
        let Some(headers) = self.headers else {
            return;
        };
        for index in 0..self.count {
            // SAFETY: EFI_HTTP_PROTOCOL returned an array of `count` response
            // headers. Each non-null field string is a pool allocation owned by
            // the caller and remains live until this guard is dropped.
            let header = unsafe { headers.as_ptr().add(index).read() };
            if let Some(field_name) = NonNull::new(header.field_name.cast_mut()) {
                // SAFETY: ownership of the response header field was
                // transferred to the caller by EFI_HTTP_PROTOCOL.
                let _ = unsafe { boot::free_pool(field_name) };
            }
            if let Some(field_value) = NonNull::new(header.field_value.cast_mut()) {
                // SAFETY: ownership of the response header field was
                // transferred to the caller by EFI_HTTP_PROTOCOL.
                let _ = unsafe { boot::free_pool(field_value) };
            }
        }
        // SAFETY: the response header array itself is a firmware pool
        // allocation transferred to the caller by EFI_HTTP_PROTOCOL.
        let _ = unsafe { boot::free_pool(headers.cast()) };
    }
}

impl Drop for HttpClient {
    fn drop(&mut self) {
        self.protocol = None;
        let _ = self.binding.destroy_child(self.child);
    }
}

fn open_protocol<P: uefi::proto::ProtocolPointer>(
    handle: Handle,
) -> uefi::Result<ScopedProtocol<P>> {
    // SAFETY: the returned scoped guard closes this exact protocol open before
    // the HTTP child or its parent network controller is destroyed.
    unsafe {
        boot::open_protocol::<P>(
            OpenProtocolParams {
                handle,
                agent: boot::image_handle(),
                controller: None,
            },
            OpenProtocolAttributes::GetProtocol,
        )
    }
}

fn url_host(url: &str) -> Result<&str, DownloadError> {
    url.split('/')
        .nth(2)
        .filter(|host| !host.is_empty())
        .ok_or(DownloadError::RequestFailed)
}

fn nul_terminated(bytes: &[u8]) -> Result<Vec<u8>, DownloadError> {
    if bytes.contains(&0) {
        return Err(DownloadError::RequestFailed);
    }
    let mut value = Vec::with_capacity(bytes.len() + 1);
    value.extend_from_slice(bytes);
    value.push(0);
    Ok(value)
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
