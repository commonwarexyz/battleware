pub mod api;
pub use api::Query;
pub mod execution;
use commonware_cryptography::{
    sha256::{Digest, Sha256},
    Digestible, Hasher,
};
pub use execution::{
    leader_index, Activity, Block, Evaluation, Finalization, Finalized, Identity, Notarization,
    Notarized, Seed, Signature, NAMESPACE,
};

/// Genesis message to use during initialization.
const GENESIS: &[u8] = b"commonware is neat";

/// Get the genesis block.
pub fn genesis_block() -> Block {
    let genesis_parent = Sha256::hash(GENESIS);
    Block::new(genesis_parent, 0, 0, vec![])
}

/// Compute the digest of the genesis block.
pub fn genesis_digest() -> Digest {
    genesis_block().digest()
}
