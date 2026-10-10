//! VM observations and asynchronous lifecycle handlers.

use axum::{Json, extract::Path, http::StatusCode};
use axvm::{AxVmError, StopReason, VmHandle, VmSnapshot, VmVcpuState};
use serde_json::{Value, json};

use crate::{http::auth::ApiToken, manager::manager};

pub async fn list_vms() -> Json<Vec<Value>> {
    Json(
        manager()
            .list()
            .iter()
            .map(|vm| vm_json(&vm.snapshot(), false))
            .collect(),
    )
}

pub async fn vm_detail(Path(id): Path<String>) -> Result<Json<Value>, StatusCode> {
    let vm = find_vm(&id)?;
    Ok(Json(vm_json(&vm.snapshot(), true)))
}

pub async fn vm_create(
    _token: ApiToken,
    Json(payload): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    let raw = payload
        .get("toml")
        .and_then(Value::as_str)
        .ok_or(StatusCode::BAD_REQUEST)?
        .to_owned();
    let config = axvmconfig::GuestConfig::from_toml(&raw).map_err(|_| StatusCode::BAD_REQUEST)?;
    // Reject a duplicate before asynchronous boot preparation reads any guest
    // image. The API contract exposes an existing VM as a conflict even when
    // the submitted replacement payload references an unavailable image.
    if manager().get(config.base.id).is_some() {
        return Err(StatusCode::CONFLICT);
    }
    // Boot preparation can read image files; keep it off the HTTP reactor.
    let operation = tokio::task::spawn_blocking(move || manager().create_vm_from_toml(&raw))
        .await
        .map_err(|error| {
            error!("HTTP VM preparation task failed: {error}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .map_err(map_management_error)?;
    let vm = operation.await.map_err(map_axvm_error)?;
    let id = vm.key().vm_id();
    info!("HTTP: VM[{id}] created via control API");
    Ok(Json(json!({ "id": id })))
}

pub async fn vm_delete(_token: ApiToken, Path(id): Path<String>) -> Result<StatusCode, StatusCode> {
    let vm = find_vm(&id)?;
    let id = vm.key().vm_id();
    let backend = crate::guest_console::backend_identity(id);
    vm.destroy()
        .map_err(map_axvm_error)?
        .await
        .map_err(map_axvm_error)?;
    if let Some(backend) = backend {
        crate::guest_console::remove_if_backend(backend);
    }
    info!("HTTP: VM[{id}] removed via control API");
    Ok(StatusCode::NO_CONTENT)
}

pub async fn vm_start(_token: ApiToken, Path(id): Path<String>) -> Result<Json<Value>, StatusCode> {
    vm_action(&id, VmAction::Start).await
}

pub async fn vm_stop(_token: ApiToken, Path(id): Path<String>) -> Result<Json<Value>, StatusCode> {
    vm_action(&id, VmAction::Stop).await
}

pub async fn vm_pause(_token: ApiToken, Path(id): Path<String>) -> Result<Json<Value>, StatusCode> {
    vm_action(&id, VmAction::Pause).await
}

pub async fn vm_resume(
    _token: ApiToken,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    vm_action(&id, VmAction::Resume).await
}

#[derive(Clone, Copy)]
enum VmAction {
    Start,
    Stop,
    Pause,
    Resume,
}

async fn vm_action(id: &str, action: VmAction) -> Result<Json<Value>, StatusCode> {
    let vm = find_vm(id)?;
    match action {
        VmAction::Start => {
            vm.start()
                .map_err(map_axvm_error)?
                .await
                .map_err(map_axvm_error)?;
        }
        VmAction::Resume => {
            vm.resume()
                .map_err(map_axvm_error)?
                .await
                .map_err(map_axvm_error)?;
        }
        VmAction::Pause => {
            let operation = vm.pause().map_err(map_axvm_error)?;
            operation.accepted().await.map_err(map_axvm_error)?;
        }
        VmAction::Stop => {
            let operation = vm.stop(StopReason::Forced).map_err(map_axvm_error)?;
            operation.accepted().await.map_err(map_axvm_error)?;
        }
    }
    Ok(Json(json!({
        "ok": true,
        "status": vm.snapshot().state.as_str(),
        "async": matches!(action, VmAction::Stop | VmAction::Pause),
    })))
}

fn find_vm(id: &str) -> Result<VmHandle, StatusCode> {
    let id = id.parse().map_err(|_| StatusCode::NOT_FOUND)?;
    manager().get(id).ok_or(StatusCode::NOT_FOUND)
}

fn map_management_error(error: anyhow::Error) -> StatusCode {
    if let Some(error) = error.downcast_ref::<AxVmError>() {
        return map_axvm_error(error.clone());
    }
    error!("management HTTP action failed: {error:#}");
    StatusCode::INTERNAL_SERVER_ERROR
}

fn map_axvm_error(error: AxVmError) -> StatusCode {
    match error {
        AxVmError::InvalidTransition { .. }
        | AxVmError::VcpuState { .. }
        | AxVmError::Backend {
            source: axvm::VmBackendError::InvalidState | axvm::VmBackendError::ResourceBusy,
            ..
        }
        | AxVmError::InvalidState { .. }
        | AxVmError::ResourceConflict { .. }
        | AxVmError::EntryClosed { .. }
        | AxVmError::StaleRun { .. } => StatusCode::CONFLICT,
        AxVmError::InvalidInput { .. }
        | AxVmError::InvalidConfig { .. }
        | AxVmError::Backend {
            source: axvm::VmBackendError::InvalidInput,
            ..
        } => StatusCode::BAD_REQUEST,
        AxVmError::OutOfMemory { .. }
        | AxVmError::ResourceUnavailable { .. }
        | AxVmError::OperationCancelled { .. } => StatusCode::SERVICE_UNAVAILABLE,
        AxVmError::VmNotFound { .. } => StatusCode::NOT_FOUND,
        error => {
            error!("management HTTP action failed: {error}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

fn vm_json(snapshot: &VmSnapshot, with_vcpus: bool) -> Value {
    let mut result = json!({
        "id": snapshot.vm_id,
        "name": snapshot.name,
        "status": snapshot.state.as_str(),
        "cpu_num": snapshot.cpu.vcpu_num,
        "memory_mb": snapshot.memory.total_bytes / (1024 * 1024),
    });
    if with_vcpus {
        result["vcpu_states"] = json!(
            snapshot
                .vcpu
                .iter()
                .map(|vcpu| json!({
                    "id": vcpu.id,
                    "state": vcpu_state_str(vcpu.state),
                    "phys_cpu_set": vcpu.phys_cpu_set,
                }))
                .collect::<Vec<_>>()
        );
        result["guest_entry_count"] = json!(snapshot.entry_count);
        result["guest_park_count"] = json!(snapshot.park_count);
    }
    result
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
