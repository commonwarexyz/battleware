use crate::{Error, Result};
use battleware_types::{
    api::{Events, Update},
    Identity, Seed, NAMESPACE,
};
use commonware_codec::ReadExt;
use futures_util::{Stream as FutStream, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::{tungstenite::Message, WebSocketStream};
use tracing::{debug, error};

/// Stream of events from the WebSocket connection
pub struct Stream<T: ReadExt + Send + Sync + 'static> {
    receiver: mpsc::UnboundedReceiver<Result<T>>,
    _handle: tokio::task::JoinHandle<()>,
}

/// Trait for verifying consensus messages
pub trait Verifiable {
    fn verify(&self, identity: &Identity) -> bool;
}

impl Verifiable for Seed {
    fn verify(&self, identity: &Identity) -> bool {
        self.verify(NAMESPACE, identity)
    }
}

impl Verifiable for Events {
    fn verify(&self, identity: &Identity) -> bool {
        self.verify(identity)
    }
}

impl Verifiable for Update {
    fn verify(&self, identity: &Identity) -> bool {
        match self {
            Update::Seed(seed) => seed.verify(NAMESPACE, identity),
            Update::Events(events) => events.verify(identity),
            Update::FilteredEvents(events) => events.verify(identity),
        }
    }
}

impl<T: ReadExt + Send + Sync + 'static> Stream<T> {
    pub(crate) fn new<S>(mut ws: WebSocketStream<S>) -> Self
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let (tx, rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(async move {
            while let Some(msg) = ws.next().await {
                match msg {
                    Ok(Message::Binary(data)) => {
                        debug!("Received binary message: {} bytes", data.len());
                        let mut buf = data.as_slice();
                        match T::read(&mut buf) {
                            Ok(event) => {
                                if tx.send(Ok(event)).is_err() {
                                    break; // Receiver dropped
                                }
                            }
                            Err(e) => {
                                error!("Failed to decode event: {}", e);
                                let err = Error::InvalidData(e);
                                if tx.send(Err(err)).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        debug!("WebSocket closed");
                        let _ = tx.send(Err(Error::ConnectionClosed));
                        break;
                    }
                    Ok(_) => {} // Ignore other message types
                    Err(e) => {
                        error!("WebSocket error: {}", e);
                        let _ = tx.send(Err(e.into()));
                        break;
                    }
                }
            }
        });

        Self {
            receiver: rx,
            _handle: handle,
        }
    }

    pub(crate) fn new_with_verifier<S>(mut ws: WebSocketStream<S>, identity: Identity) -> Self
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
        T: Verifiable,
    {
        let (tx, rx) = mpsc::unbounded_channel();

        let handle = tokio::spawn(async move {
            while let Some(msg) = ws.next().await {
                match msg {
                    Ok(Message::Binary(data)) => {
                        debug!("Received binary message: {} bytes", data.len());
                        let mut buf = data.as_slice();
                        match T::read(&mut buf) {
                            Ok(event) => {
                                // Verify the message
                                if !event.verify(&identity) {
                                    error!("Failed to verify consensus message");
                                    let err = Error::InvalidSignature;
                                    if tx.send(Err(err)).is_err() {
                                        break;
                                    }
                                    continue;
                                }

                                if tx.send(Ok(event)).is_err() {
                                    break; // Receiver dropped
                                }
                            }
                            Err(e) => {
                                error!("Failed to decode event: {}", e);
                                let err = Error::InvalidData(e);
                                if tx.send(Err(err)).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Ok(Message::Close(_)) => {
                        debug!("WebSocket closed");
                        let _ = tx.send(Err(Error::ConnectionClosed));
                        break;
                    }
                    Ok(_) => {} // Ignore other message types
                    Err(e) => {
                        error!("WebSocket error: {}", e);
                        let _ = tx.send(Err(e.into()));
                        break;
                    }
                }
            }
        });

        Self {
            receiver: rx,
            _handle: handle,
        }
    }

    /// Receive the next event from the stream
    pub async fn next(&mut self) -> Option<Result<T>> {
        self.receiver.recv().await
    }
}

impl<T: ReadExt + Send + Sync + 'static> FutStream for Stream<T> {
    type Item = Result<T>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx)
    }
}
