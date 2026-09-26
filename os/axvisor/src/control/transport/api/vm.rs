//! VM status, lifecycle, and create/delete axum handlers.
//!
//! JSON is built with `serde_json::json!()` (no hand-written escaping). These
//! handlers are dispatched by the TCP serving path in [`crate::control::transport::server`].

#[cfg(feature = "fs")]
use alloc::collections::BTreeMap;
use alloc::string::ToString;

#[cfg(feature = "fs")]
use axum::extract::Query;
use axum::response::{IntoResponse, Response};
use axum::{Json, extract::Path, http::StatusCode};
use axvm::{AxVMRef, AxVmError, VmStatus, VmVcpuState};
use axvmconfig::{
    GuestConfig, GuestType,
    templates::{VmTemplateParams, get_vm_config_template},
};
use serde_json::{Value, json};

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

/// `POST /api/vms/create` — create a VM from a TOML config.
///
/// Body: `{"toml": "<完整 TOML 配置>"}` or `{"path": "<guest 文件系统中的 .toml>"}`.
/// The path form is what the folder browser uses: the file is read on the host
/// and the VM is created from exactly the bytes that are there, so a config can
/// live in any directory instead of being pasted. The guest kernel is read from
/// the guest filesystem (`image_location = "fs"`, the only supported source), and
/// the config's `base.id` must not currently be registered. An exhausted host
/// `GET /api/vms/schema` — the fields a creation request may carry.
///
/// The field set is the template's, not this interface's: `VmTemplateParams` is
/// where a guest configuration is built from parameters — the `axvmconfig`
/// command line tool calls the same function — so the dashboard never keeps a
/// second copy of which fields a guest has. `required` is the difference between
/// a field a request must carry and one the template fills; the addresses and the
/// memory size are required because they belong to the guest image and the
/// machine it is written for, not to this platform. A `file` field is one whose
/// value names a file in the guest filesystem: a client can only offer the files
/// that are there ([`Self::vm_browse`]), which is what makes "transfer it before
/// submitting" the same fact as "the file is in the listing".
///
/// `description` and `example` travel with the declaration because they are the
/// field's own documentation: a dashboard that explains a field in the plane's
/// words cannot drift from what the plane actually does with it. `image_location`
/// offers `fs` only — a form-made guest reads its kernel from the guest
/// filesystem, and an embedded (`memory`) kernel is a build-time fact no form
/// can provide.
pub async fn vm_schema() -> Json<Value> {
    Json(json!({
        "fields": [
            {"name": "id", "type": "integer", "required": true,
             "description": "客户机的数字标识，注册表里必须唯一；候选配置里已有的 id 也不能重复。",
             "example": 2},
            {"name": "name", "type": "string", "required": true,
             "description": "显示用的名字，随意取，方便在列表里认出它。",
             "example": "linux-demo"},
            {"name": "kernel_path", "type": "file", "required": true,
             "description": "内核镜像在客户机文件系统里的路径。必须是已经就位的文件：可以在这一行直接传，或从「选择已就位」里挑一个。",
             "example": "/guest/linux/linux-qemu"},
            {"name": "image_location", "type": "enum", "required": false,
             "default": "fs", "options": ["fs"],
             "description": "内核从哪里来。表单创建的客户机只支持 fs：从客户机文件系统读上面那个内核文件。"},
            {"name": "entry_point", "type": "address", "required": true,
             "description": "CPU 从哪个客户机物理地址开始执行内核，要和内核自己链接的入口一致；QEMU virt 上的 Linux 惯例是 0x8020_0000。",
             "example": "0x8020_0000"},
            {"name": "kernel_load_addr", "type": "address", "required": true,
             "description": "内核镜像被装载到的客户机物理地址，通常与入口地址相同。",
             "example": "0x8020_0000"},
            {"name": "memory_base", "type": "address", "required": true,
             "description": "客户机内存的起始地址（客户机物理地址）。QEMU virt 从 0x8000_0000 开始划分客户机内存。",
             "example": "0x8000_0000"},
            {"name": "memory_mb", "type": "integer", "required": true,
             "description": "客户机内存大小（MiB）。不要超过宿主机实际可用的内存。",
             "example": 256},
            {"name": "guest_type", "type": "enum", "required": false,
             "default": "virtualized", "options": ["virtualized", "passthrough"],
             "description": "virtualized：纯虚拟客户机，设备都是模拟的；passthrough：直通物理设备，需要平台里真的有可直通的设备。"},
            {"name": "cpu_num", "type": "integer", "required": false,
             "default": 1,
             "description": "给客户机几个 vCPU。",
             "example": 1},
            {"name": "cmdline", "type": "string", "required": false,
             "default": null,
             "description": "传给内核的命令行，可留空。Linux 客户机要从磁盘根启动才需要 root=…；只进 shell 的话写个 console= 就够。",
             "example": "console=ttyAMA0 root=/dev/vda ro"},
        ],
    }))
}

