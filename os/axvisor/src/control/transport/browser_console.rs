//! Board-hosted HTTP/WebSocket gateway for the current network console lanes.
//!
//! The console set is a runtime registry (see [`crate::network_console`]): a VM
//! gets its lane when it is created and loses it when it is removed, so the
//! lane table changes while the hypervisor runs and a route that answered a
//! moment ago can be gone.
//!
//! This module serves the gateway only — discovery plus one socket per lane. The
//! paths those two live at are declared in
//! [`crate::control::capability::table`], which is the only place that knows
//! them; here they are just two handlers plus the routes that mount them. The
//! page a human actually looks at is the embedded dashboard
//! (`crate::control::web`, the `web-ui` feature), which consumes this gateway; a
//! build with `browser-console` but without `web-ui` is a headless gateway
//! whose `/` stays a 404.

use anyhow::{Context, Result};
use axum::{
    Json,
    extract::{
        Path,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{MethodRouter, get},
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};

const BROWSER_INPUT_CAPACITY: usize = 4096;

/// Route for the lane table: which lanes exist and whether they are taken.
pub(crate) fn console_list_route() -> MethodRouter {
    get(console_descriptions)
}

/// Route for a lane socket, shared by the guest and management panels.
pub(crate) fn console_stream_route() -> MethodRouter {
    get(upgrade_console)
}

async fn console_descriptions() -> Json<Vec<Value>> {
    Json(
        crate::network_console::console_descriptions()
            .into_iter()
            .map(|console| {
                json!({
                    "route": console.route,
                    "name": console.display_name,
                    "attached": console.attached,
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
    super::validate_browser_origin(&headers)?;
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

async fn bridge_console(
    browser: WebSocket,
    mut console_input: crate::network_console::BrowserConsoleInput,
    console_output: crate::network_console::BrowserConsoleOutput,
) -> Result<()> {
    let (browser_sender, mut browser_receiver) = browser.split();
    let greeting = Message::Binary(console_input.greeting().into_bytes().into());

    std::thread::Builder::new()
        .name("browser-console-output".into())
        .spawn(move || {
            crate::network_console::pin_current_task();
            if let Err(error) = run_browser_output(browser_sender, console_output, greeting) {
                warn!("browser console output stopped: {error:#}");
            }
        })
        .context("failed to start browser console output task")?;

    // The worker publishes the greeting only after it owns the output half and
    // has crossed the same scheduler boundary as all subsequent output. This
    // makes the greeting a protocol-level readiness edge for the input half.
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
    greeting: Message,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .context("failed to build browser console output runtime")?;

    runtime
        .block_on(browser_sender.send(greeting))
        .context("failed to write the browser console greeting")?;
    while let Some(output) = console_output.receive()? {
        runtime
            .block_on(browser_sender.send(Message::Binary(output.into())))
            .context("failed to write the browser console")?;
    }
    Ok(())
}
