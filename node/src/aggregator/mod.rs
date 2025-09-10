mod actor;
mod ingress;

use crate::{indexer::Indexer, supervisor::ViewSupervisor};
pub use actor::Actor;
use battleware_types::Identity;
use commonware_cryptography::ed25519::PublicKey;
use commonware_runtime::buffer::PoolRef;
use governor::Quota;
pub use ingress::{Mailbox, Message};
use std::num::NonZero;

pub struct Config<I: Indexer> {
    pub namespace: Vec<u8>,
    pub supervisor: ViewSupervisor,
    pub public_key: PublicKey,
    pub identity: Identity,
    pub backfill_quota: Quota,
    pub mailbox_size: usize,
    pub partition: String,
    pub prunable_items_per_blob: NonZero<u64>,
    pub persistent_items_per_blob: NonZero<u64>,
    pub write_buffer: NonZero<usize>,
    pub replay_buffer: NonZero<usize>,
    pub buffer_pool: PoolRef,
    pub indexer: I,
    pub max_uploads_outstanding: usize,
}