/// Reads one creation request's `fields` into template parameters.
///
/// Addresses are accepted as a number or as text (`0x8020_0000`): a form field is
/// a string, and the values an operator copies out of a guest configuration are
/// written in hexadecimal.
fn template_params(fields: &Value) -> Result<VmTemplateParams, String> {
    Ok(VmTemplateParams {
        id: fields
            .get("id")
            .and_then(Value::as_u64)
            .ok_or("`id` is required")? as usize,
        name: text_field(fields, "name")?,
        guest_type: match fields.get("guest_type").and_then(Value::as_str) {
            None | Some("virtualized") => GuestType::Virtualized,
            Some("passthrough") => GuestType::Passthrough,
            Some(other) => return Err(format!("`guest_type` has no `{other}` model")),
        },
        cpu_num: fields.get("cpu_num").and_then(Value::as_u64).unwrap_or(1) as usize,
        entry_point: address_field(fields, "entry_point")?,
        kernel_path: text_field(fields, "kernel_path")?,
        kernel_load_addr: address_field(fields, "kernel_load_addr")?,
        // A form-made guest reads its kernel from the guest filesystem — that is
        // the only source the schema declares, so an omitted field is the
        // template's "fs" and anything else is refused here rather than at start
        // time, where it would read as a missing embedded image.
        image_location: match fields.get("image_location").and_then(Value::as_str) {
            None | Some("") | Some("fs") => "fs".to_string(),
            Some(other) => {
                return Err(format!(
                    "`image_location` has no `{other}` source: a form-made guest reads its \
                     kernel from the guest filesystem"
                ));
            }
        },
        cmdline: fields
            .get("cmdline")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        memory_base: address_field(fields, "memory_base")?,
        memory_mb: integer_field(fields, "memory_mb")?,
    })
}

fn text_field(fields: &Value, name: &str) -> Result<String, String> {
    fields
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| format!("`{name}` is required"))
}

/// A required field carrying a count.
///
/// `as_u64` is what turns a negative, fractional or textual value into a refusal
/// instead of a coercion, which is the difference between a field the operator
/// filled in and one the form guessed at.
fn integer_field(fields: &Value, name: &str) -> Result<usize, String> {
    fields
        .get(name)
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .ok_or_else(|| format!("`{name}` is required and must be a non-negative integer"))
}

fn address_field(fields: &Value, name: &str) -> Result<usize, String> {
    match fields.get(name) {
        Some(Value::Number(number)) => number
            .as_u64()
            .map(|value| value as usize)
            .ok_or_else(|| format!("`{name}` must be an address")),
        Some(Value::String(text)) => {
            let trimmed = text.trim().replace('_', "");
            let parsed = match trimmed
                .strip_prefix("0x")
                .or_else(|| trimmed.strip_prefix("0X"))
            {
                Some(hex) => usize::from_str_radix(hex, 16),
                None => trimmed.parse::<usize>(),
            };
            parsed.map_err(|_| format!("`{name}` is not an address: `{text}`"))
        }
        _ => Err(format!("`{name}` is required")),
    }
}

