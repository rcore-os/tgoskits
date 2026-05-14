use std::fmt;

use anyhow::Context as _;
use chrono::{DateTime, Utc};
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct BoardServerClient {
    client: reqwest::Client,
    base_url: Url,
    ws_base_url: Url,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BoardTypeSummary {
    pub board_type: String,
    pub tags: Vec<String>,
    pub total: usize,
    pub available: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateSessionRequest {
    pub board_type: String,
    pub required_tags: Vec<String>,
    pub client_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCreatedResponse {
    pub session_id: String,
    pub board_id: String,
    pub lease_expires_at: DateTime<Utc>,
    pub serial_available: bool,
    pub boot_mode: String,
    pub ws_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HeartbeatResponse {
    pub session_id: String,
    pub lease_expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BootConfig {
    Uboot(UbootProfile),
    Pxe(PxeProfile),
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct UbootProfile {
    #[serde(default)]
    pub use_tftp: bool,
    pub dtb_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct PxeProfile {
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BootProfileResponse {
    pub boot: BootConfig,
    pub server_ip: Option<String>,
    pub netmask: Option<String>,
    pub interface: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SerialStatusResponse {
    pub available: bool,
    pub connected: bool,
    pub port: Option<String>,
    pub baud_rate: Option<u32>,
    pub ws_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileResponse {
    pub filename: String,
    pub relative_path: String,
    pub tftp_url: Option<String>,
    pub size: u64,
    pub uploaded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TftpSessionResponse {
    pub available: bool,
    pub provider: String,
    pub server_ip: Option<String>,
    pub netmask: Option<String>,
    pub writable: bool,
    pub files: Vec<FileResponse>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionDtbResponse {
    pub dtb_name: Option<String>,
    pub relative_path: Option<String>,
    pub session_file_path: Option<String>,
    pub tftp_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ErrorResponse {
    code: String,
    message: String,
}

#[derive(Debug, Clone, Error)]
#[error("{message}")]
pub struct BoardServerClientError {
    pub status: StatusCode,
    pub code: Option<String>,
    pub message: String,
}

impl BoardServerClientError {
    pub fn is_no_available_board_for(&self, board_type: &str) -> bool {
        self.status == StatusCode::CONFLICT
            && self.code.as_deref() == Some("conflict")
            && self.message == format!("no available board for type `{board_type}`")
    }

    pub fn is_board_type_not_found_for(&self, board_type: &str) -> bool {
        self.status == StatusCode::NOT_FOUND
            && self.code.as_deref() == Some("not_found")
            && self.message == format!("board type `{board_type}` not found")
    }
}

impl BoardServerClient {
    pub fn new(server: &str, port: u16) -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .build()
                .context("failed to build HTTP client")?,
            base_url: build_base_url("http", server, port)?,
            ws_base_url: build_base_url("ws", server, port)?,
        })
    }

    pub async fn list_board_types(&self) -> Result<Vec<BoardTypeSummary>, BoardServerClientError> {
        let response = self
            .client
            .get(self.endpoint("/api/v1/board-types"))
            .send()
            .await
            .map_err(Self::request_error)?;
        self.decode_json(response).await
    }

    pub async fn create_session(
        &self,
        board_type: &str,
    ) -> Result<SessionCreatedResponse, BoardServerClientError> {
        let response = self
            .client
            .post(self.endpoint("/api/v1/sessions"))
            .json(&CreateSessionRequest {
                board_type: board_type.to_string(),
                required_tags: vec![],
                client_name: Some("ostool".to_string()),
            })
            .send()
            .await
            .map_err(Self::request_error)?;
        self.decode_json(response).await
    }

    pub async fn heartbeat(
        &self,
        session_id: &str,
    ) -> Result<HeartbeatResponse, BoardServerClientError> {
        let response = self
            .client
            .post(self.endpoint(&format!("/api/v1/sessions/{session_id}/heartbeat")))
            .send()
            .await
            .map_err(Self::request_error)?;
        self.decode_json(response).await
    }

    pub async fn delete_session(&self, session_id: &str) -> Result<(), BoardServerClientError> {
        let response = self
            .client
            .delete(self.endpoint(&format!("/api/v1/sessions/{session_id}")))
            .send()
            .await
            .map_err(Self::request_error)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(());
        }
        self.decode_empty(response).await
    }

    pub async fn get_boot_profile(
        &self,
        session_id: &str,
    ) -> Result<BootProfileResponse, BoardServerClientError> {
        let response = self
            .client
            .get(self.endpoint(&format!("/api/v1/sessions/{session_id}/boot-profile")))
            .send()
            .await
            .map_err(Self::request_error)?;
        self.decode_json(response).await
    }

    pub async fn get_serial_status(
        &self,
        session_id: &str,
    ) -> Result<SerialStatusResponse, BoardServerClientError> {
        let response = self
            .client
            .get(self.endpoint(&format!("/api/v1/sessions/{session_id}/serial")))
            .send()
            .await
            .map_err(Self::request_error)?;
        self.decode_json(response).await
    }

    pub async fn get_tftp_status(
        &self,
        session_id: &str,
    ) -> Result<TftpSessionResponse, BoardServerClientError> {
        let response = self
            .client
            .get(self.endpoint(&format!("/api/v1/sessions/{session_id}/tftp")))
            .send()
            .await
            .map_err(Self::request_error)?;
        self.decode_json(response).await
    }

    pub async fn get_session_dtb(
        &self,
        session_id: &str,
    ) -> Result<SessionDtbResponse, BoardServerClientError> {
        let response = self
            .client
            .get(self.endpoint(&format!("/api/v1/sessions/{session_id}/dtb")))
            .send()
            .await
            .map_err(Self::request_error)?;
        self.decode_json(response).await
    }

    pub async fn download_session_dtb(
        &self,
        session_id: &str,
    ) -> Result<Vec<u8>, BoardServerClientError> {
        let response = self
            .client
            .get(self.endpoint(&format!("/api/v1/sessions/{session_id}/dtb/download")))
            .send()
            .await
            .map_err(Self::request_error)?;
        self.decode_bytes(response).await
    }

    pub async fn upload_session_file(
        &self,
        session_id: &str,
        relative_path: &str,
        bytes: Vec<u8>,
    ) -> Result<FileResponse, BoardServerClientError> {
        let response = self
            .client
            .put(self.endpoint(&format!("/api/v1/sessions/{session_id}/files")))
            .header("X-File-Path", relative_path)
            .body(bytes)
            .send()
            .await
            .map_err(Self::request_error)?;
        self.decode_json(response).await
    }

    pub fn resolve_ws_url(&self, ws_url: &str) -> anyhow::Result<Url> {
        if ws_url.starts_with("ws://") || ws_url.starts_with("wss://") {
            return Url::parse(ws_url).with_context(|| format!("invalid websocket URL `{ws_url}`"));
        }

        self.ws_base_url
            .join(ws_url)
            .with_context(|| format!("failed to resolve websocket URL `{ws_url}`"))
    }

    fn endpoint(&self, path: &str) -> Url {
        self.base_url
            .join(path.trim_start_matches('/'))
            .expect("static API path should be valid")
    }

    async fn decode_json<T: DeserializeOwned>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, BoardServerClientError> {
        if response.status().is_success() {
            response.json::<T>().await.map_err(Self::request_error)
        } else {
            Err(Self::api_error(response).await)
        }
    }

    async fn decode_empty(
        &self,
        response: reqwest::Response,
    ) -> Result<(), BoardServerClientError> {
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Self::api_error(response).await)
        }
    }

    async fn decode_bytes(
        &self,
        response: reqwest::Response,
    ) -> Result<Vec<u8>, BoardServerClientError> {
        if response.status().is_success() {
            response
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(Self::request_error)
        } else {
            Err(Self::api_error(response).await)
        }
    }

    async fn api_error(response: reqwest::Response) -> BoardServerClientError {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        parse_error_body(status, &body)
    }

    fn request_error(err: reqwest::Error) -> BoardServerClientError {
        BoardServerClientError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: Some("request_failed".to_string()),
            message: err.to_string(),
        }
    }
}

fn build_base_url(scheme: &str, server: &str, port: u16) -> anyhow::Result<Url> {
    let mut url = Url::parse(&format!("{scheme}://localhost"))
        .with_context(|| format!("failed to create {scheme} URL"))?;
    url.set_host(Some(server))
        .map_err(|_| anyhow::anyhow!("invalid server host `{server}`"))?;
    url.set_port(Some(port))
        .map_err(|_| anyhow::anyhow!("invalid port `{port}`"))?;
    Ok(url)
}

fn parse_error_body(status: StatusCode, body: &str) -> BoardServerClientError {
    match serde_json::from_str::<ErrorResponse>(body) {
        Ok(error) => BoardServerClientError {
            status,
            code: Some(error.code),
            message: error.message,
        },
        Err(_) if !body.trim().is_empty() => BoardServerClientError {
            status,
            code: None,
            message: body.trim().to_string(),
        },
        Err(_) => BoardServerClientError {
            status,
            code: None,
            message: format!("request failed with status {status}"),
        },
    }
}

impl fmt::Display for BoardTypeSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}/{})", self.board_type, self.available, self.total)
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::{BoardServerClient, BootConfig, parse_error_body};

    #[test]
    fn resolve_relative_ws_url_uses_server_defaults() {
        let client = BoardServerClient::new("127.0.0.1", 8080).unwrap();
        let url = client
            .resolve_ws_url("/api/v1/sessions/demo/serial/ws")
            .unwrap();
        assert_eq!(
            url.as_str(),
            "ws://127.0.0.1:8080/api/v1/sessions/demo/serial/ws"
        );
    }

    #[test]
    fn resolve_absolute_ws_url_keeps_original_value() {
        let client = BoardServerClient::new("127.0.0.1", 8080).unwrap();
        let url = client
            .resolve_ws_url("ws://10.0.0.2:9000/api/v1/sessions/demo/serial/ws")
            .unwrap();
        assert_eq!(
            url.as_str(),
            "ws://10.0.0.2:9000/api/v1/sessions/demo/serial/ws"
        );
    }

    #[test]
    fn parse_error_body_prefers_structured_api_errors() {
        let error = parse_error_body(
            StatusCode::CONFLICT,
            r#"{"code":"conflict","message":"no available board for type `rk3568`"}"#,
        );
        assert_eq!(error.status, StatusCode::CONFLICT);
        assert_eq!(error.code.as_deref(), Some("conflict"));
        assert_eq!(error.message, "no available board for type `rk3568`");
    }

    #[test]
    fn not_found_error_is_classified_as_missing_board_type() {
        let error = parse_error_body(
            StatusCode::NOT_FOUND,
            r#"{"code":"not_found","message":"board type `rk3568` not found"}"#,
        );
        assert!(error.is_board_type_not_found_for("rk3568"));
        assert!(!error.is_no_available_board_for("rk3568"));
    }

    #[test]
    fn parse_uboot_boot_profile() {
        let response: super::BootProfileResponse = serde_json::from_str(
            r#"{
                "boot": {
                    "kind": "uboot",
                    "use_tftp": true
                },
                "server_ip": "10.0.0.2",
                "netmask": "255.255.255.0",
                "interface": "eth0"
            }"#,
        )
        .unwrap();

        assert_eq!(response.server_ip.as_deref(), Some("10.0.0.2"));
        match response.boot {
            BootConfig::Uboot(profile) => {
                assert!(profile.use_tftp);
            }
            BootConfig::Pxe(_) => panic!("expected uboot profile"),
        }
    }

    #[test]
    fn parse_tftp_session_file_response() {
        let response: super::TftpSessionResponse = serde_json::from_str(
            r#"{
                "available": true,
                "provider": "builtin",
                "server_ip": "10.0.0.2",
                "netmask": "255.255.255.0",
                "writable": true,
                "files": [
                    {
                        "filename": "image.fit",
                        "relative_path": "ostool/sessions/demo/boot/image.fit",
                        "tftp_url": "tftp://10.0.0.2/ostool/sessions/demo/boot/image.fit",
                        "size": 1234,
                        "uploaded_at": "2026-04-01T00:00:00Z"
                    }
                ]
            }"#,
        )
        .unwrap();

        assert!(response.available);
        assert_eq!(response.files.len(), 1);
        assert_eq!(response.files[0].filename, "image.fit");
    }

    #[test]
    fn parse_session_dtb_response() {
        let response: super::SessionDtbResponse = serde_json::from_str(
            r#"{
                "dtb_name": "board.dtb",
                "relative_path": "ostool/sessions/demo/boot/dtb/board.dtb",
                "session_file_path": "boot/dtb/board.dtb",
                "tftp_url": "tftp://10.0.0.2/ostool/sessions/demo/boot/dtb/board.dtb"
            }"#,
        )
        .unwrap();

        assert_eq!(response.dtb_name.as_deref(), Some("board.dtb"));
        assert_eq!(
            response.session_file_path.as_deref(),
            Some("boot/dtb/board.dtb")
        );
    }
}
