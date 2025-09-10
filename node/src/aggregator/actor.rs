use crate::{
    aggregator::{ingress::Mailbox, Config, Message},
    indexer::Indexer,
};
use battleware_types::{
    api::Summary,
    execution::{Output, Progress, Value},
    genesis_digest,
};
use bytes::{Buf, BufMut};
use commonware_codec::{
    DecodeExt, Encode, EncodeSize, FixedSize, Read, ReadExt, ReadRangeExt, Write,
};
use commonware_consensus::aggregation::types::{Certificate, Index, Item};
use commonware_cryptography::{
    bls12381::primitives::variant::{MinSig, Variant},
    ed25519::PublicKey,
    sha256::Digest,
    Digestible,
};
use commonware_p2p::{Receiver, Sender};
use commonware_resolver::{p2p, Resolver};
use commonware_runtime::{Clock, Handle, Metrics, Spawner, Storage};
use commonware_storage::{
    cache,
    journal::fixed,
    mmr::verification::Proof,
    ordinal::{self, Ordinal},
    rmap::RMap,
    store::operation::{Keyless, Variable},
};
use commonware_utils::sequence::U64;
use futures::{
    channel::{mpsc, oneshot},
    join, StreamExt,
};
use governor::clock::Clock as GClock;
use prometheus_client::metrics::gauge::Gauge;
use rand::RngCore;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tracing::{debug, info, warn};

const BATCH_ENQUEUE: usize = 20;
const RETRY_DELAY: Duration = Duration::from_secs(10);

pub struct Proofs {
    pub state_proof: Proof<Digest>,
    pub state_proof_ops: Vec<Variable<Digest, Value>>,
    pub events_proof: Proof<Digest>,
    pub events_proof_ops: Vec<Keyless<Output>>,
}

impl Write for Proofs {
    fn write(&self, buf: &mut impl BufMut) {
        self.state_proof.write(buf);
        self.state_proof_ops.write(buf);
        self.events_proof.write(buf);
        self.events_proof_ops.write(buf);
    }
}

impl Read for Proofs {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &()) -> Result<Self, commonware_codec::Error> {
        let state_proof = Proof::<Digest>::read_cfg(reader, &500)?;
        let state_proof_ops = Vec::read_range(reader, 0..=500)?;
        let events_proof = Proof::<Digest>::read_cfg(reader, &500)?;
        let events_proof_ops = Vec::read_range(reader, 0..=500)?;
        Ok(Self {
            state_proof,
            state_proof_ops,
            events_proof,
            events_proof_ops,
        })
    }
}

impl EncodeSize for Proofs {
    fn encode_size(&self) -> usize {
        self.state_proof.encode_size()
            + self.state_proof_ops.encode_size()
            + self.events_proof.encode_size()
            + self.events_proof_ops.encode_size()
    }
}

/// A fixed-size certificate that can be used to store in an ordinal.
pub struct FixedCertificate {
    pub index: Index,
    pub digest: Digest,
    pub signature: <MinSig as Variant>::Signature,
}

impl Write for FixedCertificate {
    fn write(&self, buf: &mut impl BufMut) {
        self.index.write(buf);
        self.digest.write(buf);
        self.signature.write(buf);
    }
}

impl Read for FixedCertificate {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &()) -> Result<Self, commonware_codec::Error> {
        let index = Index::read(reader)?;
        let digest = Digest::read(reader)?;
        let signature = <MinSig as Variant>::Signature::read(reader)?;
        Ok(Self {
            index,
            digest,
            signature,
        })
    }
}

impl FixedSize for FixedCertificate {
    const SIZE: usize = Index::SIZE + Digest::SIZE + <MinSig as Variant>::Signature::SIZE;
}

impl From<Certificate<MinSig, Digest>> for FixedCertificate {
    fn from(certificate: Certificate<MinSig, Digest>) -> Self {
        Self {
            index: certificate.item.index,
            digest: certificate.item.digest,
            signature: certificate.signature,
        }
    }
}

