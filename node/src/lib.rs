use serde::{Deserialize, Serialize};
use std::{collections::HashMap, net::SocketAddr};

pub mod aggregator;
pub mod application;
pub mod engine;
pub mod indexer;
pub mod seeder;
pub mod supervisor;

/// Configuration for the [engine::Engine].
#[derive(Deserialize, Serialize)]
pub struct Config {
    pub private_key: String,
    pub share: String,
    pub polynomial: String,

    pub port: u16,
    pub metrics_port: u16,
    pub directory: String,
    pub worker_threads: usize,
    pub log_level: String,

    pub allowed_peers: Vec<String>,
    pub bootstrappers: Vec<String>,

    pub message_backlog: usize,
    pub mailbox_size: usize,
    pub deque_size: usize,

    pub indexer: String,
    pub execution_concurrency: usize,
}

/// A list of peers provided when a validator is run locally.
///
/// When run remotely, [commonware_deployer::ec2::Hosts] is used instead.
#[derive(Deserialize, Serialize)]
pub struct Peers {
    pub addresses: HashMap<String, SocketAddr>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use battleware_types::execution::{Instruction, Transaction};
    use commonware_cryptography::{
        bls12381::{
            dkg::ops,
            primitives::{poly::public, variant::MinSig},
        },
        ed25519::{PrivateKey, PublicKey},
        PrivateKeyExt, Signer,
    };
    use commonware_macros::{select, test_traced};
    use commonware_p2p::simulated::{self, Link, Network, Oracle, Receiver, Sender};
    use commonware_runtime::{
        deterministic::{self, Runner},
        Clock, Metrics, Runner as _, Spawner,
    };
    use commonware_utils::{quorum, NZUsize};
    use engine::{Config, Engine};
    use governor::Quota;
    use indexer::Mock;
    use rand::{rngs::StdRng, Rng, SeedableRng};
    use std::{
        collections::{hash_map::Entry, BTreeMap, HashMap, HashSet},
        num::{NonZeroU32, NonZeroUsize},
        time::Duration,
    };
    use tracing::info;

    /// Limit the freezer table size to 1MB because the deterministic runtime stores
    /// everything in RAM.
    const FREEZER_TABLE_INITIAL_SIZE: u32 = 2u32.pow(14); // 1MB

    /// The buffer pool page size.
    const BUFFER_POOL_PAGE_SIZE: NonZeroUsize = NZUsize!(4_096);

    /// The buffer pool capacity.
    const BUFFER_POOL_CAPACITY: NonZeroUsize = NZUsize!(1024 * 1024);

    /// Registers all validators using the oracle.
    async fn register_validators(
        oracle: &mut Oracle<PublicKey>,
        validators: &[PublicKey],
    ) -> HashMap<
        PublicKey,
        (
            (Sender<PublicKey>, Receiver<PublicKey>),
            (Sender<PublicKey>, Receiver<PublicKey>),
            (Sender<PublicKey>, Receiver<PublicKey>),
            (Sender<PublicKey>, Receiver<PublicKey>),
            (Sender<PublicKey>, Receiver<PublicKey>),
            (Sender<PublicKey>, Receiver<PublicKey>),
            (Sender<PublicKey>, Receiver<PublicKey>),
            (Sender<PublicKey>, Receiver<PublicKey>),
        ),
    > {
        let mut registrations = HashMap::new();
        for validator in validators.iter() {
            let (pending_sender, pending_receiver) =
                oracle.register(validator.clone(), 0).await.unwrap();
            let (recovered_sender, recovered_receiver) =
                oracle.register(validator.clone(), 1).await.unwrap();
            let (resolver_sender, resolver_receiver) =
                oracle.register(validator.clone(), 2).await.unwrap();
            let (broadcast_sender, broadcast_receiver) =
                oracle.register(validator.clone(), 3).await.unwrap();
            let (backfill_sender, backfill_receiver) =
                oracle.register(validator.clone(), 4).await.unwrap();
            let (seeder_sender, seeder_receiver) =
                oracle.register(validator.clone(), 5).await.unwrap();
            let (aggregator_sender, aggregator_receiver) =
                oracle.register(validator.clone(), 6).await.unwrap();
            let (aggregation_sender, aggregation_receiver) =
                oracle.register(validator.clone(), 7).await.unwrap();
            registrations.insert(
                validator.clone(),
                (
                    (pending_sender, pending_receiver),
                    (recovered_sender, recovered_receiver),
                    (resolver_sender, resolver_receiver),
                    (broadcast_sender, broadcast_receiver),
                    (backfill_sender, backfill_receiver),
                    (seeder_sender, seeder_receiver),
                    (aggregator_sender, aggregator_receiver),
                    (aggregation_sender, aggregation_receiver),
                ),
            );
        }
        registrations
    }

