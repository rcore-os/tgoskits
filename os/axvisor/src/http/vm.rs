//! VM status, lifecycle, and create/delete axum handlers.
//!
//! JSON is built with `serde_json::json!()` (no hand-written escaping). These
//! handlers are dispatched by the TCP serving path in [`super::server`].

use axum::{Json, extract::Path, http::StatusCode};
use axvm::{AxVMRef, AxVmError, VmStatus, VmVcpuState};
use axvmconfig::GuestConfig;
use serde_json::{Value, json};

use crate::http::auth::ApiToken;
use crate::manager::AxvmManager;

/// `GET /api/vms` — list all known VMs (summary form).
pub async fn list_vms() -> Json<Vec<Value>> {
    let items: Vec<Value> = AxvmManager::vm_list().iter().map(vm_json_summary).collect();
    Json(items)
}

/// `GET /api/vms/{id}` — detail for one VM, or 404 if unknown.
pub async fn vm_detail(Path(id_str): Path<String>) -> Result<Json<Value>, StatusCode> {
    let Ok(id) = id_str.parse::<usize>() else {
        return Err(StatusCode::NOT_FOUND);
    };
    match AxvmManager::vm_by_id(id) {
        Some(vm) => Ok(Json(vm_json(&vm, true))),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// `POST /api/vms/create` — create a VM from a TOML config in the JSON body.
///
/// Body: `{"toml": "<完整 TOML 配置>"}`. The guest kernel must be a build-time
/// embedded image (`image_location = "memory"`) whose id matches the config's
/// `base.id`, and that id must not currently be registered. Because embedded
/// images are matched by id (`memory_images_for_vm`), a config whose id has no
/// embedded image fails with 500 — the runtime can only realize guest images
/// that were baked into the hypervisor at build time.
pub async fn vm_create(
    _token: ApiToken,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let toml = payload
        .get("toml")
        .and_then(Value::as_str)
        .ok_or(StatusCode::BAD_REQUEST)?;
    let config = GuestConfig::from_toml(toml).map_err(|_| StatusCode::BAD_REQUEST)?;
    let id = config.base.id;
    // Explicit duplicate check: `create_vm_from_toml` fails on a re-registered id
    // with a plain anyhow string, so surface the conflict as a contract error
    // (409) instead of an opaque 500.
    if AxvmManager::vm_by_id(id).is_some() {
        return Err(StatusCode::CONFLICT);
    }
    match AxvmManager::create_vm_from_toml(toml) {
        Ok(id) => {
            info!("HTTP: VM[{id}] created via control API");
            Ok(Json(json!({ "id": id })))
        }
        Err(error) => {
            error!("HTTP: create VM[{id}] failed: {error:#}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// `DELETE /api/vms/{id}` — destroy and unregister a VM.
///
/// Two explicit steps so a failed destroy stays retryable: `destroy()` first
/// (its result is checked), and the registry is only touched on success. This
/// avoids relying on `Drop`-time destroy, which merely warns on failure after
/// the VM is already unregistered, leaving no handle to retry with.
pub async fn vm_delete(
    _token: ApiToken,
    Path(id_str): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let Ok(id) = id_str.parse::<usize>() else {
        return Err(StatusCode::NOT_FOUND);
    };
    let vm = AxvmManager::vm_by_id(id).ok_or(StatusCode::NOT_FOUND)?;
    // `destroy()`'s shared quiesce path carries the start->stop vCPU-entry
    // guard, so a DELETE arriving right after `/start` waits for the first vCPU
    // to enter the guest run loop instead of stranding it.
    vm.destroy()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    AxvmManager::remove_vm(id).ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    info!("HTTP: VM[{id}] removed via control API");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/vms/{id}/start` — start a VM.
pub async fn vm_start(
    _token: ApiToken,
    Path(id_str): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    vm_action(&id_str, VmAction::Start)
}

/// `POST /api/vms/{id}/stop` — request a VM stop.
///
/// `stop` has request semantics: it returns as soon as the request is accepted,
/// while the vCPU exits and the VM reaches `Stopped` asynchronously.
pub async fn vm_stop(
    _token: ApiToken,
    Path(id_str): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    vm_action(&id_str, VmAction::Stop)
}

/// A lifecycle action on a VM.
enum VmAction {
    Start,
    Stop,
}

/// Drive one lifecycle action, mapping host errors to HTTP status codes.
///
/// Unknown VMs yield 404, invalid lifecycle transitions yield 409, and host
/// resource exhaustion yields 503.
fn vm_action(id_str: &str, action: VmAction) -> Result<Json<Value>, StatusCode> {
    let Ok(id) = id_str.parse::<usize>() else {
        return Err(StatusCode::NOT_FOUND);
    };
    // No existence pre-check: an unknown VM surfaces as `VmNotFound` from the
    // action and maps to 404 below, keeping the check-then-act window closed.
    // Restart-after-stop is not supported: a fresh vCPU task on an idled pinned
    // CPU is never scheduled (no IPI wake source), so `start_vm` would accept
    // the start and leave the VM stuck in `Running`. Reject it explicitly so the
    // limitation is a contract error rather than an implicit hang.
    if matches!(action, VmAction::Start)
        && AxvmManager::vm_by_id(id).is_some_and(|vm| vm.status() == VmStatus::Stopped)
    {
        return Err(StatusCode::CONFLICT);
    }
    let result = match action {
        VmAction::Start => AxvmManager::start_vm(id),
        VmAction::Stop => AxvmManager::stop_vm(id),
    };
    match result {
        Ok(()) => Ok(Json(vm_action_json(id, action))),
        Err(error) => Err(map_axvm_error(error)),
    }
}

/// Report the VM status right after a lifecycle action was accepted.
///
/// `stop` is a request: the `Stopped` state arrives only once the vCPU observes
/// the request and exits asynchronously, so the reported status may still be
/// `running`/`stopping`. The `"async": true` marker makes that explicit so
/// callers do not mistake the accepted-request response for a completed stop.
fn vm_action_json(id: usize, action: VmAction) -> Value {
    let status = AxvmManager::vm_by_id(id)
        .map(|vm| vm.status().as_str())
        .unwrap_or("unknown");
    json!({
        "ok": true,
        "status": status,
        "async": matches!(action, VmAction::Stop),
    })
}

/// Map an AxVM runtime error to an HTTP status code.
fn map_axvm_error(error: anyhow::Error) -> StatusCode {
    let cause = error.root_cause();
    match cause.downcast_ref::<AxVmError>() {
        // A lifecycle transition that the current state does not allow.
        Some(AxVmError::InvalidTransition { .. } | AxVmError::InvalidState { .. }) => {
            StatusCode::CONFLICT
        }
        // Host resources (memory, vCPU list, devices, ...) were unavailable.
        Some(AxVmError::OutOfMemory { .. } | AxVmError::ResourceUnavailable { .. }) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        // Unknown VMs surface as `VmNotFound` from the action (there is no
        // existence pre-check), mapping to 404. Anything else is a host-side
        // fault.
        Some(AxVmError::VmNotFound { .. }) => StatusCode::NOT_FOUND,
        _ => {
            error!("management HTTP action failed: {error:#}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

fn vm_json_summary(vm: &AxVMRef) -> Value {
    vm_json(vm, false)
}

fn vm_json(vm: &AxVMRef, with_vcpus: bool) -> Value {
    let memory_mb = vm
        .memory_regions()
        .iter()
        .fold(0usize, |acc, region| acc.saturating_add(region.size()))
        / (1024 * 1024);
    let mut json = json!({
        "id": vm.id(),
        "name": vm.name(),
        "status": vm.status().as_str(),
        "cpu_num": vm.vcpu_num(),
        "memory_mb": memory_mb,
    });
    if with_vcpus {
        let vcpus: Vec<Value> = vm
            .vcpu_snapshots()
            .iter()
            .map(|vcpu| {
                json!({
                    "id": vcpu.id,
                    "state": vcpu_state_str(vcpu.state),
                    "phys_cpu_set": vcpu.phys_cpu_set,
                })
            })
            .collect();
        json["vcpu_states"] = json!(vcpus);
    }
    json
}

fn vcpu_state_str(state: VmVcpuState) -> &'static str {
    match state {
        VmVcpuState::Invalid => "invalid",
        VmVcpuState::Created => "created",
        VmVcpuState::Free => "free",
        VmVcpuState::Ready => "ready",
        VmVcpuState::Running => "running",
        VmVcpuState::Blocked => "blocked",
        VmVcpuState::Starting => "starting",
    }
}