impl From<FixedCertificate> for Certificate<MinSig, Digest> {
    fn from(fixed_certificate: FixedCertificate) -> Self {
        Self {
            item: Item {
                index: fixed_certificate.index,
                digest: fixed_certificate.digest,
            },
            signature: fixed_certificate.signature,
        }
    }
}

pub struct Actor<R: Storage + Metrics + Clock + Spawner + GClock + RngCore, I: Indexer> {
    context: R,
    config: Config<I>,
    inbound: Mailbox,
    mailbox: mpsc::Receiver<Message>,

    waiting: BTreeSet<u64>,
    certificates_processed: Gauge,
}

impl<R: Storage + Metrics + Clock + Spawner + GClock + RngCore, I: Indexer> Actor<R, I> {
    pub fn new(context: R, config: Config<I>) -> (Self, Mailbox) {
        // Create mailbox
        let (sender, mailbox) = mpsc::channel(config.mailbox_size);
        let inbound = Mailbox::new(sender);

        // Create metrics
        let certificates_processed = Gauge::default();
        context.register(
            "certificates_processed",
            "Number of contiguous certificates processed",
            certificates_processed.clone(),
        );

        (
            Self {
                context,
                config,
                inbound: inbound.clone(),
                mailbox,
                waiting: BTreeSet::new(),
                certificates_processed,
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
        // Create storage
        let mut cache = cache::Cache::<_, Proofs>::init(
            self.context.with_label("cache"),
            cache::Config {
                partition: format!("{}-cache", self.config.partition),
                compression: None,
                codec_config: (),
                items_per_blob: self.config.prunable_items_per_blob,
                write_buffer: self.config.write_buffer,
                replay_buffer: self.config.replay_buffer,
                buffer_pool: self.config.buffer_pool.clone(),
            },
        )
        .await
        .expect("failed to initialize cache");
        let mut results = fixed::Journal::init(
            self.context.with_label("results"),
            fixed::Config {
                partition: format!("{}-results", self.config.partition),
                items_per_blob: self.config.persistent_items_per_blob,
                write_buffer: self.config.write_buffer,
                buffer_pool: self.config.buffer_pool,
            },
        )
        .await
        .expect("failed to initialize results storage");
        let mut certificates = Ordinal::<_, FixedCertificate>::init(
            self.context.with_label("certificates"),
            ordinal::Config {
                partition: format!("{}-certificates", self.config.partition),
                items_per_blob: self.config.persistent_items_per_blob,
                write_buffer: self.config.write_buffer,
                replay_buffer: self.config.replay_buffer,
            },
        )
        .await
        .expect("failed to initialize certificate storage");

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
                fetch_retry_timeout: Duration::from_secs(10),
                priority_requests: false,
                priority_responses: false,
            },
        );
        resolver_engine.start(backfill);

        // Start by fetching the first missing certificates
        let missing = certificates.missing_items(1, BATCH_ENQUEUE);
        for next in missing {
            self.waiting.insert(next);
            resolver.fetch(next.into()).await;
        }

        // Compute genesis digest
        let genesis_digest = genesis_digest();

        // Track uploads
        let mut uploads_outstanding = 0;
        let mut cursor = cache.first().unwrap_or(1); // start at height 1
        let mut boundary = cursor;
        let mut tracked_uploads = RMap::new();
        info!(cursor, "initial summary cursor");

        // Track pending aggregation work
        let mut proposal_requests: BTreeMap<u64, oneshot::Sender<Digest>> = BTreeMap::new();
        let mut verify_requests: BTreeMap<u64, (Digest, oneshot::Sender<bool>)> = BTreeMap::new();
        loop {
            let Some(message) = self.mailbox.next().await else {
                warn!("mailbox closed");
                break;
            };
            match message {
                Message::Uploaded { index } => {
                    // Decrement uploads outstanding
                    uploads_outstanding -= 1;

                    // Track uploaded index
                    tracked_uploads.insert(index);

                    // Prune proofs up to the uploaded height (contiguous with the boundary)
                    let Some(end_region) = tracked_uploads.next_gap(boundary).0 else {
                        continue;
                    };
                    if end_region > boundary {
                        cache
                            .prune(end_region)
                            .await
                            .expect("failed to prune cache");
                        boundary = end_region;
                        info!(boundary, "updated summary upload marker");
                    }
                }
                Message::Executed {
                    view,
                    height,
                    commitment,
                    result,
                    state_proof,
                    state_proof_ops,
                    events_proof,
                    events_proof_ops,
                    response,
                } => {
                    // Persist proofs
                    let cache_task = async {
                        let proofs = Proofs {
                            state_proof,
                            state_proof_ops,
                            events_proof,
                            events_proof_ops,
                        };
                        cache.put(height, proofs).await.unwrap(); // ok to call put multiple times
                        cache.sync().await.unwrap();
                    };

                    // Persist progress
                    let result = Progress::new(
                        view,
                        height,
                        commitment,
                        result.state_root,
                        result.state_start_op,
                        result.state_end_op,
                        result.events_root,
                        result.events_start_op,
                        result.events_end_op,
                    );
                    let result_digest = result.digest();
                    let progress_task = async {
                        // Size is the next item to store and the height-th value will be stored at height - 1,
                        // so comparing size() to height is equivalent to checking if the next item stored will be
                        // at height + 1 (i.e. this height has already been processed).
                        if results.size().await.unwrap() == height {
                            warn!(height, "already processed results");
                            return;
                        }
                        results.append(result).await.unwrap();
                        results.sync().await.unwrap();
                    };
                    join!(cache_task, progress_task);
                    info!(
                        height,
                        view,
                        ?result.state_root,
                        result.state_start_op,
                        result.state_end_op,
                        ?result.events_root,
                        result.events_start_op,
                        result.events_end_op,
                        ?result_digest,
                        "processed block"
                    );

                    // Check if we should clear aggregation requests
                    if let Some(request) = proposal_requests.remove(&height) {
                        debug!(height, view, "backfilled aggregation proposal");
                        let _ = request.send(result_digest);
                    }
                    proposal_requests.retain(|index, _| *index > height);
                    if let Some((payload, request)) = verify_requests.remove(&height) {
                        debug!(height, view, "backfilled aggregation verify");
                        let _ = request.send(result_digest == payload);
                    }
                    verify_requests.retain(|index, _| *index > height);

                    // Continue processing blocks
                    let _ = response.send(());
                }
                Message::Genesis { response } => {
                    let _ = response.send(genesis_digest);
                }
                Message::Propose { index, response } => {
                    // Fetch item from progress
                    if index == 0 {
                        let _ = response.send(genesis_digest);
                        continue;
                    }
                    let item = index - 1;
                    if let Ok(result) = results.read(item).await {
                        let _ = response.send(result.digest());
                        continue;
                    };

                    // This height may not yet be stored, so we'll wait for it to occur
                    proposal_requests.insert(index, response);
                }
                Message::Verify {
                    index,
                    payload,
                    response,
                } => {
                    if index == 0 {
                        let _ = response.send(genesis_digest == payload);
                        continue;
                    }
                    let item = index - 1;
                    if let Ok(result) = results.read(item).await {
                        let _ = response.send(result.digest() == payload);
                        continue;
                    };

                    // This height may not yet be stored, so we'll wait for it to occur
                    verify_requests.insert(index, (payload, response));
                }
                Message::Certified { certificate } => {
                    self.waiting.remove(&certificate.item.index);

                    let height = certificate.item.index;
                    if height == 0 {
                        continue;
                    }

                    // Skip if already processed
                    if certificates.has(height) {
                        debug!(height, "already processed");
                        continue;
                    }
                    info!(
                        height = certificate.item.index,
                        digest = ?certificate.item.digest,
                        "certified block"
                    );

                    // Store in certificates
                    certificates
                        .put(height, certificate.clone().into())
                        .await
                        .unwrap();
                    certificates.sync().await.unwrap();

                    // Cancel resolver
                    if let Some(current_end) = certificates.next_gap(1).0 {
                        self.certificates_processed.set(current_end as i64);
                        let current_end = U64::from(current_end);
                        resolver.retain(move |x| x > &current_end).await;
                    }

                    // Enqueue missing seeds
                    let missing = certificates.missing_items(1, BATCH_ENQUEUE);
                    for next in missing {
                        if !self.waiting.insert(next) {
                            continue;
                        }
                        resolver.fetch(next.into()).await;
                    }
                }
                Message::Tip { index: tip } => {
                    debug!(tip, "new aggregation tip");
                }
                Message::Deliver {
                    index,
                    certificate,
                    response,
                } => {
                    // Decode certificate
                    let Ok(certificate) =
                        Certificate::<MinSig, Digest>::decode(&mut certificate.as_ref())
                    else {
                        response.send(false).expect("failed to send false");
                        continue;
                    };
                    if certificate.item.index != index {
                        response.send(false).expect("failed to send false");
                        continue;
                    }

                    // Verify certificate
                    if !certificate.verify(&self.config.namespace, &self.config.identity) {
                        response.send(false).expect("failed to send false");
                        continue;
                    }

                    // Store in certificates
                    self.waiting.remove(&index);
                    certificates
                        .put(index, certificate.clone().into())
                        .await
                        .unwrap();
                    certificates.sync().await.unwrap();

                    // Enqueue missing seeds
                    let missing = certificates.missing_items(1, BATCH_ENQUEUE);
                    for next in missing {
                        if !self.waiting.insert(next) {
                            continue;
                        }
                        resolver.fetch(next.into()).await;
                    }
                }
                Message::Produce { index, response } => {
                    // Fetch item from certificates
                    let Ok(Some(fixed_certificate)) = certificates.get(index).await else {
                        continue;
                    };
                    let certificate: Certificate<MinSig, Digest> = fixed_certificate.into();
                    response
                        .send(certificate.encode().into())
                        .expect("failed to send certificate");
                }
            }

            // Attempt to upload any certificates
            //
            // We only delete entires in the cache when they cross the section boundary,
            // so we may re-upload the same height again on restart.
            while uploads_outstanding < self.config.max_uploads_outstanding {
                // Get next certificate
                if !cache.has(cursor) || !certificates.has(cursor) {
                    break;
                }

                // Increment uploads outstanding
                uploads_outstanding += 1;

                // Get certificate
                let certificate = certificates
                    .get(cursor)
                    .await
                    .unwrap()
                    .expect("failed to fetch certificate");

                // Get result
                let result = results
                    .read(cursor - 1)
                    .await
                    .expect("failed to fetch result"); // offset by 1 because stored by 0th offset

                // Get proofs
                let proofs = cache
                    .get(cursor)
                    .await
                    .unwrap()
                    .expect("failed to fetch proofs");

                // Upload the summary to the indexer
                let summary = Summary {
                    progress: result,
                    certificate: certificate.into(),
                    state_proof: proofs.state_proof,
                    state_proof_ops: proofs.state_proof_ops,
                    events_proof: proofs.events_proof,
                    events_proof_ops: proofs.events_proof_ops,
                };
                self.context.with_label("summary_submit").spawn({
                    let indexer = self.config.indexer.clone();
                    let mut channel = self.inbound.clone();
                    move |context| async move {
                        let mut attempts = 1;
                        loop {
                            let Err(e) = indexer.submit_summary(summary.clone()).await else {
                                break;
                            };
                            warn!(?e, attempts, "failed to upload summary");
                            context.sleep(RETRY_DELAY).await;
                            attempts += 1;
                        }
                        debug!(cursor, attempts, "summary uploaded to indexer");
                        channel.uploaded(cursor).await;
                    }
                });

                // Increment cursor
                cursor += 1;
            }
        }
    }
}