/// resource (memory, or the browser console lane table of a `browser-console`
/// build) is a 503, so a caller can tell "try later" from "this config is wrong".
pub async fn vm_create(Json(payload): Json<Value>) -> Response {
    // The form's shape: the fields the schema advertises, and nothing else. It
    // builds the same configuration the textual bodies produce, one step earlier,
    // so the checks below and the creation path after them are shared.
    if let Some(fields) = payload.get("fields") {
        let params = match template_params(fields) {
            Ok(params) => params,
            Err(reason) => {
                return (StatusCode::BAD_REQUEST, Json(json!({ "error": reason }))).into_response();
            }
        };
        let config = get_vm_config_template(params);
        let id = config.base.id;
        #[cfg(feature = "fs")]
        let guest_name = config.base.name.clone();
        if AxvmManager::vm_by_id(id).is_some() {
            return StatusCode::CONFLICT.into_response();
        }
        #[cfg(feature = "fs")]
        if let Some(missing) = crate::control::domain::pool::missing_guest_image(&config) {
            warn!("HTTP: create refused, `{missing}` is not in the guest filesystem");
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": format!("`{missing}` is not in the guest filesystem yet; transfer it first"),
                    "missing": missing,
                })),
            )
                .into_response();
        }
        // The same bytes the registry will hold are what the candidate scan
        // reads, so a form-made guest is also written onto the guest tree and
        // survives a reboot. Serialized before the config moves into the
        // manager; a serialization failure only costs the persistence, and is
        // reported in the response rather than thrown away.
        #[cfg(feature = "fs")]
        let config_toml = match config.to_toml() {
            Ok(text) => Some(text),
            Err(error) => {
                warn!("HTTP: cannot serialize VM[{id}]'s config for persistence: {error}");
                None
            }
        };
        let created = AxvmManager::create_vm_from_config(config);
        #[cfg(feature = "fs")]
        let saved = match &created {
            Ok(_) => config_toml
                .as_deref()
                .and_then(|text| persist_candidate(id, &guest_name, text)),
            Err(_) => None,
        };
        return match created {
            Ok(id) => {
                info!("HTTP: VM[{id}] created from form fields");
                let mut body = json!({ "id": id });
                #[cfg(feature = "fs")]
                if let Some(path) = &saved {
                    info!("HTTP: VM[{id}]'s config saved as `{path}`");
                    body["config"] = json!(path);
                }
                Json(body).into_response()
            }
            Err(error) => {
                error!("HTTP: create VM[{id}] from fields failed: {error:#}");
                create_failed(error)
            }
        };
    }

    let toml = match (
        payload.get("toml").and_then(Value::as_str),
        payload.get("path").and_then(Value::as_str),
    ) {
        (Some(toml), _) => toml.to_string(),
        // Reading a config from the guest filesystem needs the filesystem
        // feature; a build without it only accepts the pasted form.
        #[cfg(feature = "fs")]
        (None, Some(path)) => match ax_std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                warn!("HTTP: cannot read config `{path}`: {error}");
                return StatusCode::BAD_REQUEST.into_response();
            }
        },
        (None, _) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let Ok(config) = GuestConfig::from_toml(&toml) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let id = config.base.id;
    // Explicit duplicate check: `create_vm_from_toml` fails on a re-registered id
    // with a plain anyhow string, so surface the conflict as a contract error
    // (409) instead of an opaque 500.
    if AxvmManager::vm_by_id(id).is_some() {
        return StatusCode::CONFLICT.into_response();
    }

    // "The file is not there yet" and "the config is wrong" are different
    // answers, and only the first one is what a transfer is for. Checking it
    // here — with the same predicate the pool scan uses — keeps the refusal
    // readable instead of failing later, inside device setup, with an error
    // about a backing file nobody asked about. Without the filesystem feature
    // there is nothing to look up, so the check is absent rather than vacuous.
    #[cfg(feature = "fs")]
    if let Some(missing) = crate::control::domain::pool::missing_guest_image(&config) {
        warn!("HTTP: create refused, `{missing}` is not in the guest filesystem");
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": format!("`{missing}` is not in the guest filesystem yet; transfer it first"),
                "missing": missing,
            })),
        )
            .into_response();
    }

    match AxvmManager::create_vm_from_toml(&toml) {
        Ok(id) => {
            info!("HTTP: VM[{id}] created via control API");
            Json(json!({ "id": id })).into_response()
        }
        Err(error) => {
            error!("HTTP: create VM[{id}] failed: {error:#}");
            create_failed(error)
        }
    }
}