    /// Links (or unlinks) validators using the oracle.
    ///
    /// The `action` parameter determines the action (e.g. link, unlink) to take.
    /// The `restrict_to` function can be used to restrict the linking to certain connections,
    /// otherwise all validators will be linked to all other validators.
    async fn link_validators(
        oracle: &mut Oracle<PublicKey>,
        validators: &[PublicKey],
        link: Link,
        restrict_to: Option<fn(usize, usize, usize) -> bool>,
    ) {
        for (i1, v1) in validators.iter().enumerate() {
            for (i2, v2) in validators.iter().enumerate() {
                // Ignore self
                if v2 == v1 {
                    continue;
                }

                // Restrict to certain connections
                if let Some(f) = restrict_to {
                    if !f(validators.len(), i1, i2) {
                        continue;
                    }
                }

                // Add link
                oracle
                    .add_link(v1.clone(), v2.clone(), link.clone())
                    .await
                    .unwrap();
            }
        }
    }

    fn all_online(n: u32, seed: u64, link: Link, required: u64) -> String {
        // Create context
        let threshold = quorum(n);
        let cfg = deterministic::Config::default().with_seed(seed);
        let executor = Runner::from(cfg);
        executor.start(|mut context| async move {
            // Create simulated network
            let (network, mut oracle) = Network::new(
                context.with_label("network"),
                simulated::Config {
                    max_size: 1024 * 1024,
                },
            );

            // Start network
            network.start();

            // Register participants
            let mut signers = Vec::new();
            let mut validators = Vec::new();
            for i in 0..n {
                let signer = PrivateKey::from_seed(i as u64);
                let pk = signer.public_key();
                signers.push(signer);
                validators.push(pk);
            }
            validators.sort();
            signers.sort_by_key(|s| s.public_key());
            let mut registrations = register_validators(&mut oracle, &validators).await;

            // Link all validators
            link_validators(&mut oracle, &validators, link, None).await;

            // Derive threshold
            let (polynomial, shares) =
                ops::generate_shares::<_, MinSig>(&mut context, None, n, threshold);
            let identity = *public::<MinSig>(&polynomial);

            // Define mock indexer
            let indexer = Mock::new(identity);

            // Create instances
            let mut public_keys = HashSet::new();
            for (idx, signer) in signers.into_iter().enumerate() {
                // Create signer context
                let public_key = signer.public_key();
                public_keys.insert(public_key.clone());

                // Configure engine
                let uid = format!("validator-{public_key}");
                let config: Config<_, Mock> = engine::Config {
                    blocker: oracle.control(public_key.clone()),
                    partition_prefix: uid.clone(),
                    blocks_freezer_table_initial_size: FREEZER_TABLE_INITIAL_SIZE,
                    finalized_freezer_table_initial_size: FREEZER_TABLE_INITIAL_SIZE,
                    signer,
                    polynomial: polynomial.clone(),
                    share: shares[idx].clone(),
                    participants: validators.clone(),
                    mailbox_size: 1024,
                    deque_size: 10,
                    backfill_quota: Quota::per_second(NonZeroU32::new(10).unwrap()),
                    leader_timeout: Duration::from_secs(1),
                    notarization_timeout: Duration::from_secs(2),
                    nullify_retry: Duration::from_secs(10),
                    fetch_timeout: Duration::from_secs(1),
                    activity_timeout: 10,
                    skip_timeout: 5,
                    max_fetch_count: 10,
                    max_fetch_size: 1024 * 512,
                    fetch_concurrent: 10,
                    fetch_rate_per_peer: Quota::per_second(NonZeroU32::new(10).unwrap()),
                    buffer_pool_page_size: BUFFER_POOL_PAGE_SIZE,
                    buffer_pool_capacity: BUFFER_POOL_CAPACITY,
                    indexer: indexer.clone(),
                    execution_concurrency: 2,
                    max_uploads_outstanding: 4,
                };
                let engine = Engine::new(context.with_label(&uid), config).await;

                // Get networking
                let (
                    pending,
                    recovered,
                    resolver,
                    broadcast,
                    backfill,
                    seeder,
                    aggregator,
                    aggregation,
                ) = registrations.remove(&public_key).unwrap();

                // Start engine
                engine.start(
                    pending,
                    recovered,
                    resolver,
                    broadcast,
                    backfill,
                    seeder,
                    aggregator,
                    aggregation,
                );
            }

            // Poll metrics
            loop {
                let metrics = context.encode();

                // Iterate over all lines
                let mut success = 0;
                for line in metrics.lines() {
                    // Ensure it is a metrics line
                    if !line.starts_with("validator-") {
                        continue;
                    }

                    // Split metric and value
                    let mut parts = line.split_whitespace();
                    let metric = parts.next().unwrap();
                    let value = parts.next().unwrap();

                    // If ends with peers_blocked, ensure it is zero
                    if metric.ends_with("_peers_blocked") {
                        let value = value.parse::<u64>().unwrap();
                        assert_eq!(value, 0);
                    }

                    // If ends with certificates_processed, ensure it is at least required_container
                    if metric.ends_with("_certificates_processed") {
                        let value = value.parse::<u64>().unwrap();
                        if value >= required {
                            success += 1;
                        }
                    }
                }
                if success == n {
                    break;
                }

                // Still waiting for all validators to complete
                context.sleep(Duration::from_secs(1)).await;
            }

            // Wait for mock indexer to contain all seeds and summaries
            loop {
                let contains_seeds = {
                    let mut contains_seeds = true;
                    let seeds = indexer.seeds.lock().unwrap();
                    for i in 1..=required {
                        if !seeds.contains_key(&i) {
                            contains_seeds = false;
                            break;
                        }
                    }
                    contains_seeds
                };
                let contains_summaries = {
                    let summaries = indexer.summaries.write().await;
                    let seen_summaries = summaries.iter().map(|(i, _)| *i).collect::<HashSet<_>>();
                    let mut contains_summaries = true;
                    for i in 1..=required {
                        if !seen_summaries.contains(&i) {
                            contains_summaries = false;
                            break;
                        }
                    }
                    contains_summaries
                };

                // If both contain all required containers, break
                if contains_seeds && contains_summaries {
                    break;
                }

                // Still waiting for all validators to complete
                context.sleep(Duration::from_millis(10)).await;
            }

            context.auditor().state()
        })
    }

