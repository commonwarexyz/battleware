use std::{
    collections::{BTreeSet, HashMap},
    time::Duration,
};

use crate::{
    indexer::Indexer,
    seeder::{ingress::Mailbox, Config, Message},
};
use battleware_types::Seed;
use commonware_codec::{DecodeExt, Encode};
use commonware_consensus::{threshold_simplex::types::View, Viewable};
use commonware_cryptography::{
    bls12381::primitives::variant::{MinSig, Variant},
    ed25519::PublicKey,
};
use commonware_p2p::{Receiver, Sender};
use commonware_resolver::{p2p, Resolver};
use commonware_runtime::{Clock, Handle, Metrics, Spawner, Storage};
use commonware_storage::{
    metadata::{self, Metadata},
    ordinal::{self, Ordinal},
    rmap::RMap,
};
use commonware_utils::sequence::U64;
use futures::{
    channel::{mpsc, oneshot},
    StreamExt,
};
use governor::clock::Clock as GClock;
use rand::RngCore;
use tracing::{debug, info, warn};

const BATCH_ENQUEUE: usize = 20;
const LAST_UPLOADED_KEY: u64 = 0;
const RETRY_DELAY: Duration = Duration::from_secs(10);

pub struct Actor<R: Storage + Metrics + Clock + Spawner + GClock + RngCore, I: Indexer> {
    context: R,
    config: Config<I>,
    inbound: Mailbox,
    mailbox: mpsc::Receiver<Message>,
    waiting: BTreeSet<View>,
}

impl<R: Storage + Metrics + Clock + Spawner + GClock + RngCore, I: Indexer> Actor<R, I> {
    pub fn new(context: R, config: Config<I>) -> (Self, Mailbox) {
        // Create mailbox
        let (sender, mailbox) = mpsc::channel(config.mailbox_size);
        let inbound = Mailbox::new(sender);

        (
            Self {
                context,
                config,
                inbound: inbound.clone(),
                mailbox,
                waiting: BTreeSet::new(),
            },
            inbound,
        )
    }

    pub fn start(
        mut self,
        backfill: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
    ) -> Handle<()> {
        self.context.spawn_ref()(self.run(backfill))
    }