/// Writes a form-made guest's configuration onto the guest tree.
///
/// The registry holds the guest only for this boot; the file is what a later
/// scan lists as a candidate, which is what makes the guest reproducible after
/// a reboot. The file name comes from the guest's own name, reduced to one
/// plain component, with the id as the fallback when nothing survives the
/// reduction. A failure is reported to the caller as `None` — the guest is
/// registered either way — and logged, because "created but not written" is a
/// state the operator should be able to see.
#[cfg(feature = "fs")]
fn persist_candidate(id: usize, guest_name: &str, config_toml: &str) -> Option<String> {
    let stem: String = guest_name
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect();
    let stem = stem.trim_matches(|character| character == '-' || character == '.');
    let file = if stem.is_empty() {
        format!("guest-{id}.toml")
    } else {
        format!("{stem}.toml")
    };
    match crate::control::domain::pool::save_in(
        crate::control::domain::pool::directory(),
        &file,
        config_toml,
    ) {
        Ok(path) => Some(path),
        Err(error) => {
            warn!("HTTP: cannot persist VM[{id}]'s config as `{file}`: {error}");
            None
        }
    }
}

/// The answer for a creation that failed.
///
/// A config naming a guest file nobody transferred yet is a precondition the
/// operator clears by transferring it, so it keeps the same 409 body the
/// pre-create gate produces instead of reading as a host fault. The kernel
/// gate decides its files by reading the config; a device's backing file is
/// named by the model that owns the option, so this refusal arrives as the
/// typed error that failure produces. Every other failure keeps the shared
/// status mapping, which is the 503 an exhausted host resource gets on a start
/// request too.
fn create_failed(error: anyhow::Error) -> Response {
    if let Some(AxVmError::DeviceBackingFileMissing { path, .. }) =
        error.root_cause().downcast_ref::<AxVmError>()
    {
        warn!("HTTP: create refused, `{path}` is not in the guest filesystem");
        return (
            StatusCode::CONFLICT,
            Json(json!({
                "error": format!("`{path}` is not in the guest filesystem yet; transfer it first"),
                "missing": path,
            })),
        )
            .into_response();
    }
    map_axvm_error(error).into_response()
}

