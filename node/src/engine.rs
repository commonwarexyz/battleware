use crate::{
    aggregator, application,
    indexer::Indexer,
    seeder,
    supervisor::{EpochSupervisor, ViewSupervisor},
};
use battleware_types::{Activity, Block, Evaluation, NAMESPACE};
use commonware_broadcast::buffered;
use commonware_consensus::{
    aggregation, marshal,
    threshold_simplex::{self, Engine as Consensus},
    Reporters,
};
use commonware_cryptography::{
    bls12381::primitives::{
        group,
        poly::{public, Poly},
        variant::MinSig,
    },
    ed25519::{PrivateKey, PublicKey},
    sha256::Digest,
    Signer,
};
use commonware_p2p::{Blocker, Receiver, Sender};
use commonware_runtime::{buffer::PoolRef, Clock, Handle, Metrics, Spawner, Storage};
use commonware_utils::{NZDuration, NZUsize, NZU64};
use futures::future::try_join_all;
use governor::clock::Clock as GClock;
use governor::Quota;
use rand::{CryptoRng, Rng};
use std::{
    num::{NonZero, NonZeroUsize},
    time::Duration,
};
use tracing::{error, warn};

/// Reporter type for [threshold_simplex::Engine].
type Reporter = Reporters<Activity, marshal::Mailbox<MinSig, Block>, seeder::Mailbox>;

/// To better support peers near tip during network instability, we multiply
/// the consensus activity timeout by this factor.
const SYNCER_ACTIVITY_TIMEOUT_MULTIPLIER: u64 = 10;
const PRUNABLE_ITEMS_PER_SECTION: NonZero<u64> = NZU64!(4_096);
const IMMUTABLE_ITEMS_PER_SECTION: NonZero<u64> = NZU64!(262_144);
const FREEZER_TABLE_RESIZE_FREQUENCY: u8 = 4;
const FREEZER_TABLE_RESIZE_CHUNK_SIZE: u32 = 2u32.pow(16); // 3MB
const FREEZER_JOURNAL_TARGET_SIZE: u64 = 1024 * 1024 * 1024; // 1GB
const FREEZER_JOURNAL_COMPRESSION: Option<u8> = Some(3);
const MMR_ITEMS_PER_BLOB: NonZero<u64> = NZU64!(128_000);
const LOG_ITEMS_PER_SECTION: NonZero<u64> = NZU64!(64_000);
const LOCATIONS_ITEMS_PER_BLOB: NonZero<u64> = NZU64!(128_000);
const CERTIFICATES_ITEMS_PER_BLOB: NonZero<u64> = NZU64!(128_000);
const CACHE_ITEMS_PER_BLOB: NonZero<u64> = NZU64!(256);
const REPLAY_BUFFER: NonZero<usize> = NZUsize!(8 * 1024 * 1024); // 8MB
const WRITE_BUFFER: NonZero<usize> = NZUsize!(1024 * 1024); // 1MB
const MAX_REPAIR: u64 = 20;

/// Configuration for the [Engine].
pub struct Config<B: Blocker<PublicKey = PublicKey>, I: Indexer> {
    pub blocker: B,
    pub partition_prefix: String,
    pub blocks_freezer_table_initial_size: u32,
    pub finalized_freezer_table_initial_size: u32,
    pub buffer_pool_page_size: NonZeroUsize,
    pub buffer_pool_capacity: NonZeroUsize,
    pub signer: PrivateKey,
    pub polynomial: Poly<Evaluation>,
    pub share: group::Share,
    pub participants: Vec<PublicKey>,
    pub mailbox_size: usize,
    pub backfill_quota: Quota,
    pub deque_size: usize,

    pub leader_timeout: Duration,
    pub notarization_timeout: Duration,
    pub nullify_retry: Duration,
    pub fetch_timeout: Duration,
    pub activity_timeout: u64,
    pub skip_timeout: u64,
    pub max_fetch_count: usize,
    pub max_fetch_size: usize,
    pub fetch_concurrent: usize,
    pub fetch_rate_per_peer: Quota,

    pub indexer: I,
    pub execution_concurrency: usize,
    pub max_uploads_outstanding: usize,
}

/// The engine that drives the [application].
pub struct Engine<
    E: Clock + GClock + Rng + CryptoRng + Spawner + Storage + Metrics,
    B: Blocker<PublicKey = PublicKey>,
    I: Indexer,
> {
    context: E,

    application: application::Actor<E, I>,
    application_mailbox: application::Mailbox<E>,
    seeder: seeder::Actor<E, I>,
    seeder_mailbox: seeder::Mailbox,
    aggregator: aggregator::Actor<E, I>,
    aggregator_mailbox: aggregator::Mailbox,
    buffer: buffered::Engine<E, PublicKey, Block>,
    buffer_mailbox: buffered::Mailbox<PublicKey, Block>,
    marshal: marshal::Actor<Block, E, MinSig, PublicKey, ViewSupervisor>,
    marshal_mailbox: marshal::Mailbox<MinSig, Block>,

    #[allow(clippy::type_complexity)]
    consensus: Consensus<
        E,
        PrivateKey,
        B,
        MinSig,
        Digest,
        application::Mailbox<E>,
        application::Mailbox<E>,
        Reporter,
        ViewSupervisor,
    >,
    aggregation: aggregation::Engine<
        E,
        PublicKey,
        MinSig,
        Digest,
        aggregator::Mailbox,
        aggregator::Mailbox,
        EpochSupervisor,
        B,
        EpochSupervisor,
    >,
}

