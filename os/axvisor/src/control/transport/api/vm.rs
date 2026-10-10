//! VM lifecycle and configuration handlers.

use alloc::{
    string::{String, ToString},
    vec::Vec,
};

use axum::{
    Json,
    extract::{Path, Query},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use axvm::{AxVmError, VmSnapshot};
use axvmconfig::{GuestConfig, GuestType, VmMemConfig, VmMemMappingType};
use serde_json::{Value, json};

use crate::manager::manager;

const GUEST_RAM_FLAGS: usize = 0x7;

struct FormParams {
    id: usize,
    name: String,
    guest_type: GuestType,
    cpu_num: usize,
    entry_point: usize,
    kernel_path: String,
    kernel_load_addr: usize,
    cmdline: Option<String>,
    memory_base: usize,
    memory_bytes: usize,
}

/// `GET /api/vms` — list the manager's owned VM snapshots.
pub async fn list_vms() -> Json<Vec<Value>> {
    Json(
        manager()
            .list()
            .iter()
            .map(|vm| vm_json(&vm.snapshot(), false))
            .collect(),
    )
}

/// `GET /api/vms/{id}` — return one point-in-time snapshot.
pub async fn vm_detail(Path(id): Path<String>) -> Result<Json<Value>, StatusCode> {
    let id = parse_vm_id(id)?;
    manager()
        .get(id)
        .map(|vm| Json(vm_json(&vm.snapshot(), true)))
        .ok_or(StatusCode::NOT_FOUND)
}

/// `GET /api/vms/schema` — fields understood by the dashboard form.
pub async fn vm_schema() -> Json<Value> {
    Json(json!({
        "fields": [
            {"name": "id", "type": "integer", "required": true},
            {"name": "name", "type": "string", "required": true},
            {"name": "kernel_path", "type": "file", "required": true},
            {"name": "rootfs_path", "type": "file", "required": false},
            {"name": "entry_point", "type": "address", "required": true, "default": FORM_ENTRY_POINT},
            {"name": "kernel_load_addr", "type": "address", "required": true, "default": FORM_KERNEL_LOAD_ADDR},
            {"name": "memory_base", "type": "address", "required": true, "default": FORM_MEMORY_BASE},
            {"name": "memory_mb", "type": "integer", "required": true},
            {"name": "guest_type", "type": "enum", "required": false, "default": "virtualized", "options": ["virtualized", "passthrough"]},
            {"name": "cpu_num", "type": "integer", "required": false, "default": 1},
            {"name": "cmdline", "type": "string", "required": false, "default": FORM_CMDLINE_DEFAULT},
        ],
    }))
}

#[cfg(target_arch = "aarch64")]
const FORM_CMDLINE_DEFAULT: &str = "root=/dev/vda ro rootwait console=ttyAMA0 init=/bin/sh";
#[cfg(not(target_arch = "aarch64"))]
const FORM_CMDLINE_DEFAULT: &str = "root=/dev/vda ro rootwait console=ttyS0 init=/bin/sh";
#[cfg(target_arch = "aarch64")]
const FORM_ENTRY_POINT: &str = "0x8020_0000";
#[cfg(target_arch = "aarch64")]
const FORM_KERNEL_LOAD_ADDR: &str = "0x8020_0000";
#[cfg(target_arch = "aarch64")]
const FORM_MEMORY_BASE: &str = "0x8000_0000";
#[cfg(target_arch = "riscv64")]
const FORM_ENTRY_POINT: &str = "0x9020_0000";
#[cfg(target_arch = "riscv64")]
const FORM_KERNEL_LOAD_ADDR: &str = "0x9020_0000";
#[cfg(target_arch = "riscv64")]
const FORM_MEMORY_BASE: &str = "0x9000_0000";
#[cfg(target_arch = "x86_64")]
const FORM_ENTRY_POINT: &str = "0x8000";
#[cfg(target_arch = "x86_64")]
const FORM_KERNEL_LOAD_ADDR: &str = "0x20_0000";
#[cfg(target_arch = "x86_64")]
const FORM_MEMORY_BASE: &str = "0x0";
#[cfg(target_arch = "loongarch64")]
const FORM_ENTRY_POINT: &str = "0x0020_0040";
#[cfg(target_arch = "loongarch64")]
const FORM_KERNEL_LOAD_ADDR: &str = "0x0020_0000";
#[cfg(target_arch = "loongarch64")]
const FORM_MEMORY_BASE: &str = "0x0";

fn form_params(fields: &Value) -> Result<FormParams, String> {
    let cpu_num = fields.get("cpu_num").map_or(Ok(1), |value| {
        let cpu_num = value
            .as_u64()
            .ok_or_else(|| "`cpu_num` must be a non-negative integer".to_string())?;
        usize::try_from(cpu_num)
            .map_err(|_| "`cpu_num` exceeds the target address space".to_string())
    })?;
    if !(1..=usize::BITS as usize).contains(&cpu_num) {
        return Err(format!("`cpu_num` must be between 1 and {}", usize::BITS));
    }

    let memory_mb = fields
        .get("memory_mb")
        .and_then(Value::as_u64)
        .ok_or_else(|| "`memory_mb` is required and must be a non-negative integer".to_string())?;
    if memory_mb == 0 {
        return Err("`memory_mb` must be greater than zero".to_string());
    }
    let memory_bytes = usize::try_from(memory_mb)
        .ok()
        .and_then(|memory_mb| memory_mb.checked_mul(1024 * 1024))
        .ok_or_else(|| "`memory_mb` is too large".to_string())?;

    Ok(FormParams {
        id: usize_field(fields, "id", "`id` is required")?,
        name: text_field(fields, "name")?,
        guest_type: match fields.get("guest_type").and_then(Value::as_str) {
            None | Some("virtualized") => GuestType::Virtualized,
            Some("passthrough") => GuestType::Passthrough,
            Some(other) => return Err(format!("unknown guest type `{other}`")),
        },
        cpu_num,
        entry_point: address_field(fields, "entry_point")?,
        kernel_path: text_field(fields, "kernel_path")?,
        kernel_load_addr: address_field(fields, "kernel_load_addr")?,
        cmdline: Some(
            fields
                .get("cmdline")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(FORM_CMDLINE_DEFAULT)
                .to_string(),
        ),
        memory_base: address_field(fields, "memory_base")?,
        memory_bytes,
    })
}

fn form_config(params: FormParams, rootfs_path: Option<&str>) -> GuestConfig {
    let mut config = GuestConfig {
        base: axvmconfig::VMBaseConfig {
            id: params.id,
            name: params.name,
            guest_type: params.guest_type,
            cpu_num: params.cpu_num,
            phys_cpu_ids: Some((0..params.cpu_num).collect()),
            phys_cpu_sets: None,
        },
        kernel: axvmconfig::VMKernelConfig {
            entry_point: params.entry_point,
            kernel_path: params.kernel_path,
            kernel_load_addr: params.kernel_load_addr,
            cmdline: params.cmdline,
            memory_regions: vec![VmMemConfig {
                gpa: params.memory_base,
                size: params.memory_bytes,
                flags: GUEST_RAM_FLAGS,
                map_type: VmMemMappingType::MapAlloc,
            }],
            configured_memory_region_count: 1,
            ..Default::default()
        },
        devices: Default::default(),
    };
    if let Some(path) = rootfs_path.filter(|path| !path.is_empty()) {
        let mut options = toml::Table::new();
        options.insert("path".into(), toml::Value::String(path.into()));
        options.insert("filesystem".into(), toml::Value::String("ext4".into()));
        config
            .devices
            .virtual_devices
            .push(axvmconfig::VirtualDeviceRequest {
                id: "virtblk0".into(),
                model: "virtio-blk".into(),
                options,
            });
    }
    config
}

fn usize_field(fields: &Value, name: &str, missing: &str) -> Result<usize, String> {
    let value = fields
        .get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| missing.to_string())?;
    usize::try_from(value).map_err(|_| format!("`{name}` exceeds the target address space"))
}

