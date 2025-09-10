use super::{
    ingress::{Mailbox, Message},
    Config,
};
use crate::{
    aggregator,
    application::mempool::Mempool,
    indexer::Indexer,
    seeder,
    supervisor::{EpochSupervisor, Supervisor, ViewSupervisor},
};
use battleware_execution::{nonce, state_transition, Adb, Noncer};
use battleware_types::{
    execution::{Output, Value, MAX_BLOCK_TRANSACTIONS},
    genesis_block, genesis_digest, Block, Identity,
};
use commonware_consensus::{marshal, threshold_simplex::types::View};
use commonware_cryptography::{
    bls12381::primitives::{poly::public, variant::MinSig},
    ed25519::Batch,
    sha256::Digest,
    BatchVerifier, Committable, Digestible, Sha256,
};
use commonware_macros::select;
use commonware_runtime::{
    buffer::PoolRef, telemetry::metrics::histogram, Clock, Handle, Metrics, Spawner, Storage,
    ThreadPool,
};
use commonware_storage::{
    adb::{self, keyless},
    translator::EightCap,
};
use commonware_utils::{futures::ClosedExt, NZU64};
use futures::StreamExt;
use futures::{channel::mpsc, future::try_join};
use futures::{future, future::Either};
use prometheus_client::metrics::{counter::Counter, histogram::Histogram};
use rand::{CryptoRng, Rng};
use std::{
    num::NonZero,
    sync::{atomic::AtomicU64, Arc, Mutex},
};
use tracing::{debug, info, warn};

/// Histogram buckets for application latency.
const LATENCY: [f64; 20] = [
    0.001, 0.002, 0.003, 0.004, 0.005, 0.0075, 0.010, 0.015, 0.020, 0.025, 0.030, 0.050, 0.075,
    0.100, 0.200, 0.500, 1.0, 2.0, 5.0, 10.0,
];

/// Attempt to prune the state every 10000 blocks (randomly).
const PRUNE_INTERVAL: u64 = 10_000;

// TODO: store the outputs of previously computed state to avoid a ton of recomputation.
async fn ancestry(
    mut marshal: marshal::Mailbox<MinSig, Block>,
    start: (Option<View>, Digest),
    end: u64,
) -> Option<Vec<Block>> {
    let mut ancestry = Vec::new();

    // Get the start block
    let Ok(block) = marshal.subscribe(start.0, start.1).await.await else {
        return None;
    };
    let mut next = (block.height.saturating_sub(1), block.parent);
    ancestry.push(block);

    // Recurse until reaching the end height
    while next.0 > end {
        let request = marshal.subscribe(None, next.1).await;
        let Ok(block) = request.await else {
            return None;
        };
        next = (block.height.saturating_sub(1), block.parent);
        ancestry.push(block);
    }

    // Reverse the ancestry
    Some(ancestry.into_iter().rev().collect())
}

/// Application actor.
pub struct Actor<R: Rng + CryptoRng + Spawner + Metrics + Clock + Storage, I: Indexer> {
    context: R,
    inbound: Mailbox<R>,
    mailbox: mpsc::Receiver<Message<R>>,
    identity: Identity,
    partition_prefix: String,
    mmr_items_per_blob: NonZero<u64>,
    mmr_write_buffer: NonZero<usize>,
    log_items_per_section: NonZero<u64>,
    log_write_buffer: NonZero<usize>,
    locations_items_per_blob: NonZero<u64>,
    buffer_pool: PoolRef,
    indexer: I,
    execution_concurrency: usize,
}

impl<R: Rng + CryptoRng + Spawner + Metrics + Clock + Storage, I: Indexer> Actor<R, I> {
    /// Create a new application actor.
    pub fn new(
        context: R,
        config: Config<I>,
    ) -> (Self, ViewSupervisor, EpochSupervisor, Mailbox<R>) {
        // Create actor
        let (sender, mailbox) = mpsc::channel(config.mailbox_size);
        let inbound = Mailbox::new(sender);

        // Create supervisors
        let identity = *public::<MinSig>(&config.polynomial);
        let supervisor = Supervisor::new(config.polynomial, config.participants, config.share);
        let view_supervisor = ViewSupervisor::new(supervisor.clone());
        let epoch_supervisor = EpochSupervisor::new(supervisor);

        (
            Self {
                context,
                mailbox,
                inbound: inbound.clone(),
                identity,
                partition_prefix: config.partition_prefix,
                mmr_items_per_blob: config.mmr_items_per_blob,
                mmr_write_buffer: config.mmr_write_buffer,
                log_items_per_section: config.log_items_per_section,
                log_write_buffer: config.log_write_buffer,
                locations_items_per_blob: config.locations_items_per_blob,
                buffer_pool: config.buffer_pool,
                indexer: config.indexer,
                execution_concurrency: config.execution_concurrency,
            },
            view_supervisor,
            epoch_supervisor,
            inbound,
        )
    }