    async fn run(
        mut self,
        backfill: (
            impl Sender<PublicKey = PublicKey>,
            impl Receiver<PublicKey = PublicKey>,
        ),
    ) {
        // Create metadata
        let mut metadata = Metadata::<_, U64, u64>::init(
            self.context.with_label("metadata"),
            metadata::Config {
                partition: format!("{}-metadata", self.config.partition_prefix),
                codec_config: (),
            },
        )
        .await
        .expect("failed to initialize metadata");

        // Create storage
        let mut storage = Ordinal::init(
            self.context.with_label("seeder"),
            ordinal::Config {
                partition: format!("{}-storage", self.config.partition_prefix),
                items_per_blob: self.config.items_per_blob,
                write_buffer: self.config.write_buffer,
                replay_buffer: self.config.replay_buffer,
            },
        )
        .await
        .expect("failed to initialize seeder storage");

        // Create resolver
        let (resolver_engine, mut resolver) = p2p::Engine::new(
            self.context.with_label("resolver"),
            p2p::Config {
                coordinator: self.config.supervisor,
                consumer: self.inbound.clone(),
                producer: self.inbound.clone(),
                mailbox_size: self.config.mailbox_size,
                requester_config: commonware_p2p::utils::requester::Config {
                    public_key: self.config.public_key,
                    rate_limit: self.config.backfill_quota,
                    initial: Duration::from_secs(1),
                    timeout: Duration::from_secs(2),
                },
                fetch_retry_timeout: Duration::from_millis(100),
                priority_requests: false,
                priority_responses: false,
            },
        );
        resolver_engine.start(backfill);

        // Track waiters for each seed
        let mut listeners: HashMap<View, Vec<oneshot::Sender<Seed>>> = HashMap::new();

        // Start by fetching the first missing seeds
        let missing = storage.missing_items(1, BATCH_ENQUEUE);
        for next in missing {
            resolver.fetch(next.into()).await;
            self.waiting.insert(next);
        }

        // Track uploads
        let mut uploads_outstanding = 0;
        let mut cursor = metadata
            .get(&LAST_UPLOADED_KEY.into())
            .cloned()
            .unwrap_or(1);
        let mut boundary = cursor;
        let mut tracked_uploads = RMap::new();
        info!(cursor, "initial seed cursor");

        // Process messages
        loop {
            let Some(message) = self.mailbox.next().await else {
                warn!("mailbox closed");
                break;
            };
            match message {
                Message::Uploaded { view } => {
                    // Decrement uploads outstanding
                    uploads_outstanding -= 1;

                    // Track uploaded view
                    tracked_uploads.insert(view);

                    // Update metadata if lowest uploaded has increased
                    let Some(end_region) = tracked_uploads.next_gap(boundary).0 else {
                        continue;
                    };
                    if end_region > boundary {
                        boundary = end_region;
                        metadata.put(LAST_UPLOADED_KEY.into(), end_region);
                        metadata.sync().await.expect("failed to sync metadata");
                        info!(boundary, "updated seed upload marker");
                    }
                }
                Message::Put(seed) => {
                    self.waiting.remove(&seed.view);

                    // Store seed
                    if !storage.has(seed.view()) {
                        storage
                            .put(seed.view(), seed.signature)
                            .await
                            .expect("failed to put seed");
                        storage.sync().await.expect("failed to sync seed");
                    }

                    // If there were any listeners, send them the seed
                    if let Some(listeners) = listeners.remove(&seed.view) {
                        for listener in listeners {
                            listener.send(seed.clone()).expect("failed to send seed");
                        }
                    }

                    // Cancel resolver
                    if let Some(current_end) = storage.next_gap(1).0 {
                        let current_end = U64::from(current_end);
                        resolver.retain(move |x| x > &current_end).await;
                    }

                    // Enqueue missing seeds
                    let missing = storage.missing_items(1, BATCH_ENQUEUE);
                    if missing.is_empty() {
                        continue;
                    }
                    for next in missing {
                        if !self.waiting.insert(next) {
                            continue;
                        }
                        resolver.fetch(next.into()).await;
                    }
                }
                Message::Get { view, response } => {
                    let Some(signature) = storage.get(view).await.expect("failed to get seed")
                    else {
                        if self.waiting.insert(view) {
                            resolver.fetch(view.into()).await;
                        }
                        listeners.entry(view).or_default().push(response);
                        continue;
                    };
                    response
                        .send(Seed { view, signature })
                        .expect("failed to send seed");
                }
                Message::Deliver {
                    view,
                    signature,
                    response,
                } => {
                    // Verify signature
                    let Ok(signature) =
                        <<MinSig as Variant>::Signature>::decode(&mut signature.as_ref())
                    else {
                        response.send(false).expect("failed to send none");
                        continue;
                    };
                    let seed = Seed::new(view, signature);
                    if !seed.verify(&self.config.namespace, &self.config.identity) {
                        response.send(false).expect("failed to send false");
                        continue;
                    }

                    self.waiting.remove(&view);

                    // Notify resolver
                    response.send(true).expect("failed to send true");

                    // Store seed
                    if !storage.has(view) {
                        storage
                            .put(view, signature)
                            .await
                            .expect("failed to put seed");
                        storage.sync().await.expect("failed to sync seed");
                    }

                    // Notify listeners
                    if let Some(listeners) = listeners.remove(&view) {
                        for listener in listeners {
                            listener.send(seed.clone()).expect("failed to send seed");
                        }
                    }

                    // Cancel resolver
                    if let Some(current_end) = storage.next_gap(1).0 {
                        let current_end = U64::from(current_end);
                        resolver.retain(move |x| x > &current_end).await;
                    }

                    // Enqueue missing seeds
                    let missing = storage.missing_items(1, BATCH_ENQUEUE);
                    for next in missing {
                        if !self.waiting.insert(next) {
                            continue;
                        }
                        resolver.fetch(next.into()).await;
                    }
                }
                Message::Produce { view, response } => {
                    // Serve seed from storage
                    let Some(encoded) = storage.get(view).await.expect("failed to get seed") else {
                        continue;
                    };
                    response
                        .send(encoded.encode().into())
                        .expect("failed to send seed");
                }
            }

            // Attempt to upload any seeds
            while uploads_outstanding < self.config.max_uploads_outstanding {
                // Get next seed
                let Some(seed) = storage.get(cursor).await.expect("failed to get seed") else {
                    break;
                };

                // Increment uploads outstanding
                uploads_outstanding += 1;

                // Upload seed to indexer
                self.context.with_label("seed_submit").spawn({
                    let seed = Seed::new(cursor, seed);
                    let indexer = self.config.indexer.clone();
                    let mut channel = self.inbound.clone();
                    move |context| async move {
                        let view = seed.view();
                        let mut attempts = 1;
                        loop {
                            let Err(e) = indexer.submit_seed(seed.clone()).await else {
                                break;
                            };
                            warn!(?e, attempts, "failed to upload seed");
                            context.sleep(RETRY_DELAY).await;
                            attempts += 1;
                        }
                        debug!(view, attempts, "seed uploaded to indexer");
                        channel.uploaded(view).await;
                    }
                });

                // Increment cursor
                cursor += 1;
            }
        }
    }
}