impl<
        E: Clock + GClock + Rng + CryptoRng + Spawner + Storage + Metrics,
        B: Blocker<PublicKey = PublicKey>,
        I: Indexer,
    > Engine<E, B, I>
{
    /// Create a new [Engine].
    pub async fn new(context: E, cfg: Config<B, I>) -> Self {
        // Create the buffer pool
        let buffer_pool = PoolRef::new(cfg.buffer_pool_page_size, cfg.buffer_pool_capacity);

        // Create the application
        let identity = *public::<MinSig>(&cfg.polynomial);
        let (application, view_supervisor, epoch_supervisor, application_mailbox) =
            application::Actor::new(
                context.with_label("application"),
                application::Config {
                    participants: cfg.participants.clone(),
                    polynomial: cfg.polynomial.clone(),
                    share: cfg.share.clone(),
                    mailbox_size: cfg.mailbox_size,
                    partition_prefix: format!("{}-application", cfg.partition_prefix),
                    mmr_items_per_blob: MMR_ITEMS_PER_BLOB,
                    mmr_write_buffer: WRITE_BUFFER,
                    log_items_per_section: LOG_ITEMS_PER_SECTION,
                    log_write_buffer: WRITE_BUFFER,
                    locations_items_per_blob: LOCATIONS_ITEMS_PER_BLOB,
                    buffer_pool: buffer_pool.clone(),
                    indexer: cfg.indexer.clone(),
                    execution_concurrency: cfg.execution_concurrency,
                },
            );

        // Create the seeder
        let (seeder, seeder_mailbox) = seeder::Actor::new(
            context.with_label("seeder"),
            seeder::Config {
                indexer: cfg.indexer.clone(),
                identity,
                supervisor: view_supervisor.clone(),
                namespace: NAMESPACE.to_vec(),
                public_key: cfg.signer.public_key(),
                backfill_quota: cfg.backfill_quota,
                mailbox_size: cfg.mailbox_size,
                partition_prefix: format!("{}-seeder", cfg.partition_prefix),
                items_per_blob: MMR_ITEMS_PER_BLOB,
                write_buffer: WRITE_BUFFER,
                replay_buffer: REPLAY_BUFFER,
                max_uploads_outstanding: cfg.max_uploads_outstanding,
            },
        );

        // Create the aggregator
        let (aggregator, aggregator_mailbox) = aggregator::Actor::new(
            context.with_label("aggregator"),
            aggregator::Config {
                identity,
                supervisor: view_supervisor.clone(),
                namespace: NAMESPACE.to_vec(),
                public_key: cfg.signer.public_key(),
                backfill_quota: cfg.backfill_quota,
                mailbox_size: cfg.mailbox_size,
                partition: format!("{}-aggregator", cfg.partition_prefix),
                buffer_pool: buffer_pool.clone(),
                prunable_items_per_blob: CACHE_ITEMS_PER_BLOB,
                persistent_items_per_blob: CERTIFICATES_ITEMS_PER_BLOB,
                write_buffer: WRITE_BUFFER,
                replay_buffer: REPLAY_BUFFER,
                indexer: cfg.indexer.clone(),
                max_uploads_outstanding: cfg.max_uploads_outstanding,
            },
        );

        // Create the buffer
        let (buffer, buffer_mailbox) = buffered::Engine::new(
            context.with_label("buffer"),
            buffered::Config {
                public_key: cfg.signer.public_key(),
                mailbox_size: cfg.mailbox_size,
                deque_size: cfg.deque_size,
                priority: true,
                codec_config: (),
            },
        );

        // Create marshal
        let (marshal, marshal_mailbox): (_, marshal::Mailbox<MinSig, Block>) =
            marshal::Actor::init(
                context.with_label("marshal"),
                marshal::Config {
                    public_key: cfg.signer.public_key(),
                    identity,
                    coordinator: view_supervisor.clone(),
                    partition_prefix: format!("{}-marshal", cfg.partition_prefix),
                    mailbox_size: cfg.mailbox_size,
                    backfill_quota: cfg.backfill_quota,
                    view_retention_timeout: cfg
                        .activity_timeout
                        .saturating_mul(SYNCER_ACTIVITY_TIMEOUT_MULTIPLIER),
                    namespace: NAMESPACE.to_vec(),
                    prunable_items_per_section: PRUNABLE_ITEMS_PER_SECTION,
                    immutable_items_per_section: IMMUTABLE_ITEMS_PER_SECTION,
                    freezer_table_initial_size: cfg.blocks_freezer_table_initial_size,
                    freezer_table_resize_frequency: FREEZER_TABLE_RESIZE_FREQUENCY,
                    freezer_table_resize_chunk_size: FREEZER_TABLE_RESIZE_CHUNK_SIZE,
                    freezer_journal_target_size: FREEZER_JOURNAL_TARGET_SIZE,
                    freezer_journal_compression: FREEZER_JOURNAL_COMPRESSION,
                    replay_buffer: REPLAY_BUFFER,
                    write_buffer: WRITE_BUFFER,
                    freezer_journal_buffer_pool: buffer_pool.clone(),
                    codec_config: (),
                    max_repair: MAX_REPAIR,
                },
            )
            .await;

        // Create the reporter
        let reporter = (marshal_mailbox.clone(), seeder_mailbox.clone()).into();

        // Create the consensus engine
        let consensus = Consensus::new(
            context.with_label("consensus"),
            threshold_simplex::Config {
                namespace: NAMESPACE.to_vec(),
                crypto: cfg.signer,
                automaton: application_mailbox.clone(),
                relay: application_mailbox.clone(),
                reporter,
                supervisor: view_supervisor,
                partition: format!("{}-consensus", cfg.partition_prefix),
                mailbox_size: cfg.mailbox_size,
                leader_timeout: cfg.leader_timeout,
                notarization_timeout: cfg.notarization_timeout,
                nullify_retry: cfg.nullify_retry,
                fetch_timeout: cfg.fetch_timeout,
                activity_timeout: cfg.activity_timeout,
                skip_timeout: cfg.skip_timeout,
                max_fetch_count: cfg.max_fetch_count,
                fetch_concurrent: cfg.fetch_concurrent,
                fetch_rate_per_peer: cfg.fetch_rate_per_peer,
                replay_buffer: REPLAY_BUFFER,
                write_buffer: WRITE_BUFFER,
                buffer_pool: buffer_pool.clone(),
                blocker: cfg.blocker.clone(),
            },
        );

        // Create the aggregator
        let aggregation = aggregation::Engine::new(
            context.with_label("aggregation"),
            aggregation::Config {
                monitor: epoch_supervisor.clone(),
                validators: epoch_supervisor,
                automaton: aggregator_mailbox.clone(),
                reporter: aggregator_mailbox.clone(),
                blocker: cfg.blocker,
                namespace: NAMESPACE.to_vec(),
                priority_acks: false,
                rebroadcast_timeout: NZDuration!(Duration::from_secs(10)),
                epoch_bounds: (0, 0),
                window: NZU64!(16),
                activity_timeout: cfg.activity_timeout,
                journal_partition: format!("{}-aggregation", cfg.partition_prefix),
                journal_write_buffer: WRITE_BUFFER,
                journal_replay_buffer: REPLAY_BUFFER,
                journal_heights_per_section: NZU64!(16_384),
                journal_compression: None,
                journal_buffer_pool: buffer_pool,
            },
        );

        // Return the engine
        Self {
            context,

            application,
            application_mailbox,
            seeder,
            seeder_mailbox,
            buffer,
            buffer_mailbox,
            marshal,
            marshal_mailbox,
            consensus,
            aggregator,
            aggregator_mailbox,
            aggregation,
        }
    }

    /// Start the [threshold_simplex::Engine].
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        self,
        pending_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        recovered_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        resolver_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        broadcast_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        backfill_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        seeder_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        aggregator_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        aggregation_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
    ) -> Handle<()> {
        self.context.clone().spawn(|_| {
            self.run(
                pending_network,
                recovered_network,
                resolver_network,
                broadcast_network,
                backfill_network,
                seeder_network,
                aggregator_network,
                aggregation_network,
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn run(
        self,
        pending_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        recovered_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        resolver_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        broadcast_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        backfill_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        seeder_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        aggregator_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
        aggregation_network: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
    ) {
        // If a downstream actor is started after an upstream actor (i.e. application after consensus), it is possible
        // that restart could block (as the upstream actor may fill the downstream actor's mailbox with items during initialization,
        // potentially blocking if not read).

        // Start the seeder
        let seeder_handle = self.seeder.start(seeder_network);

        // Start aggregation
        let aggregation_handle = self.aggregation.start(aggregation_network);

        // Start the aggregator
        let aggregator_handle = self.aggregator.start(aggregator_network);

        // Start the buffer
        let buffer_handle = self.buffer.start(broadcast_network);

        // Start the application
        let application_handle = self.application.start(
            self.marshal_mailbox,
            self.seeder_mailbox,
            self.aggregator_mailbox,
        );

        // Start marshal
        let marshal_handle = self.marshal.start(
            self.application_mailbox,
            self.buffer_mailbox,
            backfill_network,
        );

        // Start consensus
        let consensus_handle =
            self.consensus
                .start(pending_network, recovered_network, resolver_network);

        // Wait for any actor to finish
        if let Err(e) = try_join_all(vec![
            seeder_handle,
            aggregation_handle,
            aggregator_handle,
            buffer_handle,
            application_handle,
            marshal_handle,
            consensus_handle,
        ])
        .await
        {
            error!(?e, "engine failed");
        } else {
            warn!("engine stopped");
        }
    }
}
