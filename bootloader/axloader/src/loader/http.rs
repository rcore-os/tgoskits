//! HTTP boot routes shared by firmware network transports.

use alloc::{boxed::Box, format, vec::Vec};

use axloader::{
    boot_job::operation_error,
    http_control::{
        MAX_HEADER_BYTES, Route, commit_after_successful_write, error_status, operation_error_json,
        parse_head, qemu_202_write_failure, route, validate_body_length, validate_request_protocol,
    },
    integrity::decode_sha256,
    ota::OtaController,
    target::CURRENT,
};
use httpboot_protocol::{
    DEVICE_PROTOCOL_VERSION, DeviceBootJob, DeviceOperationError, LoaderDeviceStatus,
    MAX_HTTP_BOOT_INITRAMFS_BYTES, OtaSource,
};
use uefi::Status;

use super::boot_server::{BootExecution, BootServer};

/// UEFI TCP4 reports peer FIN as implementation status `EFI_CONNECTION_FIN`.
const TCP4_CONNECTION_FIN: Status = Status(Status::ERROR_BIT | 0x68);

pub enum Action {
    None,
    Reset,
    Boot(alloc::boxed::Box<BootExecution>),
}

fn injected_202_write_error() -> uefi::Error {
    Status::ABORTED.into()
}