fn text_field(fields: &Value, name: &str) -> Result<String, String> {
    fields
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| format!("`{name}` is required"))
}

fn address_field(fields: &Value, name: &str) -> Result<usize, String> {
    match fields.get(name) {
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
            .ok_or_else(|| format!("`{name}` must be an address")),
        Some(Value::String(text)) => {
            let text = text.trim().replace('_', "");
            let parsed = text
                .strip_prefix("0x")
                .or_else(|| text.strip_prefix("0X"))
                .map(|hex| usize::from_str_radix(hex, 16))
                .unwrap_or_else(|| text.parse::<usize>());
            parsed.map_err(|_| format!("`{name}` is not an address"))
        }
        _ => Err(format!("`{name}` is required")),
    }
}

/// `POST /api/vms/create` — create a VM from TOML or dashboard fields.
pub async fn vm_create(Json(payload): Json<Value>) -> Response {
    let raw = if let Some(fields) = payload.get("fields") {
        let params = match form_params(fields) {
            Ok(params) => params,
            Err(error) => {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
            }
        };
        let rootfs = fields
            .get("rootfs_path")
            .and_then(Value::as_str)
            .map(str::trim);
        match toml::to_string(&form_config(params, rootfs)) {
            Ok(raw) => raw,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": error.to_string()})),
                )
                    .into_response();
            }
        }
    } else if let Some(raw) = payload.get("toml").and_then(Value::as_str) {
        raw.to_string()
    } else if let Some(path) = payload.get("path").and_then(Value::as_str) {
        match ax_std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": error.to_string()})),
                )
                    .into_response();
            }
        }
    } else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    let config = match GuestConfig::from_toml(&raw) {
        Ok(config) => config,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": error.to_string()})),
            )
                .into_response();
        }
    };
    let id = config.base.id;
    if manager().get(id).is_some() {
        return StatusCode::CONFLICT.into_response();
    }
    if let Some(path) = crate::guest_images::missing_guest_image(&config) {
        return (
            StatusCode::CONFLICT,
            Json(json!({"error": format!("guest image `{path}` does not exist")})),
        )
            .into_response();
    }
    match manager().create_vm_from_toml_and_wait(&raw) {
        Ok(vm) => {
            info!("HTTP: VM[{id}] created via control API");
            Json(json!({"id": vm.vm_id()})).into_response()
        }
        Err(error) => {
            error!("HTTP: create VM[{id}] failed: {error:#}");
            map_axvm_error(error).into_response()
        }
    }
}