    #[test_traced("INFO")]
    fn test_good_links() {
        let link = Link {
            latency: Duration::from_millis(10),
            jitter: Duration::from_millis(1),
            success_rate: 1.0,
        };
        for seed in 0..5 {
            let state = all_online(5, seed, link.clone(), 25);
            assert_eq!(state, all_online(5, seed, link.clone(), 25));
        }
    }

    #[test_traced("INFO")]
    fn test_bad_links() {
        let link = Link {
            latency: Duration::from_millis(200),
            jitter: Duration::from_millis(150),
            success_rate: 0.75,
        };
        for seed in 0..5 {
            let state = all_online(5, seed, link.clone(), 25);
            assert_eq!(state, all_online(5, seed, link.clone(), 25));
        }
    }

    #[test_traced("INFO")]
    fn test_1k() {
        let link = Link {
            latency: Duration::from_millis(80),
            jitter: Duration::from_millis(10),
            success_rate: 0.98,
        };
        all_online(10, 0, link.clone(), 1000);
    }

    #[test_traced("INFO")]
    fn test_backfill() {
        // Create context
        let n = 5;
        let threshold = quorum(n);
        let initial_container_required = 10;
        let final_container_required = 20;
        let executor = Runner::timed(Duration::from_secs(30));
        executor.start(|mut context| async move {
            // Create simulated network
            let (network, mut oracle) = Network::new(
                context.with_label("network"),
                simulated::Config {
                    max_size: 1024 * 1024,
                },
            );

            // Start network
            network.start();

            // Register participants
            let mut signers = Vec::new();
            let mut validators = Vec::new();
            for i in 0..n {
                let signer = PrivateKey::from_seed(i as u64);
                let pk = signer.public_key();
                signers.push(signer);
                validators.push(pk);
            }
            validators.sort();
            signers.sort_by_key(|s| s.public_key());
            let mut registrations = register_validators(&mut oracle, &validators).await;

            // Link all validators (except 0)
            let link = Link {
                latency: Duration::from_millis(10),
                jitter: Duration::from_millis(1),
                success_rate: 1.0,
            };
            link_validators(
                &mut oracle,
                &validators,
                link.clone(),
                Some(|_, i, j| ![i, j].contains(&0usize)),
            )
            .await;

            // Derive threshold
            let (polynomial, shares) =
                ops::generate_shares::<_, MinSig>(&mut context, None, n, threshold);
            let identity = *public::<MinSig>(&polynomial);

            // Define mock indexer
            let indexer = Mock::new(identity);

            // Create instances
            for (idx, signer) in signers.iter().enumerate() {
                // Skip first
                if idx == 0 {
                    continue;
                }

                // Configure engine
                let public_key = signer.public_key();
                let uid = format!("validator-{public_key}");
                let config: Config<_, Mock> = engine::Config {
                    blocker: oracle.control(public_key.clone()),
                    partition_prefix: uid.clone(),
                    blocks_freezer_table_initial_size: FREEZER_TABLE_INITIAL_SIZE,
                    finalized_freezer_table_initial_size: FREEZER_TABLE_INITIAL_SIZE,
                    signer: signer.clone(),
                    polynomial: polynomial.clone(),
                    share: shares[idx].clone(),
                    participants: validators.clone(),
                    mailbox_size: 1024,
                    deque_size: 10,
                    backfill_quota: Quota::per_second(NonZeroU32::new(10).unwrap()),
                    leader_timeout: Duration::from_secs(1),
                    notarization_timeout: Duration::from_secs(2),
                    nullify_retry: Duration::from_secs(10),
                    fetch_timeout: Duration::from_secs(1),
                    activity_timeout: 10,
                    skip_timeout: 5,
                    max_fetch_count: 10,
                    max_fetch_size: 1024 * 512,
                    fetch_concurrent: 10,
                    fetch_rate_per_peer: Quota::per_second(NonZeroU32::new(10).unwrap()),
                    buffer_pool_page_size: BUFFER_POOL_PAGE_SIZE,
                    buffer_pool_capacity: BUFFER_POOL_CAPACITY,
                    indexer: indexer.clone(),
                    execution_concurrency: 2,
                    max_uploads_outstanding: 4,
                };
                let engine = Engine::new(context.with_label(&uid), config).await;

                // Get networking
                let (
                    pending,
                    recovered,
                    resolver,
                    broadcast,
                    backfill,
                    seeder,
                    aggregator,
                    aggregation,
                ) = registrations.remove(&public_key).unwrap();

                // Start engine
                engine.start(
                    pending,
                    recovered,
                    resolver,
                    broadcast,
                    backfill,
                    seeder,
                    aggregator,
                    aggregation,
                );
            }

            // Poll metrics
            loop {
                let metrics = context.encode();

                // Iterate over all lines
                let mut success = 0;
                for line in metrics.lines() {
                    // Ensure it is a metrics line
                    if !line.starts_with("validator-") {
                        continue;
                    }

                    // Split metric and value
                    let mut parts = line.split_whitespace();
                    let metric = parts.next().unwrap();
                    let value = parts.next().unwrap();

                    // If ends with peers_blocked, ensure it is zero
                    if metric.ends_with("_peers_blocked") {
                        let value = value.parse::<u64>().unwrap();
                        assert_eq!(value, 0);
                    }

                    // If ends with certificates_processed, ensure it is at least required_container
                    if metric.ends_with("_certificates_processed") {
                        let value = value.parse::<u64>().unwrap();
                        if value >= initial_container_required {
                            success += 1;
                        }
                    }
                }
                if success == n - 1 {
                    break;
                }

                // Still waiting for all validators to complete
                context.sleep(Duration::from_secs(1)).await;
            }

            // Link first peer (and disable link to second peer)
            link_validators(
                &mut oracle,
                &validators,
                link,
                Some(|_, i, j| [i, j].contains(&0usize) && ![i, j].contains(&1usize)),
            )
            .await;

            // Configure engine
            let signer = signers[0].clone();
            let share = shares[0].clone();
            let public_key = signer.public_key();
            let uid = format!("validator-{public_key}");
            let config: Config<_, Mock> = engine::Config {
                blocker: oracle.control(public_key.clone()),
                partition_prefix: uid.clone(),
                blocks_freezer_table_initial_size: FREEZER_TABLE_INITIAL_SIZE,
                finalized_freezer_table_initial_size: FREEZER_TABLE_INITIAL_SIZE,
                signer: signer.clone(),
                polynomial: polynomial.clone(),
                share,
                participants: validators.clone(),
                mailbox_size: 1024,
                deque_size: 10,
                backfill_quota: Quota::per_second(NonZeroU32::new(10).unwrap()),
                leader_timeout: Duration::from_secs(1),
                notarization_timeout: Duration::from_secs(2),
                nullify_retry: Duration::from_secs(10),
                fetch_timeout: Duration::from_secs(1),
                activity_timeout: 10,
                skip_timeout: 5,
                max_fetch_count: 10,
                max_fetch_size: 1024 * 512,
                fetch_concurrent: 10,
                fetch_rate_per_peer: Quota::per_second(NonZeroU32::new(10).unwrap()),
                buffer_pool_page_size: BUFFER_POOL_PAGE_SIZE,
                buffer_pool_capacity: BUFFER_POOL_CAPACITY,
                indexer: indexer.clone(),
                execution_concurrency: 2,
                max_uploads_outstanding: 4,
            };
            let engine = Engine::new(context.with_label(&uid), config).await;

            // Get networking
            let (
                pending,
                recovered,
                resolver,
                broadcast,
                backfill,
                seeder,
                aggregator,
                aggregation,
            ) = registrations.remove(&public_key).unwrap();

            // Start engine
            engine.start(
                pending,
                recovered,
                resolver,
                broadcast,
                backfill,
                seeder,
                aggregator,
                aggregation,
            );

            // Poll metrics
            loop {
                let metrics = context.encode();

                // Iterate over all lines
                let mut success = 0;
                for line in metrics.lines() {
                    // Ensure it is a metrics line
                    if !line.starts_with("validator-") {
                        continue;
                    }

                    // Split metric and value
                    let mut parts = line.split_whitespace();
                    let metric = parts.next().unwrap();
                    let value = parts.next().unwrap();

                    // If ends with peers_blocked, ensure it is zero
                    if metric.ends_with("_peers_blocked") {
                        let value = value.parse::<u64>().unwrap();
                        assert_eq!(value, 0);
                    }

                    // If ends with certificates_processed, ensure it is at least required_container
                    if metric.ends_with("_certificates_processed") {
                        let value = value.parse::<u64>().unwrap();
                        if value >= final_container_required {
                            success += 1;
                        }
                    }
                }
                if success == n - 1 {
                    break;
                }

                // Still waiting for all validators to complete
                context.sleep(Duration::from_secs(1)).await;
            }
        });
    }