    pub fn start(
        mut self,
        marshal: marshal::Mailbox<MinSig, Block>,
        seeder: seeder::Mailbox,
        aggregator: aggregator::Mailbox,
    ) -> Handle<()> {
        self.context.spawn_ref()(self.run(marshal, seeder, aggregator))
    }

    /// Run the application actor.
    async fn run(
        mut self,
        mut marshal: marshal::Mailbox<MinSig, Block>,
        seeder: seeder::Mailbox,
        mut aggregator: aggregator::Mailbox,
    ) {
        // Initialize metrics
        let txs_considered: Counter<u64, AtomicU64> = Counter::default();
        let txs_executed: Counter<u64, AtomicU64> = Counter::default();
        let ancestry_latency = Histogram::new(LATENCY.into_iter());
        let propose_latency = Histogram::new(LATENCY.into_iter());
        let verify_latency = Histogram::new(LATENCY.into_iter());
        let seeded_latency = Histogram::new(LATENCY.into_iter());
        let execute_latency = Histogram::new(LATENCY.into_iter());
        let finalize_latency = Histogram::new(LATENCY.into_iter());
        let prune_latency = Histogram::new(LATENCY.into_iter());
        self.context.register(
            "txs_considered",
            "Number of transactions considered during propose",
            txs_considered.clone(),
        );
        self.context.register(
            "txs_executed",
            "Number of transactions executed after finalization",
            txs_executed.clone(),
        );
        self.context.register(
            "ancestry_latency",
            "Latency of ancestry requests",
            ancestry_latency.clone(),
        );
        self.context.register(
            "propose_latency",
            "Latency of propose requests",
            propose_latency.clone(),
        );
        self.context.register(
            "verify_latency",
            "Latency of verify requests",
            verify_latency.clone(),
        );
        self.context.register(
            "seeded_latency",
            "Latency of seeded requests",
            seeded_latency.clone(),
        );
        self.context.register(
            "execute_latency",
            "Latency of execute requests",
            execute_latency.clone(),
        );
        self.context.register(
            "finalize_latency",
            "Latency of finalize requests",
            finalize_latency.clone(),
        );
        self.context.register(
            "prune_latency",
            "Latency of prune requests",
            prune_latency.clone(),
        );
        let ancestry_latency = histogram::Timed::new(
            ancestry_latency,
            Arc::new(self.context.with_label("ancestry_latency")),
        );
        let propose_latency = histogram::Timed::new(
            propose_latency,
            Arc::new(self.context.with_label("propose_latency")),
        );
        let verify_latency = histogram::Timed::new(
            verify_latency,
            Arc::new(self.context.with_label("verify_latency")),
        );
        let seeded_latency = histogram::Timed::new(
            seeded_latency,
            Arc::new(self.context.with_label("seeded_latency")),
        );
        let execute_latency = histogram::Timed::new(
            execute_latency,
            Arc::new(self.context.with_label("execute_latency")),
        );
        let finalize_latency = histogram::Timed::new(
            finalize_latency,
            Arc::new(self.context.with_label("finalize_latency")),
        );
        let prune_latency = histogram::Timed::new(
            prune_latency,
            Arc::new(self.context.with_label("prune_latency")),
        );

        // Initialize the state
        let mut state = Adb::init(
            self.context.with_label("state"),
            adb::any::variable::Config {
                mmr_journal_partition: format!("{}-state-mmr-journal", self.partition_prefix),
                mmr_metadata_partition: format!("{}-state-mmr-metadata", self.partition_prefix),
                mmr_items_per_blob: self.mmr_items_per_blob,
                mmr_write_buffer: self.mmr_write_buffer,
                log_journal_partition: format!("{}-state-log-journal", self.partition_prefix),
                log_items_per_section: self.log_items_per_section,
                log_write_buffer: self.log_write_buffer,
                log_compression: None,
                log_codec_config: (),
                locations_journal_partition: format!(
                    "{}-state-locations-journal",
                    self.partition_prefix
                ),
                locations_items_per_blob: self.locations_items_per_blob,
                translator: EightCap,
                thread_pool: None,
                buffer_pool: self.buffer_pool.clone(),
            },
        )
        .await
        .unwrap();
        let mut events = keyless::Keyless::<_, Output, Sha256>::init(
            self.context.with_label("events"),
            keyless::Config {
                mmr_journal_partition: format!("{}-events-mmr-journal", self.partition_prefix),
                mmr_metadata_partition: format!("{}-events-mmr-metadata", self.partition_prefix),
                mmr_items_per_blob: self.mmr_items_per_blob,
                mmr_write_buffer: self.mmr_write_buffer,
                log_journal_partition: format!("{}-events-log-journal", self.partition_prefix),
                log_items_per_section: self.log_items_per_section,
                log_write_buffer: self.log_write_buffer,
                log_compression: None,
                log_codec_config: (),
                locations_journal_partition: format!(
                    "{}-events-locations-journal",
                    self.partition_prefix
                ),
                locations_items_per_blob: self.locations_items_per_blob,
                locations_write_buffer: self.log_write_buffer,
                thread_pool: None,
                buffer_pool: self.buffer_pool.clone(),
            },
        )
        .await
        .unwrap();

        // Create the execution pool
        //
        // TODO (https://github.com/commonwarexyz/monorepo/issues/1540): use commonware-runtime::create_pool
        let execution_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.execution_concurrency)
            .build()
            .expect("failed to create execution pool");
        let execution_pool = ThreadPool::new(execution_pool);

