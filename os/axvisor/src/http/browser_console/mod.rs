//! Board-hosted HTTP/WebSocket gateway for the startup network console lanes.

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{
        Path,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
mod page;

const BROWSER_INPUT_CAPACITY: usize = 4096;
/// Browser-console routes served by Axvisor's optional HTTP listener.
pub(super) fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/consoles", get(console_descriptions))
        .route("/ws/{endpoint}", get(upgrade_console))
}

async fn index() -> impl IntoResponse {
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'self'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self' ws: wss:; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Html(page::INDEX_HTML),
    )
}

async fn console_descriptions() -> Json<Vec<Value>> {
    Json(
        crate::network_console::console_descriptions()
            .into_iter()
            .map(|console| {
                json!({
                    "route": console.route,
                    "name": console.display_name,
                })
            })
            .collect(),
    )
}

async fn upgrade_console(
    Path(endpoint): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, StatusCode> {
    validate_browser_origin(&headers)?;
    if !crate::network_console::has_console_route(&endpoint) {
        return Err(StatusCode::NOT_FOUND);
    }
    let (input, output) =
        crate::network_console::open_browser_console(&endpoint).map_err(|error| {
            warn!("{endpoint} browser console could not open: {error}");
            StatusCode::CONFLICT
        })?;

    Ok(upgrade
        .max_message_size(BROWSER_INPUT_CAPACITY)
        .max_frame_size(BROWSER_INPUT_CAPACITY)
        .on_upgrade(move |browser| async move {
            if let Err(error) = bridge_console(browser, input, output).await {
                warn!("{endpoint} browser console bridge stopped: {error:#}");
            }
        })
        .into_response())
}

fn validate_browser_origin(headers: &HeaderMap) -> Result<(), StatusCode> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::FORBIDDEN)?;
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or(StatusCode::FORBIDDEN)?;

    if origin == format!("http://{host}") || origin == format!("https://{host}") {
        Ok(())
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

async fn bridge_console(
    browser: WebSocket,
    mut console_input: crate::network_console::BrowserConsoleInput,
    console_output: crate::network_console::BrowserConsoleOutput,
) -> Result<()> {
    let (mut browser_sender, mut browser_receiver) = browser.split();
    browser_sender
        .send(Message::Binary(
            console_input.greeting().into_bytes().into(),
        ))
        .await
        .context("failed to write the browser console greeting")?;

    std::thread::Builder::new()
        .name("browser-console-output".into())
        .spawn(move || {
            if let Err(error) = run_browser_output(browser_sender, console_output) {
                warn!("browser console output stopped: {error:#}");
            }
        })
        .context("failed to start browser console output task")?;

    read_browser_input(&mut browser_receiver, &mut console_input).await
}

async fn read_browser_input(
    browser_receiver: &mut futures_util::stream::SplitStream<WebSocket>,
    console_input: &mut crate::network_console::BrowserConsoleInput,
) -> Result<()> {
    while let Some(message) = browser_receiver.next().await {
        let keep_open = match message.context("failed to read the browser console")? {
            Message::Text(text) => console_input.route(text.as_bytes()),
            Message::Binary(bytes) => console_input.route(&bytes),
            Message::Close(_) => return Ok(()),
            Message::Ping(_) | Message::Pong(_) => true,
        };
        if !keep_open {
            return Ok(());
        }
    }
    Ok(())
}

fn run_browser_output(
    mut browser_sender: futures_util::stream::SplitSink<WebSocket, Message>,
    mut console_output: crate::network_console::BrowserConsoleOutput,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .context("failed to build browser console output runtime")?;

    while let Some(output) = console_output.receive() {
        runtime
            .block_on(browser_sender.send(Message::Binary(output.into())))
            .context("failed to write the browser console")?;
    }
    Ok(())
}
