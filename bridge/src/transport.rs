//! WebSocket transport for the stream-json protocol.
//!
//! Wraps a `tokio-tungstenite` WebSocket connection and presents it as a
//! [`futures::Stream`] of decoded [`Event`]s and a [`futures::Sink`] that
//! accepts `Event`s to send.

use crate::types::Event;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{self, Message},
    MaybeTlsStream, WebSocketStream,
};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("WebSocket error: {0}")]
    WebSocket(#[from] tungstenite::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("connection closed")]
    Closed,
}

/// A bidirectional WebSocket transport for bridge events.
///
/// Implements `Stream<Item = Result<Event, TransportError>>` and
/// `Sink<Event, Error = TransportError>`.
pub struct WsTransport {
    sink: SplitSink<WsStream, Message>,
    stream: SplitStream<WsStream>,
}

impl WsTransport {
    /// Connect to a WebSocket URL, optionally providing a bearer token.
    pub async fn connect(url: &str, bearer_token: Option<&str>) -> Result<Self, TransportError> {
        let mut request = url
            .into_client_request()
            .map_err(|_| tungstenite::Error::Url(tungstenite::error::UrlError::NoPathOrQuery))?;

        if let Some(token) = bearer_token {
            if let Ok(val) = HeaderValue::from_str(&format!("Bearer {token}")) {
                request.headers_mut().insert("Authorization", val);
            }
        }
        request
            .headers_mut()
            .insert("anthropic-version", HeaderValue::from_static("2023-06-01"));

        let (ws, _resp) = connect_async_with_config(request, None, false).await?;
        let (sink, stream) = ws.split();
        Ok(Self { sink, stream })
    }

    /// Send an event over the WebSocket.
    pub async fn send(&mut self, event: &Event) -> Result<(), TransportError> {
        let json = serde_json::to_string(event)?;
        self.sink.send(Message::Text(json.into())).await?;
        Ok(())
    }

    /// Receive the next event from the WebSocket.
    ///
    /// Returns `None` when the connection is closed cleanly.
    pub async fn recv(&mut self) -> Result<Option<Event>, TransportError> {
        loop {
            match self.stream.next().await {
                Some(Ok(Message::Text(text))) => {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let event: Event = serde_json::from_str(trimmed)?;
                    return Ok(Some(event));
                }
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => continue,
                Some(Ok(Message::Close(_))) | None => return Ok(None),
                Some(Ok(Message::Binary(data))) => {
                    let event: Event = serde_json::from_slice(&data)?;
                    return Ok(Some(event));
                }
                Some(Ok(Message::Frame(_))) => continue,
                Some(Err(e)) => return Err(TransportError::WebSocket(e)),
            }
        }
    }

    /// Close the WebSocket gracefully.
    pub async fn close(mut self) -> Result<(), TransportError> {
        self.sink
            .send(Message::Close(None))
            .await
            .map_err(TransportError::WebSocket)
    }
}

// ---------------------------------------------------------------------------
// Stream + Sink trait impls (for use with combinators)
// ---------------------------------------------------------------------------

impl futures::Stream for WsTransport {
    type Item = Result<Event, TransportError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            match Pin::new(&mut self.stream).poll_next(cx) {
                Poll::Ready(Some(Ok(Message::Text(text)))) => {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Event>(trimmed) {
                        Ok(event) => return Poll::Ready(Some(Ok(event))),
                        Err(e) => return Poll::Ready(Some(Err(TransportError::Json(e)))),
                    }
                }
                Poll::Ready(Some(Ok(Message::Binary(data)))) => {
                    match serde_json::from_slice::<Event>(&data) {
                        Ok(event) => return Poll::Ready(Some(Ok(event))),
                        Err(e) => return Poll::Ready(Some(Err(TransportError::Json(e)))),
                    }
                }
                Poll::Ready(Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)))) => {
                    continue
                }
                Poll::Ready(Some(Ok(Message::Close(_)))) | Poll::Ready(None) => {
                    return Poll::Ready(None)
                }
                Poll::Ready(Some(Err(e))) => {
                    return Poll::Ready(Some(Err(TransportError::WebSocket(e))))
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

impl futures::Sink<Event> for WsTransport {
    type Error = TransportError;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.sink)
            .poll_ready(cx)
            .map_err(TransportError::WebSocket)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Event) -> Result<(), Self::Error> {
        let json = serde_json::to_string(&item)?;
        Pin::new(&mut self.sink).start_send(Message::Text(json.into()))?;
        Ok(())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.sink)
            .poll_flush(cx)
            .map_err(TransportError::WebSocket)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.sink)
            .poll_close(cx)
            .map_err(TransportError::WebSocket)
    }
}