/// `DELETE /api/vms/{id}` — destroy the owned VM and wait for its task exit.
pub async fn vm_delete(Path(id): Path<String>) -> Result<StatusCode, StatusCode> {
    let id = parse_vm_id(id)?;
    manager().get(id).ok_or(StatusCode::NOT_FOUND)?;
    let console_backend = crate::guest_console::backend_identity(id);
    manager().destroy_vm(id).map_err(map_axvm_error)?;
    if let Some(identity) = console_backend {
        crate::guest_console::remove_if_backend(identity);
    }
    crate::guest_console::mark_stopped(id);
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/vms/pool` — list candidate configurations.
pub async fn vm_pool() -> Json<Value> {
    let pool = crate::control::domain::pool::scan();
    Json(json!({
        "directory": pool.directory(),
        "sources": pool.sources(),
        "entries": entries_json(pool.entries()),
        "issues": issues_json(pool.issues()),
    }))
}

/// `GET /api/vms/browse?path=...` — inspect one guest directory.
pub async fn vm_browse(
    Query(query): Query<alloc::collections::BTreeMap<String, String>>,
) -> Json<Value> {
    let path = query
        .get("path")
        .map(String::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| crate::control::domain::pool::directory().to_string());
    let folder = crate::control::domain::pool::browse(&path);
    Json(json!({
        "path": folder.path(),
        "parent": folder.parent(),
        "directories": folder.directories().iter().map(|entry| json!({"name": entry.name(), "path": entry.path()})).collect::<Vec<_>>(),
        "files": files_json(folder.files()),
        "entries": entries_json(folder.entries()),
        "issues": issues_json(folder.issues()),
    }))
}

/// `POST /api/vms/pool` — validate and save a candidate configuration.
pub async fn vm_pool_save(Json(payload): Json<Value>) -> Result<Json<Value>, StatusCode> {
    let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
    let raw = payload
        .get("toml")
        .and_then(Value::as_str)
        .ok_or(StatusCode::BAD_REQUEST)?;
    match crate::control::domain::pool::save(name, raw) {
        Ok(path) => Ok(Json(json!({"path": path}))),
        Err(crate::control::domain::pool::SaveError::Exists(_)) => Err(StatusCode::CONFLICT),
        Err(crate::control::domain::pool::SaveError::Unwritable(_)) => {
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
        Err(_) => Err(StatusCode::BAD_REQUEST),
    }
}

fn files_json(files: &[crate::control::domain::pool::File]) -> Vec<Value> {
    files
        .iter()
        .map(|file| json!({"name": file.name(), "path": file.path(), "size": file.size()}))
        .collect()
}
fn entries_json(entries: &[crate::control::domain::pool::Entry]) -> Vec<Value> {
    entries.iter().map(|entry| json!({"id": entry.id(), "name": entry.name(), "path": entry.path(), "source": entry.source(), "toml": entry.toml()})).collect()
}
fn issues_json(issues: &[crate::control::domain::pool::Issue]) -> Vec<Value> {
    issues.iter().map(|issue| json!({"kind": issue.kind().as_str(), "path": issue.path(), "detail": issue.kind().to_string()})).collect()
}

pub async fn vm_start(Path(id): Path<String>) -> Result<Json<Value>, StatusCode> {
    let id = parse_vm_id(id)?;
    vm_action(id, VmAction::Start)
}
pub async fn vm_stop(Path(id): Path<String>) -> Result<Json<Value>, StatusCode> {
    let id = parse_vm_id(id)?;
    vm_action(id, VmAction::Stop)
}
pub async fn vm_pause(Path(id): Path<String>) -> Result<Json<Value>, StatusCode> {
    let id = parse_vm_id(id)?;
    vm_action(id, VmAction::Pause)
}
pub async fn vm_resume(Path(id): Path<String>) -> Result<Json<Value>, StatusCode> {
    let id = parse_vm_id(id)?;
    vm_action(id, VmAction::Resume)
}

fn parse_vm_id(id: String) -> Result<usize, StatusCode> {
    id.parse().map_err(|_| StatusCode::NOT_FOUND)
}

enum VmAction {
    Start,
    Stop,
    Pause,
    Resume,
}

fn vm_action(id: usize, action: VmAction) -> Result<Json<Value>, StatusCode> {
    if matches!(action, VmAction::Start) && manager().get(id).is_none() {
        if !manager().ensure_vm_from_pool(id).map_err(map_axvm_error)? {
            return Err(StatusCode::NOT_FOUND);
        }
    }
    let vm = manager().get(id).ok_or(StatusCode::NOT_FOUND)?;
    let result = match action {
        VmAction::Start => manager().start_vm(id),
        VmAction::Stop => manager().stop_vm(id),
        VmAction::Pause => manager().pause_vm(id),
        VmAction::Resume => manager().resume_vm(id),
    };
    result.map_err(map_axvm_error)?;
    let status = vm.snapshot().state.as_str();
    Ok(Json(json!({"ok": true, "status": status, "async": false})))
}

fn map_axvm_error(error: anyhow::Error) -> StatusCode {
    match error.root_cause().downcast_ref::<AxVmError>() {
        Some(
            AxVmError::InvalidTransition { .. }
            | AxVmError::VcpuState { .. }
            | AxVmError::Backend {
                source: axvm::VmBackendError::InvalidState | axvm::VmBackendError::ResourceBusy,
                ..
            }
            | AxVmError::InvalidState { .. }
            | AxVmError::ResourceConflict { .. }
            | AxVmError::EntryClosed { .. }
            | AxVmError::StaleRun { .. },
        ) => StatusCode::CONFLICT,
        Some(
            AxVmError::InvalidInput { .. }
            | AxVmError::InvalidConfig { .. }
            | AxVmError::Backend {
                source: axvm::VmBackendError::InvalidInput,
                ..
            },
        ) => StatusCode::BAD_REQUEST,
        Some(AxVmError::OutOfMemory { .. } | AxVmError::ResourceUnavailable { .. }) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        Some(AxVmError::OperationCancelled { .. }) => StatusCode::SERVICE_UNAVAILABLE,
        Some(AxVmError::VmNotFound { .. }) => StatusCode::NOT_FOUND,
        Some(error) => {
            error!("management HTTP action failed: {error}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
        None => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn vm_json(snapshot: &VmSnapshot, with_vcpus: bool) -> Value {
    let mut value = json!({
        "id": snapshot.vm_id,
        "name": snapshot.name,
        "status": snapshot.state.as_str(),
        "cpu_num": snapshot.cpu.vcpu_num,
        "memory_mb": snapshot.memory.total_bytes / (1024 * 1024),
    });
    if with_vcpus {
        value["vcpu_states"] = json!(
            snapshot
                .vcpu
                .iter()
                .map(|vcpu| json!({
                    "id": vcpu.id,
                    "state": format!("{:?}", vcpu.state).to_lowercase(),
                    "phys_cpu_set": vcpu.phys_cpu_set,
                }))
                .collect::<Vec<_>>()
        );
        value["guest_entry_count"] = json!(snapshot.entry_count);
        value["guest_park_count"] = json!(snapshot.park_count);
    }
    value
}
