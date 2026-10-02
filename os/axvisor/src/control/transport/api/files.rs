//! Resumable file transfer into the guest filesystem.
//!
//! The six handlers here are the HTTP face of [`crate::control::domain::files`]:
//! they parse the request, hand the domain layer a chunk and a start offset, and
//! translate the answer into a status code. They own no state and no policy —
//! where bytes may go, how long a file may be, and when a file counts as
//! complete are all domain questions, and the answers are the same ones the
//! creation path asks when it checks whether a config's files are in place.
//!
//! A chunk request is refused before its body is read when its frame shape is
//! not the one this plane accepts (an octet-stream body and a `Content-Range`
//! naming both ends): reading first and rejecting after would mean a mistyped
//! request still gets to spend memory and disk, which is exactly what the
//! staging area is meant to keep out.

use alloc::{collections::BTreeMap, format};

use axum::Json;
use axum::body::{Body, to_bytes};
use axum::extract::{Path, Query, Request};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::control::domain::files::{self, CHUNK_LIMIT, FileError, SessionView};
use crate::control::domain::pool::{self, IssueKind};

/// `GET /api/files/browse?path=...` — what one folder holds.
///
/// A transfer target has to exist already, so the interface has to be able to
/// walk to one, and a file manager has to show what is in the folder it is
/// looking at. This is the same directory read the configuration pool browses
/// with, projected to a folder view: where this folder is, what is under it, and
/// why a folder could not be read. Guest configs are left out — a folder view
/// answers "what is here", not "what can become a VM" — and a `.toml` that is no
/// guest config is one of the files, not a problem.
pub async fn browse(Query(query): Query<BTreeMap<String, String>>) -> Json<Value> {
    let path = query
        .get("path")
        .map(String::as_str)
        .filter(|path| !path.trim().is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| pool::directory().to_string());
    let folder = pool::browse(&path);
    let directories: alloc::vec::Vec<Value> = folder
        .directories()
        .iter()
        .map(|directory| json!({ "name": directory.name(), "path": directory.path() }))
        .collect();
    let files: alloc::vec::Vec<Value> = folder
        .files()
        .iter()
        .map(|file| {
            json!({
                "name": file.name(),
                "path": file.path(),
                "size": file.size(),
            })
        })
        .collect();
    let issues: alloc::vec::Vec<Value> = folder
        .issues()
        .iter()
        .filter(|issue| matches!(issue.kind(), IssueKind::DirectoryUnavailable(_)))
        .map(|issue| {
            json!({
                "kind": issue.kind().as_str(),
                "path": issue.path(),
                "detail": issue.kind().to_string(),
            })
        })
        .collect();
    Json(json!({
        "path": folder.path(),
        "parent": folder.parent(),
        "directories": directories,
        "files": files,
        "issues": issues,
    }))
}

/// `GET /api/files` — the staged objects and where they stand.
pub async fn list_files() -> Json<Value> {
    Json(json!({
        "files": files::list().iter().map(view_json).collect::<alloc::vec::Vec<_>>(),
    }))
}

/// `POST /api/files` — open a session for one file.
///
/// The body is `{ "id", "directory", "total" }`. The id comes from the client,
/// which is what makes a second drag of the same file resume the first attempt
/// instead of staging the bytes twice.
pub async fn open_file(Json(payload): Json<Value>) -> Response {
    let Some(id) = payload.get("id").and_then(Value::as_str) else {
        return bad_request("`id` is required".into());
    };
    let Some(directory) = payload.get("directory").and_then(Value::as_str) else {
        return bad_request("`directory` is required".into());
    };
    let Some(total) = payload.get("total").and_then(Value::as_u64) else {
        return bad_request("`total` is required".into());
    };

    match files::open(id, directory, total as usize) {
        Ok(session) => with_offset(StatusCode::OK, &session),
        Err(error) => error_response(id, error),
    }
}

