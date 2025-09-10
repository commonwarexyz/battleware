use crate::{state_transition, Adb};
use battleware_types::{
    api::Summary,
    execution::{Output, Progress, Seed, Transaction, Value},
    Identity, NAMESPACE,
};
use commonware_consensus::{
    aggregation::types::{Ack, Certificate, Item},
    simplex::types::view_message,
    threshold_simplex::types::seed_namespace,
};
use commonware_cryptography::{
    bls12381::primitives::{
        group::{Private, Share},
        ops,
        variant::{MinSig, Variant},
    },
    ed25519::{PrivateKey, PublicKey},
    sha256::{Digest, Sha256},
    Digestible, Hasher, PrivateKeyExt, Signer,
};
#[cfg(feature = "parallel")]
use commonware_runtime::ThreadPool;
use commonware_runtime::{buffer::PoolRef, Clock, Metrics, Spawner, Storage};
use commonware_storage::{
    adb::{self, keyless},
    translator::EightCap,
};
use commonware_utils::{NZUsize, NZU64};
use rand::{rngs::StdRng, SeedableRng};

/// Creates a master keypair for BLS signatures used in consensus
pub fn create_network_keypair() -> (Private, <MinSig as Variant>::Public) {
    let mut rng = StdRng::seed_from_u64(0);
    ops::keypair::<_, MinSig>(&mut rng)
}

/// Creates an account keypair for Ed25519 signatures used by users
pub fn create_account_keypair(seed: u64) -> (PrivateKey, PublicKey) {
    let mut rng = StdRng::seed_from_u64(seed);
    let private = PrivateKey::from_rng(&mut rng);
    let public = private.public_key();
    (private, public)
}

/// Creates a test seed for consensus
pub fn create_seed(network_secret: &Private, view: u64) -> Seed {
    let seed_namespace = seed_namespace(NAMESPACE);
    let message = view_message(view);
    Seed::new(
        view,
        ops::sign_message::<MinSig>(network_secret, Some(&seed_namespace), &message),
    )
}

/// Creates state and events databases for testing
pub async fn create_adbs<E: Spawner + Metrics + Storage + Clock>(
    context: &E,
) -> (Adb<E, EightCap>, keyless::Keyless<E, Output, Sha256>) {
    let buffer_pool = PoolRef::new(NZUsize!(1024), NZUsize!(1024));

    let state = Adb::init(
        context.with_label("state"),
        adb::any::variable::Config {
            mmr_journal_partition: String::from("state-mmr-journal"),
            mmr_metadata_partition: String::from("state-mmr-metadata"),
            mmr_items_per_blob: NZU64!(1024),
            mmr_write_buffer: NZUsize!(1024),
            log_journal_partition: String::from("state-log-journal"),
            log_items_per_section: NZU64!(1024),
            log_write_buffer: NZUsize!(1024),
            log_compression: None,
            log_codec_config: (),
            locations_journal_partition: String::from("state-locations-journal"),
            locations_items_per_blob: NZU64!(1024),
            translator: EightCap,
            thread_pool: None,
            buffer_pool: buffer_pool.clone(),
        },
    )
    .await
    .expect("Failed to initialize state ADB");

    let events = keyless::Keyless::<_, Output, Sha256>::init(
        context.with_label("events"),
        keyless::Config {
            mmr_journal_partition: String::from("events-mmr-journal"),
            mmr_metadata_partition: String::from("events-mmr-metadata"),
            mmr_items_per_blob: NZU64!(1024),
            mmr_write_buffer: NZUsize!(1024),
            log_journal_partition: String::from("events-log-journal"),
            log_items_per_section: NZU64!(1024),
            log_write_buffer: NZUsize!(1024),
            log_compression: None,
            log_codec_config: (),
            locations_journal_partition: String::from("events-locations-journal"),
            locations_items_per_blob: NZU64!(1024),
            locations_write_buffer: NZUsize!(1024),
            thread_pool: None,
            buffer_pool,
        },
    )
    .await
    .expect("Failed to initialize events Keyless");

    (state, events)
}

/// Helper to create a summary with transactions
pub async fn execute_block<E: Spawner + Metrics + Storage + Clock>(
    network_secret: &Private,
    network_identity: Identity,
    state: &mut Adb<E, EightCap>,
    events: &mut keyless::Keyless<E, Output, Sha256>,
    view: u64,
    txs: Vec<Transaction>,
) -> (Seed, Summary) {
    // Get height from state
    let current_height = state
        .get_metadata()
        .await
        .unwrap()
        .and_then(|(_, v)| match v {
            Some(Value::Commit { height, start: _ }) => Some(height),
            _ => None,
        })
        .unwrap_or(0);
    let height = current_height + 1;

    // Create seed
    let seed = create_seed(network_secret, view);

    // Execute state transition
    #[cfg(feature = "parallel")]
    let pool = ThreadPool::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .expect("failed to create execution pool"),
    );
    let result = state_transition::execute_state_transition(
        state,
        events,
        network_identity,
        height,
        seed.clone(),
        txs,
        #[cfg(feature = "parallel")]
        pool,
    )
    .await;

    // Sync results
    state.sync().await.unwrap();
    events.sync().await.unwrap();

    // Generate proofs
    let state_proof_ops = result.state_end_op - result.state_start_op;
    let (state_proof, state_proof_ops) = state
        .historical_proof(result.state_end_op, result.state_start_op, state_proof_ops)
        .await
        .unwrap();
    let events_proof_ops = result.events_end_op - result.events_start_op;
    let (events_proof, events_proof_ops) = events
        .historical_proof(
            result.events_end_op,
            result.events_start_op,
            NZU64!(events_proof_ops),
        )
        .await
        .unwrap();

    // Create progress
    let progress = Progress::new(
        view,
        height,
        Sha256::hash(&height.to_be_bytes()),
        result.state_root,
        result.state_start_op,
        result.state_end_op,
        result.events_root,
        result.events_start_op,
        result.events_end_op,
    );

    // Create certificate
    let item = Item {
        index: height,
        digest: progress.digest(),
    };
    let ack = Ack::<MinSig, Digest>::sign(
        NAMESPACE,
        0,
        &Share {
            index: 0,
            private: network_secret.clone(),
        },
        item.clone(),
    );
    let certificate = Certificate::<MinSig, Digest> {
        item,
        signature: ack.signature.value,
    };

    // Create summary
    (
        seed,
        Summary {
            progress,
            certificate,
            state_proof,
            state_proof_ops,
            events_proof,
            events_proof_ops,
        },
    )
}