        // Compute genesis digest
        let genesis_digest = genesis_digest();

        // Track built blocks
        let built: Option<(View, Block)> = None;
        let built = Arc::new(Mutex::new(built));

        // Initialize mempool
        let mut mempool = Mempool::new(self.context.with_label("mempool"));

        // Use reconnecting indexer wrapper
        let reconnecting_indexer = crate::indexer::ReconnectingIndexer::new(
            self.context.with_label("indexer"),
            self.indexer,
        );

        // This will never fail and handles reconnection internally
        let mut next_prune = self.context.gen_range(1..=PRUNE_INTERVAL);
        let mut tx_stream = Box::pin(reconnecting_indexer.listen_mempool().await.unwrap());
        loop {
            select! {
                    message =  self.mailbox.next() => {
                        let Some(message) = message else {
                            return;
                        };
                        match message {
                            Message::Genesis { response } => {
                                // Use the digest of the genesis message as the initial
                                // payload.
                                let _ = response.send(genesis_digest);
                            }
                            Message::Propose {
                                view,
                                parent,
                                mut response,
                            } => {
                                // Start the timer
                                let ancestry_timer = ancestry_latency.timer();
                                let propose_timer = propose_latency.timer();

                                // Immediately send a response for genesis block
                                if parent.1 == genesis_digest {
                                    drop(ancestry_timer);
                                    self.inbound.ancestry(view, vec![genesis_block()], propose_timer, response).await;
                                    continue;
                                }

                                // Get the ancestry
                                let ancestry = ancestry(marshal.clone(), (Some(parent.0), parent.1), state.get_metadata().await.unwrap().and_then(|(_, v)| match v {
                                    Some(Value::Commit { height, start: _ }) => Some(height),
                                    _ => None,
                                }).unwrap_or(0));

                                // Wait for the parent block to be available or the request to be cancelled in a separate task (to
                                // continue processing other messages)
                                self.context.with_label("ancestry").spawn({
                                    let mut inbound = self.inbound.clone();
                                    move |_| async move {
                                        select! {
                                            ancestry = ancestry => {
                                                // Get the ancestry
                                                let Some(ancestry) = ancestry else {
                                                    ancestry_timer.cancel();
                                                    warn!(view, "missing parent ancestry");
                                                    return;
                                                };
                                                drop(ancestry_timer);

                                                // Pass back to mailbox
                                                inbound.ancestry(view, ancestry, propose_timer, response).await;
                                            },
                                            _ = response.closed() => {
                                                // The response was cancelled
                                                ancestry_timer.cancel();
                                                warn!(view, "propose aborted");
                                            }
                                        }
                                    }
                                });
                            }
                            Message::Ancestry {
                                view,
                                blocks,
                                timer,
                                response,
                            } => {
                                // Get parent block
                                let parent = blocks.last().unwrap();

                                // Find first block on top of finalized state (may have increased since we started)
                                let height = state.get_metadata().await.unwrap().and_then(|(_, v)| match v {
                                    Some(Value::Commit { height, start: _ }) => Some(height),
                                    _ => None,
                                }).unwrap_or(0);
                                let mut noncer = Noncer::new(&state);
                                for block in &blocks {
                                    // Skip blocks below our height
                                    if block.height <= height {
                                        debug!(block = block.height, processed = height, "skipping block during propose");
                                        continue;
                                    }

                                    // Apply transaction nonces to state
                                    for tx in &block.transactions {
                                        // We don't care if the nonces are valid or not, we just need to ensure we'll process tip the same way as state will be processed during finalization
                                        noncer.prepare(tx).await;
                                    }
                                }

                                // Select up to max transactions
                                let mut considered = 0;
                                let mut transactions = Vec::new();
                                while transactions.len() < MAX_BLOCK_TRANSACTIONS {
                                    // Get next transaction
                                    let Some(tx) = mempool.next() else {
                                        break;
                                    };
                                    considered += 1;

                                    // Attempt to apply
                                    if !noncer.prepare(&tx).await {
                                        continue;
                                    }

                                    // Add to transactions
                                    transactions.push(tx);
                                }
                                let txs = transactions.len();

                                // Update metrics
                                txs_considered.inc_by(considered as u64);

                                // When ancestry for propose is provided, we can attempt to pack a block
                                let block = Block::new(parent.digest(), view, parent.height+1, transactions);
                                let digest = block.digest();
                                {
                                    // We may drop the transactions from a block that was never broadcast...users
                                    // can rebroadcast.
                                    let mut built = built.lock().unwrap();
                                    *built = Some((view, block));
                                }

                                // Send the digest to the consensus
                                let result = response.send(digest);
                                info!(view, ?digest, txs, success=result.is_ok(), "proposed block");
                                drop(timer);
                            }
                            Message::Broadcast { payload } => {
                                // Check if the last built is equal
                                let Some(built) = built.lock().unwrap().take() else {
                                    warn!(?payload, "missing block to broadcast");
                                    continue;
                                };

                                // Check if the block is equal
                                if built.1.commitment() != payload {
                                    warn!(?payload, "outdated broadcast");
                                    continue;
                                }

                                // Send the block to the syncer
                                debug!(
                                    ?payload,
                                    view = built.0,
                                    height = built.1.height,
                                    "broadcast requested"
                                );
                                marshal.broadcast(built.1).await;
                            }
                            Message::Verify {
                                view,
                                parent,
                                payload,
                                mut response,
                            } => {
                                // Start the timer
                                let timer = verify_latency.timer();

                                // Get the parent and current block
                                let parent_request = if parent.1 == genesis_digest {
                                    Either::Left(future::ready(Ok(genesis_block())))
                                } else {
                                    Either::Right(marshal.subscribe(Some(parent.0), parent.1).await)
                                };

                                // Wait for the blocks to be available or the request to be cancelled in a separate task (to
                                // continue processing other messages)
                                self.context.with_label("verify").spawn({
                                    let mut marshal = marshal.clone();
                                    move |mut context| async move {
                                        let requester =
                                            try_join(parent_request, marshal.subscribe(None, payload).await);
                                        select! {
                                            result = requester => {
                                                // Unwrap the results
                                                let (parent, block) = result.unwrap();

                                                // Verify the block
                                                if block.view != view {
                                                    let _ = response.send(false);
                                                    return;
                                                }
                                                if block.height != parent.height + 1 {
                                                    let _ = response.send(false);
                                                    return;
                                                }
                                                if block.parent != parent.digest() {
                                                    let _ = response.send(false);
                                                    return;
                                                }

                                                // Batch verify transaction signatures (we don't care if the nonces are valid or not, we'll just skip the ones that are invalid)
                                                let mut batcher = Batch::new();
                                                for tx in &block.transactions {
                                                    tx.verify_batch(&mut batcher);
                                                }
                                                if !batcher.verify(&mut context) {
                                                    let _ = response.send(false);
                                                    return;
                                                }

                                                // Persist the verified block (transactions may be invalid)
                                                marshal.verified(view, block).await;

                                                // Send the verification result to the consensus
                                                let _ = response.send(true);

                                                // Stop the timer
                                                drop(timer);
                                            },
                                            _ = response.closed() => {
                                                // The response was cancelled
                                                warn!(view, "verify aborted");
                                            }
                                        }
                                    }
                                });
                            }
                            Message::Finalized { block, response } => {
                                // Start the timer
                                let seeded_timer = seeded_latency.timer();
                                let finalize_timer = finalize_latency.timer();

                                // While waiting for the seed required for processing, we should spawn a task
                                // to handle resolution to avoid blocking the application.
                                self.context.with_label("seeded").spawn({
                                    let mut inbound = self.inbound.clone();
                                    let mut seeder = seeder.clone();
                                    move |_| async move {
                                        let seed = seeder.get(block.view).await;
                                        drop(seeded_timer);
                                        inbound.seeded(block, seed, finalize_timer, response).await;
                                    }
                                });

                            }
                            Message::Seeded { block, seed, timer, response } => {
                                // Execute state transition (will only apply if next block)
                                let height = block.height;
                                let commitment = block.commitment();

                                // Apply the block to our state
                                //
                                // We must wait for the seed to be available before processing the block,
                                // otherwise we will not be able to match players or compute attack strength.
                                let execute_timer = execute_latency.timer();
                                let tx_count = block.transactions.len();
                                let result = state_transition::execute_state_transition(
                                    &mut state,
                                    &mut events,
                                    self.identity,
                                    height,
                                    seed,
                                    block.transactions,
                                    execution_pool.clone(),
                                ).await;
                                drop(execute_timer);

                                // Update metrics
                                txs_executed.inc_by(tx_count as u64);

                                // Update mempool based on processed transactions
                                for (public, next_nonce) in &result.processed_nonces {
                                    mempool.retain(public, *next_nonce);
                                }

                                // Generate range proof for changes
                                let state_proof_ops = result.state_end_op - result.state_start_op;
                                let events_start_op = result.events_start_op;
                                let events_proof_ops = result.events_end_op - events_start_op;
                                let ((state_proof, state_proof_ops), (events_proof, events_proof_ops)) = try_join(
                                    state.historical_proof(result.state_end_op, result.state_start_op, state_proof_ops),
                                    events.historical_proof(result.events_end_op, events_start_op, NZU64!(events_proof_ops)),
                                ).await.expect("failed to generate proofs");

                                // Send to aggregator
                                aggregator.executed(block.view, block.height, commitment, result, state_proof, state_proof_ops, events_proof, events_proof_ops, response).await;

                                // Stop the timer
                                drop(timer);

                                // Attempt to prune (this syncs data prior to prune, so we don't need to call separately)
                                next_prune -= 1;
                                if next_prune == 0 {
                                    // Prune storage
                                    let timer = prune_latency.timer();
                                    try_join(
                                        state.prune(state.inactivity_floor_loc()),
                                        events.prune(events_start_op),
                                    ).await.expect("failed to prune storage");
                                    drop(timer);

                                    // Reset next prune
                                    next_prune = self.context.gen_range(1..=PRUNE_INTERVAL);
                                }
                            },
                        }
                },
                pending = tx_stream.next() => {
                    // The reconnecting wrapper handles all connection issues internally
                    // We only get Some(Ok(tx)) for valid transactions
                    let Some(Ok(pending)) = pending else {
                        // This should only happen if there's a transaction-level error
                        // The stream itself won't end due to the reconnecting wrapper
                        continue;
                    };

                    // Process transactions (already verified in indexer client)
                    for tx in pending.transactions {
                        // Check if below next
                        let next = nonce(&state, &tx.public).await;
                        if tx.nonce < next {
                            // If below next, we drop the incoming transaction
                            debug!(tx = tx.nonce, state = next, "dropping incoming transaction");
                            continue;
                        }

                        // Add to mempool
                        mempool.add(tx);
                    }
                }
            }
        }
    }
}