/// `HEAD /api/files/{id}` — where an interrupted transfer stopped.
///
/// Answering with the offset is what lets a client pick up a transfer the
/// control plane no longer remembers anything else about.
pub async fn resume_file(Path(id): Path<String>) -> Response {
    match files::resume(&id) {
        Ok(offset) => Response::builder()
            .status(StatusCode::OK)
            .header(UPLOAD_OFFSET, offset.to_string())
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(error) => error_response(&id, error),
    }
}

/// `PATCH /api/files/{id}` — append one chunk.
pub async fn send_chunk(Path(id): Path<String>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let Some(content_type) = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
    else {
        return bad_request("a chunk must carry `Content-Type: application/octet-stream`".into());
    };
    if !content_type.eq_ignore_ascii_case(OCTET_STREAM) {
        return bad_request(format!(
            "a chunk body must be `{OCTET_STREAM}`, not `{content_type}`"
        ));
    }
    let Some(range) = parts
        .headers
        .get(header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
    else {
        return bad_request("a chunk must carry a `Content-Range` header".into());
    };
    let Some((start, end, total)) = parse_range(range) else {
        return bad_request(format!(
            "`Content-Range: {range}` is not `bytes <start>-<end>/<total>`"
        ));
    };
    if let Some(total) = total {
        if total > files::FILE_LIMIT {
            return too_large(total);
        }
    }
    let expected = end - start + 1;
    if expected > CHUNK_LIMIT {
        return too_large(expected);
    }

    // Only now is the body read, and with the chunk limit as its bound.
    let chunk = match to_bytes(body, CHUNK_LIMIT).await {
        Ok(chunk) => chunk,
        Err(_) => {
            return bad_request(format!("the chunk body is larger than {CHUNK_LIMIT} bytes"));
        }
    };
    if chunk.len() != expected {
        return bad_request(format!(
            "`{range}` names {expected} bytes but the body carries {}",
            chunk.len()
        ));
    }

    match files::send(&id, start, &chunk) {
        Ok(offset) => Response::builder()
            .status(StatusCode::OK)
            .header(UPLOAD_OFFSET, offset.to_string())
            .body(Body::empty())
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(error) => error_response(&id, error),
    }
}

/// `POST /api/files/{id}/place` — move a finished file to its final name.
pub async fn place_file(Path(id): Path<String>, Json(payload): Json<Value>) -> Response {
    let Some(name) = payload.get("name").and_then(Value::as_str) else {
        return bad_request("`name` is required".into());
    };

    match files::place(&id, name) {
        Ok(session) => with_offset(StatusCode::OK, &session),
        Err(error) => error_response(&id, error),
    }
}

/// `POST /api/files/dirs` — create one directory level.
///
/// The interface's "new folder", and the only operation that adds a directory:
/// a transfer target has to exist already, because the operator picks it, and a
/// missing one is either a mistake or a step that was skipped.
pub async fn make_directory(Json(payload): Json<Value>) -> Response {
    let Some(parent) = payload.get("parent").and_then(Value::as_str) else {
        return bad_request("`parent` is required".into());
    };
    let Some(name) = payload.get("name").and_then(Value::as_str) else {
        return bad_request("`name` is required".into());
    };

    match files::make_directory(parent, name) {
        Ok(path) => (StatusCode::OK, Json(json!({ "path": path }))).into_response(),
        Err(error) => error_response(parent, error),
    }
}

/// `DELETE /api/files/{id}` — forget a session and remove its staged bytes.
pub async fn drop_file(Path(id): Path<String>) -> Response {
    match files::drop(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => error_response(&id, error),
    }
}

/// The chunk body's only accepted media type.
const OCTET_STREAM: &str = "application/octet-stream";

/// Response header that reports how many bytes are on disk.
const UPLOAD_OFFSET: &str = "Upload-Offset";

fn view_json(session: &SessionView) -> Value {
    json!({
        "id": session.id,
        "directory": session.directory,
        "name": session.name,
        "path": session.path,
        "total": session.total,
        "written": session.written,
        "state": session.state,
        "detail": session.detail,
    })
}

fn with_offset(status: StatusCode, session: &SessionView) -> Response {
    match serde_json::to_vec(&view_json(session)) {
        Ok(body) => Response::builder()
            .status(status)
            .header(header::CONTENT_TYPE, "application/json")
            .header(UPLOAD_OFFSET, session.written.to_string())
            .body(Body::from(body))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Maps a domain failure onto the status code that describes it.
///
/// A conflict always carries the offset the client should continue from: that is
/// the whole point of answering a resume with a status instead of a fresh start.
fn error_response(id: &str, error: FileError) -> Response {
    let (status, reason, offset) = match error {
        FileError::InvalidId(detail) => (
            StatusCode::BAD_REQUEST,
            format!("`{detail}` is not a usable session id"),
            0,
        ),
        FileError::InvalidDirectory(detail) => (
            StatusCode::BAD_REQUEST,
            format!("`{detail}` is not a usable target directory"),
            0,
        ),
        FileError::InvalidName(detail) => (
            StatusCode::BAD_REQUEST,
            format!("`{detail}` is not a plain file name"),
            0,
        ),
        FileError::NoSuchDirectory(detail) => (
            StatusCode::NOT_FOUND,
            format!("`{detail}` does not exist; create it first"),
            0,
        ),
        FileError::Exists(detail) => (
            StatusCode::CONFLICT,
            format!("`{detail}` already exists"),
            0,
        ),
        FileError::TooLarge { total, limit } => (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("{total} bytes exceeds the {limit} byte limit"),
            0,
        ),
        FileError::Unknown(detail) => (
            StatusCode::NOT_FOUND,
            format!("no file session `{detail}`"),
            0,
        ),
        FileError::Conflict { reason, offset } => (StatusCode::CONFLICT, reason, offset),
        FileError::Unwritable(message) => (StatusCode::INTERNAL_SERVER_ERROR, message, 0),
        FileError::StorageFull(message) => (
            StatusCode::INSUFFICIENT_STORAGE,
            format!("the guest filesystem is full: {message}"),
            0,
        ),
    };

    // The offset is worth reporting even on a failure: a client that lost a race
    // or ran out of space continues from the same place instead of guessing.
    let offset = match (status, offset) {
        (StatusCode::CONFLICT, 0) => files::resume(id).unwrap_or(0),
        (_, offset) => offset,
    };

    let mut response = (
        status,
        Json(json!({ "error": reason, "offset": offset, "subject": id })),
    )
        .into_response();
    if status == StatusCode::CONFLICT {
        let value = header::HeaderValue::from_str(&offset.to_string())
            .unwrap_or_else(|_| header::HeaderValue::from_static("0"));
        response.headers_mut().insert(UPLOAD_OFFSET, value);
    }
    response
}

fn bad_request(reason: String) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": reason }))).into_response()
}

fn too_large(total: usize) -> Response {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        Json(json!({
            "error": format!("{total} bytes exceeds the {} byte limit", files::FILE_LIMIT),
        })),
    )
        .into_response()
}

/// Parses `bytes <start>-<end>/<total>`, the one chunk frame this plane accepts.
///
/// `*` is accepted for the total: a client streaming without knowing the final
/// length is not something the staging area supports, but the shape it sends is
/// still a range, and the declared length is what the session was opened with.
fn parse_range(value: &str) -> Option<(usize, usize, Option<usize>)> {
    let rest = value.strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start: usize = start.trim().parse().ok()?;
    let end: usize = end.trim().parse().ok()?;
    if end < start {
        return None;
    }
    let total = match total.trim() {
        "*" => None,
        value => Some(value.parse().ok()?),
    };
    Some((start, end, total))
}