/// One bounded connection. A successful write retains its source until consumed.
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

    fn reply_error(&mut self, error: &DeviceOperationError) -> uefi::Result<()> {
        let body = operation_error_json(error).map_err(|_| Status::ABORTED)?;
        self.reply(error_status(&error.code), &body)
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
                self.reply_error(&operation_error(
                    "request_header_too_large",
                    "HTTP request header exceeds the control-plane limit",
                ))?;
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
                    self.reply_error(&operation_error(
                        "request_header_too_large",
                        "HTTP request header exceeds the control-plane limit",
                    ))?;
                    return Ok(Action::None);
                }
                break index + 4;
            }
        };
        let (head, parsed_header_end) = match parse_head(&header[..used]) {
            Ok(value) => value,
            Err(error) => {
                self.reply_error(&operation_error("invalid_request", error))?;
                return Ok(Action::None);
            }
        };
        debug_assert_eq!(parsed_header_end, header_end);
        if let Err(error) = validate_request_protocol(&head) {
            self.reply_error(&error)?;
            return Ok(Action::None);
        }
        let method = head.method.as_str();
        let path = head.path.as_str();
        let selected_route = route(method, path);
        let content_length = Some(head.content_length);
        let initial = &header[header_end..used];
        if let Err(error) = selected_route.check_admission(
            head.header("x-boot-epoch"),
            boot_server.epoch(),
            ota.as_ref().is_some_and(OtaController::trial),
        ) {
            self.reply(error.status(), error.body())?;
            return Ok(Action::None);
        }
        let upload_size = if let Route::BootJobUpload { id, kind } = selected_route {
            let descriptor = match boot_server.descriptor(id, kind) {
                Ok(descriptor) => descriptor,
                Err(error) => {
                    self.reply_error(&error)?;
                    return Ok(Action::None);
                }
            };
            let expected_size = descriptor.size;
            let expected_digest = decode_sha256(&descriptor.sha256);
            if content_length != Some(expected_size as usize) {
                let error = operation_error(
                    "image_size_mismatch",
                    format!(
                        "upload length {:?} does not match declared length {}",
                        content_length, expected_size
                    ),
                );
                let _ = boot_server.fail(id, error.clone());
                self.reply_error(&error)?;
                return Ok(Action::None);
            }
            if head.ota_image_sha256() != expected_digest {
                let error = operation_error(
                    "image_digest_mismatch",
                    "X-Image-Sha256 does not match the boot manifest",
                );
                let _ = boot_server.fail(id, error.clone());
                self.reply_error(&error)?;
                return Ok(Action::None);
            }
            Some(expected_size as usize)
        } else {
            None
        };
        if let Route::BootJobStart { id } = selected_route {
            match boot_server.failure(id) {
                Ok(Some(error)) => {
                    self.reply_error(error)?;
                    return Ok(Action::None);
                }
                Err(error) => {
                    self.reply_error(&error)?;
                    return Ok(Action::None);
                }
                Ok(None) => {}
            }
        }
        if let (Route::BootJobUpload { id, .. }, Some(expected_size)) =
            (selected_route, upload_size)
            && initial.len() > expected_size
        {
            let error = operation_error(
                "image_size_mismatch",
                format!(
                    "upload exceeded declared length {expected_size}: received at least {}",
                    initial.len()
                ),
            );
            let _ = boot_server.fail(id, error.clone());
            self.reply_error(&error)?;
            return Ok(Action::None);
        }
        if let Err(error) = validate_body_length(
            method,
            path,
            head.content_length,
            initial.len(),
            MAX_HTTP_BOOT_INITRAMFS_BYTES,
        ) {
            self.reply_error(&operation_error(error, error))?;
            return Ok(Action::None);
        }
        if selected_route == Route::DeviceStatus {
            let body = serde_json::to_vec(&LoaderDeviceStatus {
                protocol_version: DEVICE_PROTOCOL_VERSION,
                boot_epoch: boot_server.epoch().into(),
                mac_address: boot_server.mac_address(),
                current_mac_address: boot_server.current_mac_address(),
                arch: CURRENT.arch_name.into(),
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
                self.reply_error(&operation_error(
                    "ota_unavailable",
                    "OTA control is unavailable from this loader",
                ))?;
                return Ok(Action::None);
            };
            self.reply(
                "200 OK",
                &serde_json::to_vec(&ota.status_json()).map_err(|_| Status::ABORTED)?,
            )?;
            return Ok(Action::None);
        }
        if let Route::BootJobStatus { id } = selected_route {
            match boot_server.status_for(id) {
                Ok(status) => self.reply(
                    "200 OK",
                    &serde_json::to_vec(&status).map_err(|_| Status::ABORTED)?,
                )?,
                Err(error) => self.reply_error(&error)?,
            }
            return Ok(Action::None);
        }
        if selected_route == Route::SerialContinue {
            let Some(length) = content_length.filter(|n| *n > 0 && *n <= 1024) else {
                self.reply_error(&operation_error(
                    "invalid_serial_binding",
                    "serial binding body must be 1-1024 bytes",
                ))?;
                return Ok(Action::None);
            };
            let body = self.read_body(length, initial)?;
            let binding = match serde_json::from_slice::<httpboot_protocol::SerialBinding>(&body) {
                Ok(binding) => binding,
                Err(_) => {
                    self.reply_error(&operation_error(
                        "invalid_serial_binding",
                        "serial binding JSON is invalid",
                    ))?;
                    return Ok(Action::None);
                }
            };
            let result = boot_server.serial.borrow_mut().grant(binding);
            let status =
                serde_json::to_vec(&*boot_server.serial.borrow()).map_err(|_| Status::ABORTED)?;
            if result.is_ok() {
                self.reply("200 OK", &status)?;
            } else {
                self.reply_error(&operation_error(
                    "serial_binding_conflict",
                    "serial binding is already owned by another request",
                ))?;
            }
            return Ok(Action::None);
        }
        if let Route::SerialBindingRevoke { id } = selected_route {
            let result = boot_server.serial.borrow_mut().revoke(id);
            if result.is_ok() {
                self.reply("200 OK", b"{}")?;
            } else {
                self.reply_error(&operation_error(
                    "serial_binding_conflict",
                    "serial binding does not match the active owner",
                ))?;
            }
            return Ok(Action::None);
        }
        if let Some(ota) = ota.as_mut() {
            if selected_route == Route::OtaConfirm {
                let Some(length) = content_length.filter(|size| *size > 0 && *size <= 1024) else {
                    self.reply_error(&operation_error(
                        "invalid_ota_confirmation",
                        "OTA confirmation body must be 1-1024 bytes",
                    ))?;
                    return Ok(Action::None);
                };
                let body = self.read_body(length, initial)?;
                let value: serde_json::Value = match serde_json::from_slice(&body) {
                    Ok(value) => value,
                    Err(_) => {
                        self.reply_error(&operation_error(
                            "invalid_ota_confirmation",
                            "OTA confirmation JSON is invalid",
                        ))?;
                        return Ok(Action::None);
                    }
                };
                let id = value
                    .get("update_id")
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| !id.is_empty());
                let Some(id) = id else {
                    self.reply_error(&operation_error(
                        "invalid_ota_confirmation",
                        "OTA confirmation requires a non-empty update_id",
                    ))?;
                    return Ok(Action::None);
                };
                let source = match head.ota_source() {
                    Ok(source) => source,
                    Err(_) => {
                        self.reply_error(&operation_error(
                            "invalid_ota_source",
                            "OTA update source is invalid",
                        ))?;
                        return Ok(Action::None);
                    }
                };
                if ota.confirm(id, source).is_err() {
                    self.reply_error(&operation_error(
                        "ota_confirmation_conflict",
                        "OTA update ID or source does not match the pending trial",
                    ))?;
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
                    self.reply_error(&operation_error(
                        "boot_job_busy",
                        "a boot job is active during OTA upload",
                    ))?;
                    return Ok(Action::None);
                }
                let Some(length) = content_length
                    .filter(|size| *size > 0 && *size <= axloader::ota::MAX_IMAGE_BYTES)
                else {
                    self.reply_error(&operation_error(
                        "image_too_large",
                        "OTA image is empty or exceeds the device limit",
                    ))?;
                    return Ok(Action::None);
                };
                let Some(expected) = head.ota_image_sha256() else {
                    self.reply_error(&operation_error(
                        "invalid_ota_image",
                        "X-Image-Sha256 is missing or invalid",
                    ))?;
                    return Ok(Action::None);
                };
                if initial.len() > length {
                    self.reply_error(&operation_error(
                        "invalid_request",
                        "request body exceeds Content-Length",
                    ))?;
                    return Ok(Action::None);
                }
                let source = match head.ota_source() {
                    Ok(source) => source,
                    Err(_) => {
                        self.reply_error(&operation_error(
                            "invalid_ota_source",
                            "OTA update source is invalid",
                        ))?;
                        return Ok(Action::None);
                    }
                };
                let display_version = match head.ota_image_version() {
                    Ok(version) => version,
                    Err(_) => {
                        self.reply_error(&operation_error(
                            "invalid_ota_image_version",
                            "OTA image version is invalid",
                        ))?;
                        return Ok(Action::None);
                    }
                };
                let id = if source == OtaSource::Server {
                    let Some(id) = head.ota_update_id() else {
                        self.reply_error(&operation_error(
                            "invalid_ota_update_id",
                            "server OTA upload requires a valid X-Update-ID",
                        ))?;
                        return Ok(Action::None);
                    };
                    id
                } else {
                    ota.next_direct_id(&expected)
                };
                let Ok((disk, mut writer)) = ota.start_update(length) else {
                    self.reply_error(&operation_error(
                        "ota_state_conflict",
                        "OTA storage is not ready for another update",
                    ))?;
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
                        self.reply_error(&operation_error(
                            "image_receive_failed",
                            "OTA upload ended before Content-Length bytes",
                        ))?;
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
                    self.reply_error(&operation_error(
                        "invalid_ota_image",
                        "OTA image digest, checksum or EFI structure is invalid",
                    ))?;
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
            self.reply_error(&operation_error(
                "ota_unavailable",
                "OTA control is unavailable from this loader",
            ))?;
            return Ok(Action::None);
        }
        if selected_route == Route::BootJobCreate {
            let Some(length) = content_length.filter(|size| *size > 0 && *size <= MAX_HEADER_BYTES)
            else {
                self.reply_error(&operation_error(
                    "invalid_boot_manifest",
                    "boot manifest body is empty or too large",
                ))?;
                return Ok(Action::None);
            };
            let body = self.read_body(length, initial)?;
            let manifest: DeviceBootJob = match serde_json::from_slice(&body) {
                Ok(value) => value,
                Err(_) => {
                    self.reply_error(&operation_error(
                        "invalid_boot_manifest",
                        "boot manifest JSON does not match the v7 schema",
                    ))?;
                    return Ok(Action::None);
                }
            };
            let boot_id = manifest.boot_id.clone();
            match boot_server.create(manifest) {
                Ok(created) => {
                    let status = boot_server
                        .status_for(&boot_id)
                        .map_err(|_| Status::ABORTED)?;
                    self.reply(
                        if created { "201 Created" } else { "200 OK" },
                        &serde_json::to_vec(&status).map_err(|_| Status::ABORTED)?,
                    )?;
                }
                Err(error) => self.reply_error(&error)?,
            }
            return Ok(Action::None);
        }
        if let Route::BootJobCancel { id } = selected_route {
            match boot_server.cancel(id) {
                Ok(()) => self.reply("204 No Content", b"")?,
                Err(error) => self.reply_error(&error)?,
            }
            return Ok(Action::None);
        }
        if let Route::BootJobUpload { id, kind } = selected_route {
            let expected_size = upload_size.expect("upload preflight produced a descriptor size");
            let data = match self.read_body(expected_size, initial) {
                Ok(data) => data,
                Err(error) => {
                    let status = error.status();
                    if matches!(
                        status,
                        Status::END_OF_FILE | TCP4_CONNECTION_FIN | Status::BAD_BUFFER_SIZE
                    ) {
                        let reason = if matches!(status, Status::END_OF_FILE | TCP4_CONNECTION_FIN)
                        {
                            format!("upload ended before declared length {expected_size}")
                        } else {
                            format!("upload exceeded declared length {expected_size}")
                        };
                        let error = operation_error("image_size_mismatch", reason);
                        let _ = boot_server.fail(id, error.clone());
                        self.reply_error(&error)?;
                        return Ok(Action::None);
                    }
                    if status == Status::OUT_OF_RESOURCES {
                        let error = operation_error(
                            "image_load_failed",
                            "insufficient resources while receiving image upload",
                        );
                        let _ = boot_server.fail(id, error.clone());
                        self.reply_error(&error)?;
                        return Ok(Action::None);
                    }
                    let _ = boot_server.fail(
                        id,
                        operation_error(
                            "image_receive_failed",
                            format!("failed to receive complete upload: {error:?}"),
                        ),
                    );
                    return Err(error);
                }
            };
            match boot_server.upload(id, kind, data) {
                Ok(()) => {
                    let status = boot_server.status_for(id).map_err(|_| Status::ABORTED)?;
                    self.reply(
                        "200 OK",
                        &serde_json::to_vec(&status).map_err(|_| Status::ABORTED)?,
                    )?;
                }
                Err(error) => self.reply_error(&error)?,
            }
            return Ok(Action::None);
        }
        if let Route::BootJobStart { id } = selected_route {
            match boot_server.prepare(id, head.header("x-serial-binding")) {
                Ok(execution) => {
                    let action = Action::Boot(Box::new(execution));
                    let injected_error =
                        qemu_202_write_failure(id).then_some(injected_202_write_error);
                    return commit_after_successful_write(
                        || self.reply("202 Accepted", br#"{"phase":"ready_to_handoff"}"#),
                        action,
                        injected_error,
                    );
                }
                Err(error) => {
                    self.reply_error(&error)?;
                    return Ok(Action::None);
                }
            }
        }
        self.reply_error(&operation_error(
            "not_found",
            "control endpoint does not exist",
        ))?;
        Ok(Action::None)
    }
}
