use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use anyhow::{Context as _, anyhow};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use crate::sterm::{AsyncTerminal, TerminalConfig};

#[derive(Debug, Deserialize)]
pub(crate) struct ServerControlMessage {
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) message: Option<String>,
}

pub(crate) enum ServerControlAction {
    Ignore,
    Close,
    Error(anyhow::Error),
    Forward,
}

pub(crate) fn classify_server_control_message(
    control: &ServerControlMessage,
    locally_closed: bool,
) -> ServerControlAction {
    match control.kind.as_str() {
        "opened" => ServerControlAction::Ignore,
        "closed" => {
            if locally_closed {
                ServerControlAction::Close
            } else {
                ServerControlAction::Error(anyhow!(
                    "ostool-server closed the serial websocket; the board session may have been released"
                ))
            }
        }
        "error" => {
            let message = control
                .message
                .clone()
                .unwrap_or_else(|| "serial websocket error".to_string());
            ServerControlAction::Error(anyhow!("ostool-server serial websocket error: {message}"))
        }
        _ => ServerControlAction::Forward,
    }
}

pub async fn run_serial_terminal(ws_url: reqwest::Url) -> anyhow::Result<()> {
    let (stream, _) = tokio_tungstenite::connect_async(ws_url.as_str())
        .await
        .with_context(|| format!("failed to connect serial websocket {}", ws_url))?;
    let (mut sink, mut stream) = stream.split();
    let locally_closed = Arc::new(AtomicBool::new(false));

    let (inbound_tx, inbound_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let read_task = tokio::spawn({
        let locally_closed = locally_closed.clone();
        async move {
            while let Some(message) = stream.next().await {
                match message.context("serial websocket read failed")? {
                    Message::Binary(bytes) => {
                        if inbound_tx.send(bytes.to_vec()).is_err() {
                            break;
                        }
                    }
                    Message::Text(text) => {
                        if let Ok(control) = serde_json::from_str::<ServerControlMessage>(&text) {
                            match classify_server_control_message(
                                &control,
                                locally_closed.load(Ordering::SeqCst),
                            ) {
                                ServerControlAction::Ignore => continue,
                                ServerControlAction::Close => break,
                                ServerControlAction::Error(err) => {
                                    let _ = inbound_tx
                                        .send(format!("\n[ostool-server] {err}\n").into_bytes());
                                    return Err(err);
                                }
                                ServerControlAction::Forward => {}
                            }
                        }
                        if inbound_tx.send(text.bytes().collect()).is_err() {
                            break;
                        }
                    }
                    Message::Close(_) => {
                        if locally_closed.load(Ordering::SeqCst) {
                            break;
                        }
                        return Err(anyhow!(
                            "ostool-server closed the serial websocket; the board session may have been released"
                        ));
                    }
                    Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
                }
            }

            Ok::<(), anyhow::Error>(())
        }
    });

    let write_task = tokio::spawn({
        let locally_closed = locally_closed.clone();
        async move {
            while let Some(bytes) = outbound_rx.recv().await {
                sink.send(Message::Binary(bytes.into()))
                    .await
                    .context("serial websocket write failed")?;
            }

            locally_closed.store(true, Ordering::SeqCst);
            let _ = sink
                .send(Message::Text(r#"{"type":"close"}"#.to_string().into()))
                .await;
            let _ = sink.send(Message::Close(None)).await;
            Ok::<(), anyhow::Error>(())
        }
    });

    let terminal = AsyncTerminal::new(TerminalConfig {
        intercept_exit_sequence: true,
        timeout: None,
        timeout_label: "remote serial terminal".to_string(),
    });
    let run_result = terminal
        .run(inbound_rx, outbound_tx, |_handle, _byte| {})
        .await;

    let mut write_task = write_task;
    let write_result =
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut write_task).await;
    let mut read_task = read_task;
    let read_result =
        tokio::time::timeout(std::time::Duration::from_millis(300), &mut read_task).await;

    let write_error = match write_result {
        Ok(Ok(Ok(()))) => None,
        Ok(Ok(Err(err))) => Some(err),
        Ok(Err(err)) if !err.is_cancelled() => {
            Some(anyhow!("serial websocket writer join error: {err}"))
        }
        Ok(Err(_)) => None,
        Err(_) => {
            write_task.abort();
            let _ = write_task.await;
            Some(anyhow!("serial websocket writer shutdown timed out"))
        }
    };
    let read_error = match read_result {
        Ok(Ok(Ok(()))) => None,
        Ok(Ok(Err(err))) => Some(err),
        Ok(Err(err)) if !err.is_cancelled() => {
            Some(anyhow!("serial websocket reader join error: {err}"))
        }
        Ok(Err(_)) => None,
        Err(_) => {
            read_task.abort();
            let _ = read_task.await;
            Some(anyhow!("serial websocket reader shutdown timed out"))
        }
    };

    if let Some(err) = write_error.or(read_error) {
        if run_result.is_ok() {
            return Err(err);
        }
        log::warn!("remote serial terminal shutdown failed: {err:#}");
    }

    run_result
}

#[cfg(test)]
mod tests {
    use super::{ServerControlAction, ServerControlMessage, classify_server_control_message};

    #[test]
    fn parse_server_control_message() {
        let opened: ServerControlMessage = serde_json::from_str(r#"{"type":"opened"}"#).unwrap();
        assert_eq!(opened.kind, "opened");
    }

    #[test]
    fn parse_server_error_control_message() {
        let error: ServerControlMessage =
            serde_json::from_str(r#"{"type":"error","message":"power failed"}"#).unwrap();
        assert_eq!(error.kind, "error");
        assert_eq!(error.message.as_deref(), Some("power failed"));
    }

    #[test]
    fn closed_control_message_becomes_error_when_not_locally_closed() {
        let control: ServerControlMessage = serde_json::from_str(r#"{"type":"closed"}"#).unwrap();
        match classify_server_control_message(&control, false) {
            ServerControlAction::Error(err) => {
                assert!(err.to_string().contains("may have been released"));
            }
            _ => panic!("expected error action"),
        }
    }

    #[test]
    fn closed_control_message_is_normal_when_locally_closed() {
        let control: ServerControlMessage = serde_json::from_str(r#"{"type":"closed"}"#).unwrap();
        assert!(matches!(
            classify_server_control_message(&control, true),
            ServerControlAction::Close
        ));
    }
}