    #[test_traced("INFO")]
    fn test_unclean_shutdown() {
        // Create context
        let n = 5;
        let threshold = quorum(n);
        let required_container = 100;

        // Derive threshold
        let mut rng = StdRng::seed_from_u64(0);
        let (polynomial, shares) = ops::generate_shares::<_, MinSig>(&mut rng, None, n, threshold);
        let identity = *public::<MinSig>(&polynomial);

        // Define mock indexer (must live outside of the loop because
        // it stores seeds beyond the consensus pruning boundary)
        let indexer = Mock::new(identity);

        // Random restarts every x seconds
        let mut runs = 0;
        let mut prev_ctx = None;
        loop {
            // Setup run
            let polynomial = polynomial.clone();
            let shares = shares.clone();
            let indexer = indexer.clone();
            let f = |mut context: deterministic::Context| async move {
                // Create simulated network
                let (network, mut oracle) = Network::new(
                    context.with_label("network"),
                    simulated::Config {
                        max_size: 1024 * 1024,
                    },
                );

                // Start network
                network.start();

                // Register participants
                let mut signers = Vec::new();
                let mut validators = Vec::new();
                for i in 0..n {
                    let signer = PrivateKey::from_seed(i as u64);
                    let pk = signer.public_key();
                    signers.push(signer);
                    validators.push(pk);
                }
                validators.sort();
                signers.sort_by_key(|s| s.public_key());
                let mut registrations = register_validators(&mut oracle, &validators).await;

                // Link all validators
                let link = Link {
                    latency: Duration::from_millis(10),
                    jitter: Duration::from_millis(1),
                    success_rate: 1.0,
                };
                link_validators(&mut oracle, &validators, link, None).await;

                // Create instances
                let mut public_keys = HashSet::new();
                for (idx, signer) in signers.into_iter().enumerate() {
                    // Create signer context
                    let public_key = signer.public_key();
                    public_keys.insert(public_key.clone());

                    // Configure engine
                    let uid = format!("validator-{public_key}");
                    let config: Config<_, Mock> = engine::Config {
                        blocker: oracle.control(public_key.clone()),
                        partition_prefix: uid.clone(),
                        blocks_freezer_table_initial_size: FREEZER_TABLE_INITIAL_SIZE,
                        finalized_freezer_table_initial_size: FREEZER_TABLE_INITIAL_SIZE,
                        signer,
                        polynomial: polynomial.clone(),
                        share: shares[idx].clone(),
                        participants: validators.clone(),
                        mailbox_size: 1024,
                        deque_size: 10,
                        backfill_quota: Quota::per_second(NonZeroU32::new(10).unwrap()),
                        leader_timeout: Duration::from_secs(1),
                        notarization_timeout: Duration::from_secs(2),
                        nullify_retry: Duration::from_secs(10),
                        fetch_timeout: Duration::from_secs(1),
                        activity_timeout: 10,
                        skip_timeout: 5,
                        max_fetch_count: 10,
                        max_fetch_size: 1024 * 512,
                        fetch_concurrent: 10,
                        fetch_rate_per_peer: Quota::per_second(NonZeroU32::new(10).unwrap()),
                        buffer_pool_page_size: BUFFER_POOL_PAGE_SIZE,
                        buffer_pool_capacity: BUFFER_POOL_CAPACITY,
                        indexer: indexer.clone(),
                        execution_concurrency: 2,
                        max_uploads_outstanding: 4,
                    };
                    let engine = Engine::new(context.with_label(&uid), config).await;

                    // Get networking
                    let (
                        pending,
                        recovered,
                        resolver,
                        broadcast,
                        backfill,
                        seeder,
                        aggregator,
                        aggregation,
                    ) = registrations.remove(&public_key).unwrap();

                    // Start engine
                    engine.start(
                        pending,
                        recovered,
                        resolver,
                        broadcast,
                        backfill,
                        seeder,
                        aggregator,
                        aggregation,
                    );
                }

                // Poll metrics
                let poller = context
                    .with_label("metrics")
                    .spawn(move |context| async move {
                        // Wait for all validators to reach required container
                        loop {
                            let metrics = context.encode();

                            // Iterate over all lines
                            let mut success = 0;
                            for line in metrics.lines() {
                                // Ensure it is a metrics line
                                if !line.starts_with("validator-") {
                                    continue;
                                }

                                // Split metric and value
                                let mut parts = line.split_whitespace();
                                let metric = parts.next().unwrap();
                                let value = parts.next().unwrap();

                                // If ends with peers_blocked, ensure it is zero
                                if metric.ends_with("_peers_blocked") {
                                    let value = value.parse::<u64>().unwrap();
                                    assert_eq!(value, 0);
                                }

                                // If ends with certificates_processed, ensure it is at least required_container
                                if metric.ends_with("_certificates_processed") {
                                    let value = value.parse::<u64>().unwrap();
                                    if value >= required_container {
                                        success += 1;
                                    }
                                }
                            }
                            if success == n {
                                break;
                            }

                            // Still waiting for all validators to complete
                            context.sleep(Duration::from_millis(10)).await;
                        }

                        // Wait for mock indexer to contain all seeds and summaries
                        loop {
                            let contains_seeds = {
                                let mut contains_seeds = true;
                                let seeds = indexer.seeds.lock().unwrap();
                                for i in 1..=required_container {
                                    if !seeds.contains_key(&i) {
                                        contains_seeds = false;
                                        break;
                                    }
                                }
                                contains_seeds
                            };
                            let contains_summaries = {
                                let summaries = indexer.summaries.write().await;
                                let seen_summaries =
                                    summaries.iter().map(|(i, _)| *i).collect::<HashSet<_>>();
                                let mut contains_summaries = true;
                                for i in 1..=required_container {
                                    if !seen_summaries.contains(&i) {
                                        contains_summaries = false;
                                        break;
                                    }
                                }
                                contains_summaries
                            };

                            // If both contain all required containers, break
                            if contains_seeds && contains_summaries {
                                break;
                            }

                            // Still waiting for all validators to complete
                            context.sleep(Duration::from_millis(10)).await;
                        }
                    });

                // Exit at random points until finished
                let wait =
                    context.gen_range(Duration::from_millis(100)..Duration::from_millis(1_000));

                // Wait for one to finish
                select! {
                    _ = poller => {
                        // Finished
                        (true, context)
                    },
                    _ = context.sleep(wait) => {
                        // Randomly exit
                        (false, context)
                    }
                }
            };

            // Handle run
            let (complete, context) = if let Some(prev_ctx) = prev_ctx {
                Runner::from(prev_ctx)
            } else {
                Runner::timed(Duration::from_secs(300))
            }
            .start(f);
            if complete {
                break;
            }

            // Prepare for next run
            prev_ctx = Some(context.recover());
            runs += 1;
        }
        assert!(runs > 1);
        info!(runs, "unclean shutdown recovery worked");
    }

