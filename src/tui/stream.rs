//! The daemon chat-stream driver: opens the bi-directional
//! `ChatService.StreamChat` call and hands the UI an outbound sink plus an
//! inbound [`tokio::sync::mpsc`] receiver of [`ChatEvent`]s.
//!
//! The stream stays open for the lifetime of the TUI. Dropping the returned
//! outbound sender ends the client→daemon half, which the daemon uses to close
//! the response half — a clean gRPC bidi shutdown (no `Stop` needed).

use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tonic::transport::Channel;
use tonic::Request;

use crate::endpoint::with_workspace;

use astra_proto::astra::engine::v1::chat_service_client::ChatServiceClient;
use astra_proto::astra::engine::v1::{ChatClientMsg, ChatEvent};

/// One inbound item on the UI-facing event channel. The stream is open for the
/// lifetime of the TUI, so its end is a distinct event from an error (which
/// means the daemon died mid-stream rather than closed cleanly).
#[derive(Debug)]
pub enum DaemonEvent {
    /// A normal chat event forwarded from the daemon.
    Event(ChatEvent),
    /// The daemon closed the stream cleanly.
    Closed,
    /// The stream errored (daemon died / connection lost).
    Error(String),
}

/// Open the chat stream. Returns `(outbound_sender, inbound_receiver)`.
pub async fn spawn(
    channel: Channel,
) -> anyhow::Result<(mpsc::Sender<ChatClientMsg>, mpsc::Receiver<DaemonEvent>)> {
    let mut client = ChatServiceClient::new(channel);

    let (outbound_tx, outbound_rx) = mpsc::channel::<ChatClientMsg>(32);
    let (event_tx, event_rx) = mpsc::channel::<DaemonEvent>(512);

    let request_stream = ReceiverStream::new(outbound_rx);
    let response = client
        .stream_chat(with_workspace(Request::new(request_stream)))
        .await?;
    let mut inbound = response.into_inner();

    // Pump inbound events to the UI. The task ends when the daemon closes the
    // stream, the stream errors, or the UI drops `event_rx`. An `Err` from the
    // stream is surfaced as a distinct [`DaemonEvent::Error`] so the UI can show
    // "daemon connection lost" rather than exiting silently.
    tokio::spawn(async move {
        loop {
            match inbound.message().await {
                Ok(Some(event)) => {
                    if event_tx.send(DaemonEvent::Event(event)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => {
                    let _ = event_tx.send(DaemonEvent::Closed).await;
                    break;
                }
                Err(status) => {
                    let _ = event_tx
                        .send(DaemonEvent::Error(format!(
                            "daemon connection lost: {status}"
                        )))
                        .await;
                    break;
                }
            }
        }
    });

    Ok((outbound_tx, event_rx))
}
