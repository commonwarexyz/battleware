use battleware_types::{Activity, Seed};
use bytes::Bytes;
use commonware_consensus::{
    threshold_simplex::types::{Seedable, View},
    Reporter,
};
use commonware_resolver::{p2p::Producer, Consumer};
use commonware_utils::sequence::U64;
use futures::{
    channel::{mpsc, oneshot},
    SinkExt,
};

pub enum Message {
    Put(Seed),
    Get {
        view: View,
        response: oneshot::Sender<Seed>,
    },
    Deliver {
        view: View,
        signature: Bytes,
        response: oneshot::Sender<bool>,
    },
    Produce {
        view: View,
        response: oneshot::Sender<Bytes>,
    },
    Uploaded {
        view: View,
    },
}

#[derive(Clone)]
pub struct Mailbox {
    sender: mpsc::Sender<Message>,
}

impl Mailbox {
    pub(super) fn new(sender: mpsc::Sender<Message>) -> Self {
        Self { sender }
    }

    pub async fn put(&mut self, seed: Seed) {
        self.sender
            .send(Message::Put(seed))
            .await
            .expect("failed to send put");
    }

    pub async fn get(&mut self, view: View) -> Seed {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(Message::Get {
                view,
                response: sender,
            })
            .await
            .expect("failed to send get");
        receiver.await.expect("failed to receive get")
    }

    pub async fn uploaded(&mut self, view: View) {
        self.sender
            .send(Message::Uploaded { view })
            .await
            .expect("failed to send uploaded");
    }
}

impl Consumer for Mailbox {
    type Key = U64;
    type Value = Bytes;
    type Failure = ();

    async fn deliver(&mut self, key: Self::Key, value: Self::Value) -> bool {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(Message::Deliver {
                view: key.into(),
                signature: value,
                response: sender,
            })
            .await
            .expect("failed to send deliver");
        receiver.await.unwrap_or(true) // default to true to avoid blocking
    }

    async fn failed(&mut self, _: Self::Key, _: Self::Failure) {
        // We don't need to do anything on failure, the resolver will retry.
    }
}

impl Producer for Mailbox {
    type Key = U64;

    async fn produce(&mut self, key: Self::Key) -> oneshot::Receiver<Bytes> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(Message::Produce {
                view: key.into(),
                response: sender,
            })
            .await
            .expect("failed to send produce");
        receiver
    }
}

impl Reporter for Mailbox {
    type Activity = Activity;

    async fn report(&mut self, activity: Self::Activity) {
        match activity {
            Activity::Notarization(notarization) => {
                self.put(notarization.seed()).await;
            }
            Activity::Nullification(nullification) => {
                self.put(nullification.seed()).await;
            }
            Activity::Finalization(finalization) => {
                self.put(finalization.seed()).await;
            }
            _ => {}
        }
    }
}
