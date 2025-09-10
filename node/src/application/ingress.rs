use battleware_types::{Block, Seed};
use commonware_consensus::threshold_simplex::types::{Context, View};
use commonware_consensus::{Automaton, Relay, Reporter};
use commonware_cryptography::sha256::Digest;
use commonware_runtime::{telemetry::metrics::histogram, Clock};
use futures::{
    channel::{mpsc, oneshot},
    SinkExt,
};

/// Messages sent to the application.
pub enum Message<E: Clock> {
    Genesis {
        response: oneshot::Sender<Digest>,
    },
    Propose {
        view: View,
        parent: (View, Digest),
        response: oneshot::Sender<Digest>,
    },
    Ancestry {
        view: View,
        blocks: Vec<Block>,
        timer: histogram::Timer<E>,
        response: oneshot::Sender<Digest>,
    },
    Broadcast {
        payload: Digest,
    },
    Verify {
        view: View,
        parent: (View, Digest),
        payload: Digest,
        response: oneshot::Sender<bool>,
    },
    Finalized {
        block: Block,
        response: oneshot::Sender<()>,
    },
    Seeded {
        block: Block,
        seed: Seed,
        timer: histogram::Timer<E>,
        response: oneshot::Sender<()>,
    },
}

/// Mailbox for the application.
#[derive(Clone)]
pub struct Mailbox<E: Clock> {
    sender: mpsc::Sender<Message<E>>,
}

impl<E: Clock> Mailbox<E> {
    pub(super) fn new(sender: mpsc::Sender<Message<E>>) -> Self {
        Self { sender }
    }

    pub(super) async fn ancestry(
        &mut self,
        view: View,
        blocks: Vec<Block>,
        timer: histogram::Timer<E>,
        response: oneshot::Sender<Digest>,
    ) {
        self.sender
            .send(Message::Ancestry {
                view,
                blocks,
                timer,
                response,
            })
            .await
            .expect("Failed to send ancestry");
    }

    pub(super) async fn seeded(
        &mut self,
        block: Block,
        seed: Seed,
        timer: histogram::Timer<E>,
        response: oneshot::Sender<()>,
    ) {
        self.sender
            .send(Message::Seeded {
                block,
                seed,
                timer,
                response,
            })
            .await
            .expect("Failed to send seeded");
    }
}

impl<E: Clock> Automaton for Mailbox<E> {
    type Digest = Digest;
    type Context = Context<Self::Digest>;

    async fn genesis(&mut self) -> Self::Digest {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Message::Genesis { response })
            .await
            .expect("Failed to send genesis");
        receiver.await.expect("Failed to receive genesis")
    }

    async fn propose(&mut self, context: Context<Self::Digest>) -> oneshot::Receiver<Self::Digest> {
        // If we linked payloads to their parent, we would include
        // the parent in the `Context` in the payload.
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Message::Propose {
                view: context.view,
                parent: context.parent,
                response,
            })
            .await
            .expect("Failed to send propose");
        receiver
    }

    async fn verify(
        &mut self,
        context: Context<Self::Digest>,
        payload: Self::Digest,
    ) -> oneshot::Receiver<bool> {
        // If we linked payloads to their parent, we would verify
        // the parent included in the payload matches the provided `Context`.
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Message::Verify {
                view: context.view,
                parent: context.parent,
                payload,
                response,
            })
            .await
            .expect("Failed to send verify");
        receiver
    }
}

impl<E: Clock> Relay for Mailbox<E> {
    type Digest = Digest;

    async fn broadcast(&mut self, digest: Self::Digest) {
        self.sender
            .send(Message::Broadcast { payload: digest })
            .await
            .expect("Failed to send broadcast");
    }
}

impl<E: Clock> Reporter for Mailbox<E> {
    type Activity = Block;

    async fn report(&mut self, block: Self::Activity) {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Message::Finalized { block, response })
            .await
            .expect("Failed to send finalized");

        // Wait for the item to be processed (used to increment "save point" in marshal)
        let _ = receiver.await; // TODO: may be shutting down
    }
}
