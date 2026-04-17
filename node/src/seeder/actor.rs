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

/// Tracks upload confirmations separately from upload scheduling.
///
/// `next_unconfirmed_upload` is the first seed view we do not yet know is uploaded.
/// Any completed view above that pointer is remembered in
/// `completed_out_of_order_uploads` until the gap closes.
struct UploadTracker {
    next_unconfirmed_upload: View,
    completed_out_of_order_uploads: RMap,
}

impl UploadTracker {
    /// Start tracking from the first seed that still needs confirmation.
    fn new(next_unconfirmed_upload: View) -> Self {
        assert!(next_unconfirmed_upload > 0, "seed uploads start at view 1");
        Self {
            next_unconfirmed_upload,
            completed_out_of_order_uploads: RMap::new(),
        }
    }

    /// Record a completed upload.
    ///
    /// Returns the new contiguous uploaded frontier when this completion closes the
    /// current gap; otherwise returns `None`.
    fn record_uploaded(&mut self, view: View) -> Option<View> {
        // Ignore duplicate or already-accounted-for completions.
        if view < self.next_unconfirmed_upload {
            return None;
        }

        // This upload finished early; remember it until the missing lower view arrives.
        if view > self.next_unconfirmed_upload {
            self.completed_out_of_order_uploads.insert(view);
            return None;
        }

        // We received the exact view we were waiting on. Advance the frontier and
        // consume any higher views that had already completed out of order.
        let mut uploaded_through = view;
        self.next_unconfirmed_upload = self
            .next_unconfirmed_upload
            .checked_add(1)
            .expect("seed upload view overflow");

        loop {
            let Some(end_region) = self
                .completed_out_of_order_uploads
                .next_gap(self.next_unconfirmed_upload)
                .0
            else {
                break;
            };
            self.completed_out_of_order_uploads
                .remove(self.next_unconfirmed_upload, end_region);
            uploaded_through = end_region;
            self.next_unconfirmed_upload = end_region
                .checked_add(1)
                .expect("seed upload view overflow");
        }

        Some(uploaded_through)
    }

    #[cfg(test)]
    fn next_unconfirmed_upload(&self) -> View {
        self.next_unconfirmed_upload
    }

    #[cfg(test)]
    fn is_waiting_on_out_of_order_completion(&self, view: View) -> bool {
        self.completed_out_of_order_uploads.get(&view).is_some()
    }
}

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
        // Metadata stores the highest confirmed upload, so the next view to start and
        // the first view still awaiting confirmation are both `LAST_UPLOADED_KEY + 1`.
        let mut next_upload_to_start = metadata
            .get(&LAST_UPLOADED_KEY.into())
            .cloned()
            .unwrap_or(0)
            .checked_add(1)
            .expect("seed upload view overflow");
        let mut upload_tracker = UploadTracker::new(next_upload_to_start);
        info!(next_upload_to_start, "initial seed upload cursor");

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

                    // Update metadata if the contiguous uploaded frontier has increased.
                    if let Some(end_region) = upload_tracker.record_uploaded(view) {
                        metadata.put(LAST_UPLOADED_KEY.into(), end_region);
                        metadata.sync().await.expect("failed to sync metadata");
                        info!(boundary = end_region, "updated seed upload marker");
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
                let Some(seed) = storage
                    .get(next_upload_to_start)
                    .await
                    .expect("failed to get seed")
                else {
                    break;
                };

                // Increment uploads outstanding
                uploads_outstanding += 1;

                // Upload seed to indexer
                self.context.with_label("seed_submit").spawn({
                    let seed = Seed::new(next_upload_to_start, seed);
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

                // Increment the next upload we are willing to start.
                next_upload_to_start += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::UploadTracker;

    #[test]
    fn upload_tracker_advances_past_out_of_order_completions_once_gap_closes() {
        let mut tracker = UploadTracker::new(10);

        // 11 and 12 finish before 10. Once 10 arrives, the tracker should advance
        // straight through 12 and clear the temporary out-of-order state.
        assert_eq!(tracker.record_uploaded(11), None);
        assert_eq!(tracker.record_uploaded(12), None);
        assert_eq!(tracker.record_uploaded(10), Some(12));
        assert_eq!(tracker.next_unconfirmed_upload(), 13);
        assert!(!tracker.is_waiting_on_out_of_order_completion(10));
        assert!(!tracker.is_waiting_on_out_of_order_completion(11));
        assert!(!tracker.is_waiting_on_out_of_order_completion(12));
    }
}
