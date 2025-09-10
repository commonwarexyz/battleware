use crate::{Adb, Layer, State};
use battleware_types::{
    execution::{Output, Seed, Transaction, Value},
    Identity, NAMESPACE,
};
use commonware_cryptography::{ed25519::PublicKey, sha256::Digest, Sha256};
#[cfg(feature = "parallel")]
use commonware_runtime::ThreadPool;
use commonware_runtime::{Clock, Metrics, Spawner, Storage};
use commonware_storage::{adb::keyless, mmr::hasher::Standard, translator::Translator};
use std::collections::BTreeMap;

/// Result of executing a block's state transition
pub struct StateTransitionResult {
    pub state_root: Digest,
    pub state_start_op: u64,
    pub state_end_op: u64,
    pub events_root: Digest,
    pub events_start_op: u64,
    pub events_end_op: u64,
    /// Map of public keys to their next expected nonce after processing
    pub processed_nonces: BTreeMap<PublicKey, u64>,
}

/// Execute state transition for a block
///
/// This function processes all transactions in a block, updating both state and events
/// databases. It handles transaction nonce validation, execution, and persistence.
/// Only processes the block if it's the next expected height.
///
/// Returns the resulting state and events roots along with their operation counts,
/// plus a map of processed public keys to their next expected nonces.
pub async fn execute_state_transition<S: Spawner + Storage + Clock + Metrics, T: Translator>(
    state: &mut Adb<S, T>,
    events: &mut keyless::Keyless<S, Output, Sha256>,
    identity: Identity,
    height: u64,
    seed: Seed,
    transactions: Vec<Transaction>,
    #[cfg(feature = "parallel")] pool: ThreadPool,
) -> StateTransitionResult {
    // Check if this is the next expected height for state
    let (state_height, mut state_start_op) = state
        .get_metadata()
        .await
        .unwrap()
        .and_then(|(_, v)| match v {
            Some(Value::Commit { height, start }) => Some((height, start)),
            _ => None,
        })
        .unwrap_or((0, 0));
    assert!(
        height == state_height || height == state_height + 1,
        "state transition must be for next block or tip"
    );

    // Get events metadata
    let (events_height, mut events_start_op) = events
        .get_metadata()
        .await
        .unwrap()
        .and_then(|(_, v)| match v {
            Some(Output::Commit { height, start }) => Some((height, start)),
            _ => None,
        })
        .unwrap_or((0, 0));

    // Only process if this is the next block
    let mut processed_nonces = BTreeMap::new();
    if height == state_height + 1 {
        state_start_op = state.op_count();
        let mut layer = Layer::new(state, identity, NAMESPACE, seed);
        let (outputs, nonces) = layer
            .execute(
                #[cfg(feature = "parallel")]
                pool,
                transactions,
            )
            .await;
        processed_nonces.extend(nonces);

        // Apply events if this is the next block
        if height == events_height + 1 {
            events_start_op = events.op_count();
            for output in outputs.into_iter() {
                events.append(output).await.unwrap();
            }
            events
                .commit(Some(Output::Commit {
                    height,
                    start: events_start_op,
                }))
                .await
                .unwrap();
        }

        // Apply state once we've committed events (can't regenerate after state updated)
        state.apply(layer.commit()).await;
        state
            .commit(Some(Value::Commit {
                height,
                start: state_start_op,
            }))
            .await
            .unwrap();
    }

    // Compute roots
    let mut mmr_hasher = Standard::<Sha256>::new();
    let state_root = state.root(&mut mmr_hasher);
    let state_end_op = state.op_count();
    let events_root = events.root(&mut mmr_hasher);
    let events_end_op = events.op_count();

    StateTransitionResult {
        state_root,
        state_start_op,
        state_end_op,
        events_root,
        events_start_op,
        events_end_op,
        processed_nonces,
    }
}
