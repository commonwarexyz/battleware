use battleware_execution::state_transition::StateTransitionResult;
use battleware_types::execution::{Output, Value};
use bytes::Bytes;
use commonware_consensus::{
    aggregation::types::{Activity, Certificate, Index},
    threshold_simplex::types::View,
    Automaton, Reporter,
};
use commonware_cryptography::{bls12381::primitives::variant::MinSig, sha256::Digest};
use commonware_resolver::{p2p::Producer, Consumer};
use commonware_storage::{
    mmr::verification::Proof,
    store::operation::{Keyless, Variable},
};
use commonware_utils::sequence::U64;
use futures::{
    channel::{mpsc, oneshot},
    SinkExt,
};

pub enum Message {
    Executed {
        view: View,
        height: u64,
        commitment: Digest,
        result: StateTransitionResult,
        state_proof: Proof<Digest>,
        state_proof_ops: Vec<Variable<Digest, Value>>,
        events_proof: Proof<Digest>,
        events_proof_ops: Vec<Keyless<Output>>,
        response: oneshot::Sender<()>,
    },
    Genesis {
        response: oneshot::Sender<Digest>,
    },
    Propose {
        index: Index,
        response: oneshot::Sender<Digest>,
    },
    Verify {
        index: Index,
        payload: Digest,
        response: oneshot::Sender<bool>,
    },
    Tip {
        index: Index,
    },
    Certified {
        certificate: Certificate<MinSig, Digest>,
    },
    Deliver {
        index: Index,
        certificate: Bytes,
        response: oneshot::Sender<bool>,
    },
    Produce {
        index: Index,
        response: oneshot::Sender<Bytes>,
    },
    Uploaded {
        index: Index,
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

    pub(super) async fn uploaded(&mut self, index: Index) {
        self.sender
            .send(Message::Uploaded { index })
            .await
            .expect("failed to send uploaded");
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn executed(
        &mut self,
        view: View,
        height: u64,
        commitment: Digest,
        result: StateTransitionResult,
        state_proof: Proof<Digest>,
        state_proof_ops: Vec<Variable<Digest, Value>>,
        events_proof: Proof<Digest>,
        events_proof_ops: Vec<Keyless<Output>>,
        response: oneshot::Sender<()>,
    ) {
        self.sender
            .send(Message::Executed {
                view,
                height,
                commitment,
                result,
                state_proof,
                state_proof_ops,
                events_proof,
                events_proof_ops,
                response,
            })
            .await
            .expect("failed to send executed");
    }
}

impl Automaton for Mailbox {
    type Digest = Digest;
    type Context = Index;

    async fn genesis(&mut self) -> Self::Digest {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Message::Genesis { response })
            .await
            .expect("Failed to send aggregation genesis");
        receiver
            .await
            .expect("Failed to receive aggregation genesis")
    }

    async fn propose(&mut self, context: Self::Context) -> oneshot::Receiver<Self::Digest> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Message::Propose {
                index: context,
                response,
            })
            .await
            .expect("Failed to send aggregation propose");
        receiver
    }

    async fn verify(
        &mut self,
        context: Self::Context,
        payload: Self::Digest,
    ) -> oneshot::Receiver<bool> {
        let (response, receiver) = oneshot::channel();
        self.sender
            .send(Message::Verify {
                index: context,
                payload,
                response,
            })
            .await
            .expect("Failed to send aggregation verify");
        receiver
    }
}

impl Reporter for Mailbox {
    type Activity = Activity<MinSig, Digest>;

    async fn report(&mut self, activity: Self::Activity) {
        match activity {
            Activity::Certified(certificate) => {
                self.sender
                    .send(Message::Certified { certificate })
                    .await
                    .expect("Failed to send aggregation certified");
            }
            Activity::Tip(index) => {
                self.sender
                    .send(Message::Tip { index })
                    .await
                    .expect("Failed to send aggregation tip");
            }
            _ => {}
        }
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
                index: key.into(),
                certificate: value,
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
                index: key.into(),
                response: sender,
            })
            .await
            .expect("failed to send produce");
        receiver
    }
}