/// `DELETE /api/vms/{id}` — destroy and unregister a VM.
///
/// Two explicit steps so a failed destroy stays retryable: `destroy()` first
/// (its result is checked), and the registry is only touched on success. This
/// avoids relying on `Drop`-time destroy, which merely warns on failure after
/// the VM is already unregistered, leaving no handle to retry with.
pub async fn vm_delete(Path(id_str): Path<String>) -> Result<StatusCode, StatusCode> {
    let Ok(id) = id_str.parse::<usize>() else {
        return Err(StatusCode::NOT_FOUND);
    };
    let vm = AxvmManager::vm_by_id(id).ok_or(StatusCode::NOT_FOUND)?;
    // `destroy()`'s shared quiesce path carries the start->stop vCPU-entry
    // guard, so a DELETE arriving right after `/start` waits for the first vCPU
    // to enter the guest run loop instead of stranding it.
    vm.destroy()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let console_backend = crate::guest_console::backend_identity(id);
    AxvmManager::remove_vm(id).ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    if let Some(identity) = console_backend {
        crate::guest_console::remove_if_backend(identity);
    }
    info!("HTTP: VM[{id}] removed via control API");
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/vms/pool` — the configs a start request can create on demand.
///
/// The pool is every guest config the guest tree holds: the scan reads the
/// directory a new config is written to, any extra `AXVISOR_VM_DIRS` entries and
/// the tree itself (see [`crate::control::domain::pool`]), so a config anywhere
/// under the tree is a candidate. It is not the
/// VM registry: a pool entry is only a candidate and is created when a start
/// request names its id. `entries` carries the raw TOML so a client can show or
/// prefill a config, each entry says which `source` folder it came from, and
/// `issues` reports every file that cannot become an entry (unreadable, empty,
/// not a guest config, id claimed twice) instead of hiding it. `directory` and
/// `sources` are echoed so the client can tell "nothing provisioned" from
/// "wrong directory".
#[cfg(feature = "fs")]
pub async fn vm_pool() -> Json<Value> {
    let pool = crate::control::domain::pool::scan();
    let entries = entries_json(pool.entries());
    let issues = issues_json(pool.issues());
    Json(json!({
        "directory": pool.directory(),
        "sources": pool.sources(),
        "entries": entries,
        "issues": issues,
    }))
}

/// `GET /api/vms/browse?path=...` — list one directory of the guest filesystem.
///
/// Where [`vm_pool`] answers "what can be started without a create call", this
/// answers "what is in this folder": the subdirectories to walk into, every file
/// in it, and every `.toml` already parsed, so a client can offer the startable
/// configs, name the files a creation field may reference, and say why the other
/// configs are not usable. Together with the `path` form of [`vm_create`] this is
/// what lets an operator create a VM from a config in any folder instead of only
/// from the pool. A directory that cannot be read comes back as an empty listing
/// plus an issue, never an error, so browsing to a missing folder is a visible
/// state rather than a failed request.
#[cfg(feature = "fs")]
pub async fn vm_browse(Query(query): Query<BTreeMap<String, String>>) -> Json<Value> {
    let path = query
        .get("path")
        .map(String::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| crate::control::domain::pool::directory().to_string());
    let folder = crate::control::domain::pool::browse(&path);
    let directories: Vec<Value> = folder
        .directories()
        .iter()
        .map(|directory| json!({ "name": directory.name(), "path": directory.path() }))
        .collect();
    Json(json!({
        "path": folder.path(),
        "parent": folder.parent(),
        "directories": directories,
        "files": files_json(folder.files()),
        "entries": entries_json(folder.entries()),
        "issues": issues_json(folder.issues()),
    }))
}

/// `POST /api/vms/pool` — store a pasted config in the directory new configs go
/// to.
///
/// Body: `{"name": "guest.toml", "toml": "<完整 TOML 配置>"}`. This is how a
/// config reaches the pool on a machine whose shell cannot write a multi-line
/// file: the control plane validates the text and writes it as a pool file,
/// after which it is a candidate like any other. The name must be a plain
/// `*.toml` file name, so a request cannot write outside the pool directory.
#[cfg(feature = "fs")]
pub async fn vm_pool_save(Json(payload): Json<Value>) -> Result<Json<Value>, StatusCode> {
    let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
    let toml = payload
        .get("toml")
        .and_then(Value::as_str)
        .ok_or(StatusCode::BAD_REQUEST)?;
    match crate::control::domain::pool::save(name, toml) {
        Ok(path) => {
            info!("HTTP: config saved to pool as `{path}`");
            Ok(Json(json!({ "path": path })))
        }
        Err(error @ crate::control::domain::pool::SaveError::Unwritable(_)) => {
            error!("HTTP: cannot save pool config: {error}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
        Err(error) => {
            warn!("HTTP: rejected pool config: {error}");
            Err(StatusCode::BAD_REQUEST)
        }
    }
}

/// Render the plain files of a browsed folder for JSON.
///
/// These are the candidates a `file` creation field can name: a path a client
/// offers here is one that already exists in the guest filesystem, which is the
/// same predicate the create gate reads.
#[cfg(feature = "fs")]
fn files_json(files: &[crate::control::domain::pool::File]) -> Vec<Value> {
    files
        .iter()
        .map(|file| {
            json!({
                "name": file.name(),
                "path": file.path(),
                "size": file.size(),
            })
        })
        .collect()
}

/// Render pool entries for JSON, raw TOML included.
#[cfg(feature = "fs")]
fn entries_json(entries: &[crate::control::domain::pool::Entry]) -> Vec<Value> {
    entries
        .iter()
        .map(|entry| {
            json!({
                "id": entry.id(),
                "name": entry.name(),
                "path": entry.path(),
                "source": entry.source(),
                "toml": entry.toml(),
            })
        })
        .collect()
}

/// Render pool or browse issues for JSON.
#[cfg(feature = "fs")]
fn issues_json(issues: &[crate::control::domain::pool::Issue]) -> Vec<Value> {
    issues
        .iter()
        .map(|issue| {
            json!({
                "kind": issue.kind().as_str(),
                "path": issue.path(),
                "detail": issue.kind().to_string(),
            })
        })
        .collect()
}

/// `POST /api/vms/{id}/start` — start a VM.
///
/// An id that is not registered yet is created from its VM pool entry first
/// (see [`Self::vm_pool`] and [`AxvmManager::ensure_registered`]), so a client
/// can start what the pool lists without a separate create call. A full browser
/// console lane table fails that creation with 503.
pub async fn vm_start(Path(id_str): Path<String>) -> Result<Json<Value>, StatusCode> {
    vm_action(&id_str, VmAction::Start)
}

/// `POST /api/vms/{id}/stop` — request a VM stop.
///
/// `stop` has request semantics: it returns as soon as the request is accepted,
/// while the vCPU exits and the VM reaches `Stopped` asynchronously.
pub async fn vm_stop(Path(id_str): Path<String>) -> Result<Json<Value>, StatusCode> {
    vm_action(&id_str, VmAction::Stop)
}

/// `POST /api/vms/{id}/pause` — request a VM pause.
///
/// `pause` has the same request semantics as `stop`: the status flips to
/// `Paused` synchronously while the running vCPUs park at their next run-loop
/// iteration, so the response marks `async: true`.
pub async fn vm_pause(Path(id_str): Path<String>) -> Result<Json<Value>, StatusCode> {
    vm_action(&id_str, VmAction::Pause)
}

/// `POST /api/vms/{id}/resume` — resume a paused VM.
///
/// The status flips back to `Running` synchronously and the parked vCPUs are
/// woken, so the response marks `async: false`; the guest re-executes once the
/// vCPU tasks re-enter the guest.
pub async fn vm_resume(Path(id_str): Path<String>) -> Result<Json<Value>, StatusCode> {
    vm_action(&id_str, VmAction::Resume)
}

/// A lifecycle action on a VM.
enum VmAction {
    Start,
    Stop,
    Pause,
    Resume,
}

/// Drive one lifecycle action, mapping host errors to HTTP status codes.
///
/// Unknown VMs yield 404, invalid lifecycle transitions yield 409, and host
/// resource exhaustion yields 503.
fn vm_action(id_str: &str, action: VmAction) -> Result<Json<Value>, StatusCode> {
    let Ok(id) = id_str.parse::<usize>() else {
        return Err(StatusCode::NOT_FOUND);
    };
    // A start whose id is still only a pool candidate is created from its pool
    // entry first, so the control plane can start what `GET /api/vms/pool`
    // lists without a separate create call. Registered ids skip this and keep
    // the runtime's own state errors, and an id in neither place stays a 404:
    // the existence check only gates the create attempt.
    #[cfg(feature = "fs")]
    if matches!(action, VmAction::Start) && AxvmManager::vm_by_id(id).is_none() {
        match AxvmManager::ensure_registered(id) {
            Ok(true) => {}
            Ok(false) => return Err(StatusCode::NOT_FOUND),
            Err(error) => {
                return Err(map_axvm_error(
                    error.context(format!("create VM[{id}] from the VM pool")),
                ));
            }
        }
    }
    // No existence pre-check for the registered case: an unknown VM surfaces as
    // `VmNotFound` from the action and maps to 404 below, keeping the
    // check-then-act window closed.
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
        VmAction::Pause => AxvmManager::pause_vm(id),
        VmAction::Resume => AxvmManager::resume_vm(id),
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
        "async": matches!(action, VmAction::Stop | VmAction::Pause),
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
        // A device whose backing file is not in the guest filesystem is a
        // precondition the operator clears by transferring it. Creation answers
        // this with the file's name (`create_failed`); the other entry points,
        // a start of a pooled config among them, get the same status with the
        // file named in the log, since a bare status is all their contract
        // carries.
        Some(AxVmError::DeviceBackingFileMissing { path, .. }) => {
            warn!("HTTP: refused, `{path}` is not in the guest filesystem");
            StatusCode::CONFLICT
        }
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
        // VM-level aggregate re-execution evidence: the vCPU run loop increments
        // this *only* after a successful `run_vcpu`, so a resume that only
        // flips the status without re-entering the guest (or a failed entry
        // that returns `Err` before the guest runs) does not move it. The probe
        // asserts this count advances after every resume. It is an aggregate
        // across all vCPU tasks, so it proves *at least one* vCPU re-executed.
        json["guest_entry_count"] = json!(vm.guest_entry_count());
        // Pause-observation signal: the vCPU run loop increments this only when
        // a vCPU has genuinely parked in the suspend wait (published from inside
        // the wait condition, so the signal appears only once the vCPU is
        // actually blocked). This observes *a* vCPU park, not full quiescence —
        // there is no pause-completion API. The control plane waits for it after
        // each pause before resuming; a resume sent earlier is absorbed while
        // the vCPU is still running the guest and this counter does not advance.
        json["guest_park_count"] = json!(vm.guest_park_count());
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
