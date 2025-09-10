use battleware_types::{leader_index, Evaluation, Identity, Signature};
use commonware_codec::Encode;
use commonware_consensus::{
    aggregation::types::Epoch, threshold_simplex::types::View, Monitor, Supervisor as Su,
    ThresholdSupervisor as TSu,
};
use commonware_cryptography::{
    bls12381::{
        dkg::ops::evaluate_all,
        primitives::{
            group,
            poly::{self, Poly},
            variant::MinSig,
        },
    },
    ed25519,
};
use commonware_resolver::p2p;
use commonware_runtime::RwLock;
use futures::{channel::mpsc, SinkExt};
use std::{collections::HashMap, sync::Arc};

/// Manages epoch state and subscribers.
struct EpochManager {
    epoch: Epoch,
    subscribers: Vec<mpsc::Sender<Epoch>>,
}

impl EpochManager {
    fn new() -> Self {
        Self {
            epoch: 0,
            subscribers: Vec::new(),
        }
    }

    async fn update(&mut self, epoch: Epoch) {
        // Update epoch
        self.epoch = epoch;

        // Notify all subscribers
        let mut i = 0;
        while i < self.subscribers.len() {
            if self.subscribers[i].send(epoch).await.is_err() {
                // Remove disconnected subscriber
                self.subscribers.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    async fn subscribe(&mut self) -> (Epoch, mpsc::Receiver<Epoch>) {
        let (tx, rx) = mpsc::channel(1);
        self.subscribers.push(tx);
        (self.epoch, rx)
    }

    fn current(&self) -> Epoch {
        self.epoch
    }
}

/// Core supervisor data shared between View and Epoch supervisors.
pub struct Supervisor {
    identity: Identity,
    polynomial: Vec<Evaluation>,
    participants: Vec<ed25519::PublicKey>,
    participants_map: HashMap<ed25519::PublicKey, u32>,
    share: group::Share,
    epoch_manager: RwLock<EpochManager>,
}

impl Supervisor {
    /// Create a new supervisor.
    pub fn new(
        polynomial: Poly<Evaluation>,
        mut participants: Vec<ed25519::PublicKey>,
        share: group::Share,
    ) -> Arc<Self> {
        // Setup participants
        participants.sort();
        let mut participants_map = HashMap::new();
        for (index, validator) in participants.iter().enumerate() {
            participants_map.insert(validator.clone(), index as u32);
        }
        let identity = *poly::public::<MinSig>(&polynomial);
        let polynomial = evaluate_all::<MinSig>(&polynomial, participants.len() as u32);

        // Return supervisor
        Arc::new(Self {
            identity,
            polynomial,
            participants,
            participants_map,
            share,
            epoch_manager: RwLock::new(EpochManager::new()),
        })
    }
}

/// View-based [Supervisor] for [commonware_consensus::threshold_simplex].
#[derive(Clone)]
pub struct ViewSupervisor {
    inner: Arc<Supervisor>,
}

impl ViewSupervisor {
    pub fn new(supervisor: Arc<Supervisor>) -> Self {
        Self { inner: supervisor }
    }
}

impl p2p::Coordinator for ViewSupervisor {
    type PublicKey = ed25519::PublicKey;

    fn peers(&self) -> &Vec<Self::PublicKey> {
        &self.inner.participants
    }

    fn peer_set_id(&self) -> u64 {
        // Block on getting the current epoch
        futures::executor::block_on(async { self.inner.epoch_manager.read().await.current() })
    }
}

impl Su for ViewSupervisor {
    type Index = View;
    type PublicKey = ed25519::PublicKey;

    fn leader(&self, _: Self::Index) -> Option<Self::PublicKey> {
        unimplemented!("only defined in supertrait")
    }

    fn participants(&self, _: Self::Index) -> Option<&Vec<Self::PublicKey>> {
        Some(&self.inner.participants)
    }

    fn is_participant(&self, _: Self::Index, candidate: &Self::PublicKey) -> Option<u32> {
        self.inner.participants_map.get(candidate).cloned()
    }
}

impl TSu for ViewSupervisor {
    type Seed = Signature;
    type Identity = Identity;
    type Polynomial = Vec<Evaluation>;
    type Share = group::Share;

    fn leader(&self, _: Self::Index, seed: Self::Seed) -> Option<Self::PublicKey> {
        let seed_bytes = seed.encode();
        let index = leader_index(seed_bytes.as_ref(), self.inner.participants.len());
        Some(self.inner.participants[index].clone())
    }

    fn identity(&self) -> &Self::Identity {
        &self.inner.identity
    }

    fn polynomial(&self, _: Self::Index) -> Option<&Self::Polynomial> {
        Some(&self.inner.polynomial)
    }

    fn share(&self, _: Self::Index) -> Option<&Self::Share> {
        Some(&self.inner.share)
    }
}

/// Epoch-based [Supervisor] for [commonware_consensus::aggregation].
#[derive(Clone)]
pub struct EpochSupervisor {
    inner: Arc<Supervisor>,
}

impl EpochSupervisor {
    pub fn new(supervisor: Arc<Supervisor>) -> Self {
        Self { inner: supervisor }
    }

    pub async fn update(&self, epoch: Epoch) {
        self.inner.epoch_manager.write().await.update(epoch).await;
    }
}

impl Su for EpochSupervisor {
    type Index = Epoch;
    type PublicKey = ed25519::PublicKey;

    fn leader(&self, _: Self::Index) -> Option<Self::PublicKey> {
        unimplemented!("only defined in supertrait")
    }

    fn participants(&self, _: Self::Index) -> Option<&Vec<Self::PublicKey>> {
        Some(&self.inner.participants)
    }

    fn is_participant(&self, _: Self::Index, candidate: &Self::PublicKey) -> Option<u32> {
        self.inner.participants_map.get(candidate).cloned()
    }
}

impl TSu for EpochSupervisor {
    type Identity = Identity;
    type Polynomial = Vec<Evaluation>;
    type Seed = Signature;
    type Share = group::Share;

    fn leader(&self, _: Self::Index, seed: Self::Seed) -> Option<Self::PublicKey> {
        let seed_bytes = seed.encode();
        let index = leader_index(seed_bytes.as_ref(), self.inner.participants.len());
        Some(self.inner.participants[index].clone())
    }

    fn identity(&self) -> &Self::Identity {
        &self.inner.identity
    }

    fn polynomial(&self, _: Self::Index) -> Option<&Self::Polynomial> {
        Some(&self.inner.polynomial)
    }

    fn share(&self, _: Self::Index) -> Option<&Self::Share> {
        Some(&self.inner.share)
    }
}

impl Monitor for EpochSupervisor {
    type Index = Epoch;

    async fn subscribe(&mut self) -> (Self::Index, mpsc::Receiver<Self::Index>) {
        self.inner.epoch_manager.write().await.subscribe().await
    }
}
