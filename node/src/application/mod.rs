use std::num::NonZero;

use crate::indexer::Indexer;
use battleware_types::Evaluation;
use commonware_cryptography::{
    bls12381::primitives::{group, poly::Poly},
    ed25519::PublicKey,
};

mod actor;
pub use actor::Actor;
mod ingress;
use commonware_runtime::buffer::PoolRef;
pub use ingress::Mailbox;
mod mempool;

/// Configuration for the application.
pub struct Config<I: Indexer> {
    /// Participants active in consensus.
    pub participants: Vec<PublicKey>,

    /// The unevaluated group polynomial associated with the current dealing.
    pub polynomial: Poly<Evaluation>,

    /// The share of the secret.
    pub share: group::Share,

    /// Number of messages from consensus to hold in our backlog
    /// before blocking.
    pub mailbox_size: usize,

    /// The prefix for the partition.
    pub partition_prefix: String,

    /// The number of items per blob for the MMR.
    pub mmr_items_per_blob: NonZero<u64>,

    /// The number of items per write for the MMR.
    pub mmr_write_buffer: NonZero<usize>,

    /// The number of items per section for the log.
    pub log_items_per_section: NonZero<u64>,

    /// The number of items per write for the log.
    pub log_write_buffer: NonZero<usize>,

    /// The number of items per blob for the locations.
    pub locations_items_per_blob: NonZero<u64>,

    /// The buffer pool to use.
    pub buffer_pool: PoolRef,

    /// The indexer to upload to.
    pub indexer: I,

    /// The number of threads to use for execution.
    pub execution_concurrency: usize,
}