    fn test_execution(seed: u64, link: Link) -> String {
        // Create context
        let n = 5;
        let threshold = quorum(n);
        let cfg = deterministic::Config::default()
            .with_seed(seed)
            .with_timeout(Some(Duration::from_secs(1200)));
        let executor = Runner::from(cfg);
        executor.start(|mut context| async move {
            // Create simulated network
            let (network, mut oracle) = Network::new(
                context.with_label("network"),
                simulated::Config {
                    max_size: 1024 * 1024,
                },
            );

            // Start network
            network.start();

            // Register participants
            let mut signers = Vec::new();
            let mut validators = Vec::new();
            for i in 0..n {
                let signer = PrivateKey::from_seed(i as u64);
                let pk = signer.public_key();
                signers.push(signer);
                validators.push(pk);
            }
            validators.sort();
            signers.sort_by_key(|s| s.public_key());
            let mut registrations = register_validators(&mut oracle, &validators).await;

            // Link all validators
            link_validators(&mut oracle, &validators, link, None).await;

            // Derive threshold
            let (polynomial, shares) =
                ops::generate_shares::<_, MinSig>(&mut context, None, n, threshold);
            let identity = *public::<MinSig>(&polynomial);

            // Define mock indexer
            let indexer = Mock::new(identity);

            // Create instances
            let mut public_keys = HashSet::new();
            for (idx, signer) in signers.into_iter().enumerate() {
                // Create signer context
                let public_key = signer.public_key();
                public_keys.insert(public_key.clone());

                // Configure engine
                let uid = format!("validator-{public_key}");
                let config: Config<_, Mock> = engine::Config {
                    blocker: oracle.control(public_key.clone()),
                    partition_prefix: uid.clone(),
                    blocks_freezer_table_initial_size: FREEZER_TABLE_INITIAL_SIZE,
                    finalized_freezer_table_initial_size: FREEZER_TABLE_INITIAL_SIZE,
                    signer,
                    polynomial: polynomial.clone(),
                    share: shares[idx].clone(),
                    participants: validators.clone(),
                    mailbox_size: 1024,
                    deque_size: 10,
                    backfill_quota: Quota::per_second(NonZeroU32::new(10).unwrap()),
                    leader_timeout: Duration::from_secs(1),
                    notarization_timeout: Duration::from_secs(2),
                    nullify_retry: Duration::from_secs(10),
                    fetch_timeout: Duration::from_secs(1),
                    activity_timeout: 10,
                    skip_timeout: 5,
                    max_fetch_count: 10,
                    max_fetch_size: 1024 * 1024,
                    fetch_concurrent: 10,
                    fetch_rate_per_peer: Quota::per_second(NonZeroU32::new(10).unwrap()),
                    buffer_pool_page_size: BUFFER_POOL_PAGE_SIZE,
                    buffer_pool_capacity: BUFFER_POOL_CAPACITY,
                    indexer: indexer.clone(),
                    execution_concurrency: 2,
                    max_uploads_outstanding: 4,
                };
                let engine = Engine::new(context.with_label(&uid), config).await;

                // Get networking
                let (
                    pending,
                    recovered,
                    resolver,
                    broadcast,
                    backfill,
                    seeder,
                    aggregator,
                    aggregation,
                ) = registrations.remove(&public_key).unwrap();

                // Start engine
                engine.start(
                    pending,
                    recovered,
                    resolver,
                    broadcast,
                    backfill,
                    seeder,
                    aggregator,
                    aggregation,
                );
            }

            // Submit 1000 transactions
            let mut remaining = BTreeMap::new();
            for i in 0..1_000 {
                // Generate a signer
                let signer = PrivateKey::from_seed(i as u64);

                // Generate a transaction
                let tx = Transaction::sign(&signer, 0, Instruction::Generate);
                indexer.submit_tx(tx.clone());
                remaining.insert(signer.public_key(), tx);

                // Sleep for a bit to spread them out
                context.sleep(Duration::from_millis(5)).await;
            }

            // Wait for all transactions to be processed
            let mut seen = HashMap::new();
            let mut last_height = None;
            let mut all_height = 1;
            while last_height.is_none() || all_height < last_height.unwrap() {
                // Remove all transactions in some event
                let summaries = indexer
                    .summaries
                    .write()
                    .await
                    .drain(..)
                    .collect::<Vec<_>>();

                // If no events, sleep
                if summaries.is_empty() {
                    // Rebroadcast all remaining transactions
                    for (_, tx) in remaining.iter() {
                        indexer.submit_tx(tx.clone());
                    }

                    // Avoid busy loop
                    context.sleep(Duration::from_secs(1)).await;
                    continue;
                }

                // Process events
                for (height, summary) in summaries.into_iter() {
                    // Remove any pending transactions
                    for event in summary.events_proof_ops.iter() {
                        if let commonware_storage::store::operation::Keyless::Append(
                            battleware_types::execution::Output::Event(
                                battleware_types::execution::Event::Generated { account, .. },
                            ),
                        ) = event
                        {
                            remaining.remove(account);
                        }
                    }

                    // Ensure all validators see the same events at the same height
                    match seen.entry(height) {
                        Entry::Vacant(entry) => {
                            entry.insert((1, summary));
                        }
                        Entry::Occupied(mut entry) => {
                            assert_eq!(entry.get().1, summary);
                            entry.get_mut().0 += 1;
                        }
                    }

                    // Update last height
                    if last_height.is_none() && remaining.is_empty() {
                        last_height = Some(height);
                    }
                }

                // Wait for all validators to see all important heights
                loop {
                    let Some((seen, _)) = seen.get(&all_height) else {
                        break;
                    };
                    if seen < &n {
                        break;
                    }
                    all_height += 1;
                }
            }

            // Return the state
            context.auditor().state()
        })
    }

    #[test_traced]
    fn test_execution_basic() {
        test_execution(
            42,
            Link {
                latency: Duration::from_millis(10),
                jitter: Duration::from_millis(1),
                success_rate: 1.0,
            },
        );
    }

    #[test_traced("INFO")]
    fn test_execution_good_links() {
        let link = Link {
            latency: Duration::from_millis(10),
            jitter: Duration::from_millis(1),
            success_rate: 1.0,
        };
        for seed in 0..5 {
            let state1 = test_execution(seed, link.clone());
            let state2 = test_execution(seed, link.clone());
            assert_eq!(state1, state2);
        }
    }

    #[test_traced("INFO")]
    fn test_execution_bad_links() {
        let link = Link {
            latency: Duration::from_millis(200),
            jitter: Duration::from_millis(150),
            success_rate: 0.75,
        };
        for seed in 0..5 {
            let state1 = test_execution(seed, link.clone());
            let state2 = test_execution(seed, link.clone());
            assert_eq!(state1, state2);
        }
    }
}
