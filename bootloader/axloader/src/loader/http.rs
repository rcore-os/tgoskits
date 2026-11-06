//! HTTP boot routes shared by firmware network transports.

use alloc::{format, vec::Vec};

use axloader::{
    http_control::{
        MAX_HEADER_BYTES, Route, error_status, parse_head, route, validate_body_length,
    },
    integrity::decode_sha256,
    ota::OtaController,
    target::UEFI,
};
use httpboot_protocol::{
    DEVICE_PROTOCOL_VERSION, DeviceBootJob, LoaderDeviceStatus, MAX_HTTP_BOOT_INITRAMFS_BYTES,
    OtaSource,
};
use uefi::Status;

use super::boot_server::{BootExecution, BootServer, FileKind};

pub enum Action {
    None,
    Reset,
    Boot(alloc::boxed::Box<BootExecution>),
}

/// One bounded connection. A successful write must retain its source until consumed.
pub(super) trait HttpConnection {
    fn receive(&mut self, buf: &mut [u8]) -> uefi::Result<usize>;
    fn send(&mut self, buf: &[u8]) -> uefi::Result<()>;

    fn reply(&mut self, status: &str, body: &[u8]) -> uefi::Result<()> {
        let headers = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: \
             {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        self.send(headers.as_bytes())?;
        if !body.is_empty() {
            self.send(body)?;
        }
        Ok(())
    }

    fn read_body(&mut self, length: usize, initial: &[u8]) -> uefi::Result<Vec<u8>> {
        if initial.len() > length {
            return Err(Status::BAD_BUFFER_SIZE.into());
        }
        let mut body = Vec::new();
        body.try_reserve_exact(length)
            .map_err(|_| Status::OUT_OF_RESOURCES)?;
        body.extend_from_slice(initial);
        let mut chunk = [0; 8192];
        while body.len() < length {
            let limit = (length - body.len()).min(chunk.len());
            let count = self.receive(&mut chunk[..limit])?;
            if count == 0 {
                return Err(Status::END_OF_FILE.into());
            }
            body.extend_from_slice(&chunk[..count]);
        }
        Ok(body)
    }

    fn handle_http(
        &mut self,
        ota: &mut Option<OtaController>,
        boot_server: &mut BootServer,
    ) -> uefi::Result<Action> {
        let mut header = [0; MAX_HEADER_BYTES + 1];
        let mut used = 0;
        let header_end = loop {
            if used >= MAX_HEADER_BYTES {
                self.reply("431 Request Header Fields Too Large", b"{}")?;
                return Ok(Action::None);
            }
            let count = self.receive(&mut header[used..])?;
            if count == 0 {
                return Ok(Action::None);
            }
            used += count;
            if let Some(index) = header[..used]
                .windows(4)
                .position(|part| part == b"\r\n\r\n")
            {
                if index + 4 > MAX_HEADER_BYTES {
                    self.reply("431 Request Header Fields Too Large", b"{}")?;
                    return Ok(Action::None);
                }
                break index + 4;
            }
        };
        let (head, parsed_header_end) = match parse_head(&header[..used]) {
            Ok(value) => value,
            Err(error) => {
                self.reply(error_status(error), b"{}")?;
                return Ok(Action::None);
            }
        };
        debug_assert_eq!(parsed_header_end, header_end);
        let method = head.method.as_str();
        let path = head.path.as_str();
        let selected_route = route(method, path);
        let content_length = Some(head.content_length);
        let initial = &header[header_end..used];
        if let Err(error) = validate_body_length(
            method,
            path,
            head.content_length,
            initial.len(),
            MAX_HTTP_BOOT_INITRAMFS_BYTES,
        ) {
            self.reply(error_status(error), b"{}")?;
            return Ok(Action::None);
        }
        if let Err(error) = selected_route.check_admission(
            head.header("x-boot-epoch"),
            boot_server.epoch(),
            ota.as_ref().is_some_and(OtaController::trial),
        ) {
            self.reply(error.status(), error.body())?;
            return Ok(Action::None);
        }
        if selected_route == Route::DeviceStatus {
            let body = serde_json::to_vec(&LoaderDeviceStatus {
                protocol_version: DEVICE_PROTOCOL_VERSION,
                boot_epoch: boot_server.epoch().into(),
                mac_address: boot_server.mac_address(),
                current_mac_address: boot_server.current_mac_address(),
                arch: UEFI.boot_arch,
                loader_version: env!("CARGO_PKG_VERSION").into(),
                hardware: boot_server.hardware().clone(),
                boot: boot_server.status(),
                serial: Some(boot_server.serial.borrow().clone()),
                ota: ota.as_ref().map(OtaController::protocol_state),
            })
            .map_err(|_| Status::ABORTED)?;
            self.reply("200 OK", &body)?;
            return Ok(Action::None);
        }
        if selected_route == Route::OtaStatus {
            let Some(ota) = ota.as_ref() else {
                self.reply("503 Service Unavailable", b"{}")?;
                return Ok(Action::None);
            };
            self.reply(
                "200 OK",
                &serde_json::to_vec(&ota.status_json()).map_err(|_| Status::ABORTED)?,
            )?;
            return Ok(Action::None);
        }
        if let Route::BootJobStatus { id } = selected_route {
            if boot_server.descriptor(id, FileKind::Kernel).is_err() {
                self.reply("404 Not Found", b"{}")?;
            } else {
                self.reply(
                    "200 OK",
                    &serde_json::to_vec(&boot_server.status()).map_err(|_| Status::ABORTED)?,
                )?;
            }
            return Ok(Action::None);
        }
        if selected_route == Route::SerialContinue {
            let Some(length) = content_length.filter(|n| *n > 0 && *n <= 1024) else {
                self.reply("400 Bad Request", b"{}")?;
                return Ok(Action::None);
            };
            let body = self.read_body(length, initial)?;
            let binding = match serde_json::from_slice::<httpboot_protocol::SerialBinding>(&body) {
                Ok(binding) => binding,
                Err(_) => {
                    self.reply("400 Bad Request", br#"{"error":"invalid_serial_binding"}"#)?;
                    return Ok(Action::None);
                }
            };
            let result = boot_server.serial.borrow_mut().grant(binding);
            let status =
                serde_json::to_vec(&*boot_server.serial.borrow()).map_err(|_| Status::ABORTED)?;
            self.reply(
                if result.is_ok() {
                    "200 OK"
                } else {
                    "409 Conflict"
                },
                &status,
            )?;
            return Ok(Action::None);
        }
        if let Route::SerialBindingRevoke { id } = selected_route {
            let result = boot_server.serial.borrow_mut().revoke(id);
            self.reply(
                if result.is_ok() {
                    "200 OK"
                } else {
                    "409 Conflict"
                },
                b"{}",
            )?;
            return Ok(Action::None);
        }
        if let Some(ota) = ota.as_mut() {
            if selected_route == Route::OtaConfirm {
                let Some(length) = content_length.filter(|size| *size > 0 && *size <= 1024) else {
                    self.reply("400 Bad Request", b"{}")?;
                    return Ok(Action::None);
                };
                let body = self.read_body(length, initial)?;
                let value: serde_json::Value =
                    serde_json::from_slice(&body).map_err(|_| Status::INVALID_PARAMETER)?;
                let id = value
                    .get("update_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("");
                let source = match head.ota_source() {
                    Ok(source) => source,
                    Err(_) => {
                        self.reply("400 Bad Request", b"{}")?;
                        return Ok(Action::None);
                    }
                };
                if ota.confirm(id, source).is_err() {
                    self.reply("409 Conflict", b"{}")?;
                } else {
                    self.reply(
                        "200 OK",
                        &serde_json::to_vec(&ota.status_json()).map_err(|_| Status::ABORTED)?,
                    )?;
                }
                return Ok(Action::None);
            }
            if selected_route == Route::OtaImage {
                if boot_server.busy() {
                    self.reply("409 Conflict", br#"{"error":"boot_job_busy"}"#)?;
                    return Ok(Action::None);
                }
                let Some(length) = content_length
                    .filter(|size| *size > 0 && *size <= axloader::ota::MAX_IMAGE_BYTES)
                else {
                    self.reply("413 Payload Too Large", b"{}")?;
                    return Ok(Action::None);
                };
                let Some(expected) = head.ota_image_sha256() else {
                    self.reply("400 Bad Request", b"{}")?;
                    return Ok(Action::None);
                };
                if initial.len() > length {
                    self.reply("400 Bad Request", b"{}")?;
                    return Ok(Action::None);
                }
                let source = match head.ota_source() {
                    Ok(source) => source,
                    Err(_) => {
                        self.reply("400 Bad Request", b"{}")?;
                        return Ok(Action::None);
                    }
                };
                let display_version = match head.ota_image_version() {
                    Ok(version) => version,
                    Err(_) => {
                        self.reply("400 Bad Request", b"{}")?;
                        return Ok(Action::None);
                    }
                };
                let id = if source == OtaSource::Server {
                    let Some(id) = head.ota_update_id() else {
                        self.reply("400 Bad Request", b"{}")?;
                        return Ok(Action::None);
                    };
                    id
                } else {
                    ota.next_direct_id(&expected)
                };
                let Ok((disk, mut writer)) = ota.start_update(length) else {
                    self.reply("409 Conflict", b"{}")?;
                    return Ok(Action::None);
                };
                writer
                    .write(initial)
                    .inspect_err(|_| ota.record_failure("image_write_failed"))?;
                let mut written = initial.len();
                let mut chunk = [0; 8192];
                while written < length {
                    let limit = (length - written).min(chunk.len());
                    let count = self
                        .receive(&mut chunk[..limit])
                        .inspect_err(|_| ota.record_failure("image_receive_failed"))?;
                    if count == 0 {
                        ota.record_failure("short_image_upload");
                        self.reply("400 Bad Request", b"{}")?;
                        return Ok(Action::None);
                    }
                    writer
                        .write(&chunk[..count])
                        .inspect_err(|_| ota.record_failure("image_write_failed"))?;
                    written += count;
                }
                if ota
                    .finish_update(disk, writer, expected, id, source, display_version)
                    .is_err()
                {
                    ota.record_failure("image_digest_or_efi_validation_failed");
                    self.reply("422 Unprocessable Content", b"{}")?;
                    return Ok(Action::None);
                }
                self.reply(
                    "202 Accepted",
                    &serde_json::to_vec(&serde_json::json!({
                        "update_id": core::str::from_utf8(&id).unwrap_or("")
                    }))
                    .map_err(|_| Status::ABORTED)?,
                )?;
                return Ok(Action::Reset);
            }
        } else if path.starts_with("/api/v1/ota/") {
            self.reply("503 Service Unavailable", b"{}")?;
            return Ok(Action::None);
        }
        if selected_route == Route::BootJobCreate {
            let Some(length) = content_length.filter(|size| *size > 0 && *size <= MAX_HEADER_BYTES)
            else {
                self.reply("400 Bad Request", b"{}")?;
                return Ok(Action::None);
            };
            let body = self.read_body(length, initial)?;
            let manifest: DeviceBootJob = match serde_json::from_slice(&body) {
                Ok(value) => value,
                Err(_) => {
                    self.reply("400 Bad Request", b"{}")?;
                    return Ok(Action::None);
                }
            };
            let status = match boot_server.create(manifest) {
                Ok(true) => "201 Created",
                Ok(false) => "200 OK",
                Err(_) => "409 Conflict",
            };
            self.reply(
                status,
                &serde_json::to_vec(&boot_server.status()).map_err(|_| Status::ABORTED)?,
            )?;
            return Ok(Action::None);
        }
        if let Route::BootJobCancel { id } = selected_route {
            let status = if boot_server.cancel(id).is_ok() {
                "204 No Content"
            } else {
                "404 Not Found"
            };
            self.reply(status, b"")?;
            return Ok(Action::None);
        }
        if let Route::BootJobUpload { id, kind } = selected_route {
            let Ok(descriptor) = boot_server.descriptor(id, kind) else {
                self.reply("404 Not Found", b"{}")?;
                return Ok(Action::None);
            };
            if content_length != Some(descriptor.size as usize)
                || head.ota_image_sha256() != decode_sha256(&descriptor.sha256)
            {
                let _ = boot_server.cancel(id);
                self.reply("400 Bad Request", b"{}")?;
                return Ok(Action::None);
            }
            let data = match self.read_body(descriptor.size as usize, initial) {
                Ok(data) => data,
                Err(error) => {
                    let _ = boot_server.cancel(id);
                    return Err(error);
                }
            };
            let result = boot_server.upload(id, kind, data);
            self.reply(
                if result.is_ok() {
                    "200 OK"
                } else {
                    "422 Unprocessable Content"
                },
                &serde_json::to_vec(&boot_server.status()).map_err(|_| Status::ABORTED)?,
            )?;
            return Ok(Action::None);
        }
        if let Route::BootJobStart { id } = selected_route {
            match boot_server.prepare(id, head.header("x-serial-binding")) {
                Ok(execution) => {
                    self.reply("202 Accepted", br#"{"phase":"ready_to_handoff"}"#)?;
                    return Ok(Action::Boot(alloc::boxed::Box::new(execution)));
                }
                Err(error) => {
                    let body = serde_json::to_vec(&serde_json::json!({"error": error}))
                        .map_err(|_| Status::ABORTED)?;
                    self.reply("409 Conflict", &body)?;
                    return Ok(Action::None);
                }
            }
        }
        self.reply("404 Not Found", b"{}")?;
        Ok(Action::None)
    }
}
