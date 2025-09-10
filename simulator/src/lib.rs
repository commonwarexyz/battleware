use axum::{
    body::Bytes,
    extract::{ws::WebSocketUpgrade, State as AxumState},
    http::{header, Method, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use battleware_types::{
    api::{Events, FilteredEvents, Lookup, Pending, Submission, Summary, Update, UpdatesFilter},
    execution::{Event, Output, Progress, Seed, Transaction, Value},
    Identity, Query, NAMESPACE,
};
use commonware_codec::{DecodeExt, Encode};
use commonware_consensus::{aggregation::types::Certificate, Viewable};
use commonware_cryptography::{
    bls12381::primitives::variant::MinSig, ed25519::PublicKey, sha256::Digest,
};
use commonware_storage::{
    adb::{
        create_multi_proof, create_proof, create_proof_store_from_digests,
        digests_required_for_proof,
    },
    store::operation::{Keyless, Variable},
};
use commonware_utils::from_hex;
use futures::{SinkExt, StreamExt};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, RwLock},
};
use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
pub enum InternalUpdate {
    Seed(Seed),
    Events(Events, Vec<(u64, Digest)>),
}

#[derive(Default)]
pub struct State {
    seeds: BTreeMap<u64, Seed>,

    nodes: BTreeMap<u64, Digest>,
    leaves: BTreeMap<u64, Variable<Digest, Value>>,
    #[allow(clippy::type_complexity)]
    keys: HashMap<Digest, BTreeMap<u64, (u64, Variable<Digest, Value>)>>,
    progress: BTreeMap<u64, (Progress, Certificate<MinSig, Digest>)>,

    submitted_events: HashSet<u64>,
    submitted_state: HashSet<u64>,
}

#[derive(Clone)]
pub struct Simulator {
    identity: Identity,
    state: Arc<RwLock<State>>,
    update_tx: broadcast::Sender<InternalUpdate>,
    mempool_tx: broadcast::Sender<Pending>,
}

impl Simulator {
    pub fn new(identity: Identity) -> Self {
        let (update_tx, _) = broadcast::channel(1024);
        let (mempool_tx, _) = broadcast::channel(1024);
        let state = Arc::new(RwLock::new(State::default()));

        Self {
            identity,
            state,
            update_tx,
            mempool_tx,
        }
    }
}

impl Simulator {
    pub fn submit_seed(&self, seed: Seed) {
        let mut state = self.state.write().unwrap();
        if state.seeds.insert(seed.view(), seed.clone()).is_some() {
            return;
        }
        let _ = self.update_tx.send(InternalUpdate::Seed(seed));
    }

    pub fn submit_transactions(&self, transactions: Vec<Transaction>) {
        let _ = self.mempool_tx.send(Pending { transactions });
    }

    pub fn submit_state(&self, summary: Summary, inner: Vec<(u64, Digest)>) {
        let mut state = self.state.write().unwrap();
        if !state.submitted_state.insert(summary.progress.height) {
            return;
        }

        // Store node digests
        for (pos, digest) in inner {
            state.nodes.insert(pos, digest);
        }

        // Store leaves
        let start_loc = summary.progress.state_start_op;
        for (i, value) in summary.state_proof_ops.into_iter().enumerate() {
            // Store in leaves
            let loc = start_loc + i as u64;
            state.leaves.insert(loc, value.clone());

            // Store in keys
            match value {
                Variable::Update(key, value) => {
                    state
                        .keys
                        .entry(key)
                        .or_default()
                        .insert(summary.progress.height, (loc, Variable::Update(key, value)));
                }
                Variable::Delete(key) => {
                    state
                        .keys
                        .entry(key)
                        .or_default()
                        .insert(summary.progress.height, (loc, Variable::Delete(key)));
                }
                _ => {}
            }
        }

        // Store progress at height to build proofs
        state.progress.insert(
            summary.progress.height,
            (summary.progress, summary.certificate),
        );
    }

    pub fn submit_events(&self, summary: Summary, events_digests: Vec<(u64, Digest)>) {
        let mut state = self.state.write().unwrap();
        let height = summary.progress.height;
        if !state.submitted_events.insert(height) {
            return;
        }

        // Broadcast events with digests for efficient filtering
        let _ = self.update_tx.send(InternalUpdate::Events(
            Events {
                progress: summary.progress,
                certificate: summary.certificate,
                events_proof: summary.events_proof,
                events_proof_ops: summary.events_proof_ops,
            },
            events_digests,
        ));
    }

    pub fn query_state(&self, key: &Digest) -> Option<Lookup> {
        let state = self.state.read().unwrap();
        let (height, operation) = state.keys.get(key)?.last_key_value()?;
        let (loc, Variable::Update(_, value)) = operation else {
            return None;
        };

        // Get progress and certificate
        let (progress, certificate) = state.progress.get(height)?;

        // Get required nodes
        let required_digest_positions =
            digests_required_for_proof::<Digest>(progress.state_end_op, *loc, *loc);
        let required_digests = required_digest_positions
            .iter()
            .map(|pos| state.nodes.get(pos).cloned().unwrap())
            .collect::<Vec<_>>();

        // Construct proof
        let proof = create_proof(progress.state_end_op, required_digests);

        Some(Lookup {
            progress: *progress,
            certificate: certificate.clone(),
            proof,
            location: *loc,
            operation: Variable::Update(*key, value.clone()),
        })
    }

    pub fn query_seed(&self, query: &Query) -> Option<Seed> {
        let state = self.state.read().unwrap();
        match query {
            Query::Latest => state.seeds.last_key_value().map(|(_, seed)| seed.clone()),
            Query::Index(index) => state.seeds.get(index).cloned(),
        }
    }

    pub fn update_subscriber(&self) -> broadcast::Receiver<InternalUpdate> {
        self.update_tx.subscribe()
    }

    pub fn mempool_subscriber(&self) -> broadcast::Receiver<Pending> {
        self.mempool_tx.subscribe()
    }
}

pub struct Api {
    simulator: Arc<Simulator>,
}

impl Api {
    pub fn new(simulator: Arc<Simulator>) -> Self {
        Self { simulator }
    }

    pub fn router(&self) -> Router {
        // Configure CORS
        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
            .allow_headers([header::CONTENT_TYPE]);

        Router::new()
            .route("/submit", post(submit))
            .route("/seed/:query", get(query_seed))
            .route("/state/:query", get(query_state))
            .route("/updates/:filter", get(updates_ws))
            .route("/mempool", get(mempool_ws))
            .layer(cors)
            .with_state(self.simulator.clone())
    }
}

async fn submit(AxumState(simulator): AxumState<Arc<Simulator>>, body: Bytes) -> impl IntoResponse {
    let submission = match Submission::decode(&mut body.as_ref()) {
        Ok(submission) => submission,
        Err(_) => return StatusCode::BAD_REQUEST,
    };

    match submission {
        Submission::Seed(seed) => {
            if !seed.verify(NAMESPACE, &simulator.identity) {
                return StatusCode::BAD_REQUEST;
            }
            simulator.submit_seed(seed);
            StatusCode::OK
        }
        Submission::Transactions(txs) => {
            simulator.submit_transactions(txs);
            StatusCode::OK
        }
        Submission::Summary(summary) => {
            let Some((state_digests, events_digests)) = summary.verify(&simulator.identity) else {
                return StatusCode::BAD_REQUEST;
            };
            simulator.submit_events(summary.clone(), events_digests);
            simulator.submit_state(summary, state_digests);
            StatusCode::OK
        }
    }
}

async fn query_state(
    AxumState(simulator): AxumState<Arc<Simulator>>,
    axum::extract::Path(query): axum::extract::Path<String>,
) -> impl IntoResponse {
    let raw = match from_hex(&query) {
        Some(raw) => raw,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };
    let key = match Digest::decode(&mut raw.as_slice()) {
        Ok(key) => key,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    match simulator.query_state(&key) {
        Some(value) => (StatusCode::OK, value.encode().to_vec()).into_response(),
        None => (StatusCode::NOT_FOUND, vec![]).into_response(),
    }
}

async fn query_seed(
    AxumState(simulator): AxumState<Arc<Simulator>>,
    axum::extract::Path(query): axum::extract::Path<String>,
) -> impl IntoResponse {
    let raw = match from_hex(&query) {
        Some(raw) => raw,
        None => return StatusCode::BAD_REQUEST.into_response(),
    };
    let query = match Query::decode(&mut raw.as_slice()) {
        Ok(query) => query,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    match simulator.query_seed(&query) {
        Some(seed) => (StatusCode::OK, seed.encode().to_vec()).into_response(),
        None => (StatusCode::NOT_FOUND, vec![]).into_response(),
    }
}

async fn updates_ws(
    AxumState(simulator): AxumState<Arc<Simulator>>,
    axum::extract::Path(filter): axum::extract::Path<String>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_updates_ws(socket, simulator, filter))
}

async fn mempool_ws(
    AxumState(simulator): AxumState<Arc<Simulator>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_mempool_ws(socket, simulator))
}

async fn handle_updates_ws(
    socket: axum::extract::ws::WebSocket,
    simulator: Arc<Simulator>,
    filter: String,
) {
    let (mut sender, _receiver) = socket.split();
    let mut updates = simulator.update_subscriber();

    // Parse filter from URL path using UpdatesFilter
    let filter = match from_hex(&filter) {
        Some(filter) => filter,
        None => return,
    };
    let subscription = match UpdatesFilter::decode(&mut filter.as_slice()) {
        Ok(subscription) => subscription,
        Err(_) => return,
    };

    // Send updates based on subscription
    while let Ok(internal_update) = updates.recv().await {
        // Convert InternalUpdate to Update and apply filtering
        let update = match internal_update {
            InternalUpdate::Seed(seed) => Some(Update::Seed(seed)),
            InternalUpdate::Events(events, digests) => match &subscription {
                UpdatesFilter::All => Some(Update::Events(events)),
                UpdatesFilter::Account(account) => {
                    filter_updates_for_account(events, digests, account).await
                }
            },
        };
        let Some(update) = update else {
            continue;
        };

        // Send update
        if sender
            .send(axum::extract::ws::Message::Binary(update.encode().to_vec()))
            .await
            .is_err()
        {
            break;
        }
    }
}

async fn handle_mempool_ws(socket: axum::extract::ws::WebSocket, simulator: Arc<Simulator>) {
    let (mut sender, _receiver) = socket.split();
    let mut txs = simulator.mempool_subscriber();

    while let Ok(tx) = txs.recv().await {
        if sender
            .send(axum::extract::ws::Message::Binary(tx.encode().to_vec()))
            .await
            .is_err()
        {
            break;
        }
    }
}

async fn filter_updates_for_account(
    events: Events,
    digests: Vec<(u64, Digest)>,
    account: &PublicKey,
) -> Option<Update> {
    // Determine which operations to include
    let mut filtered_ops = Vec::new();
    for (i, op) in events.events_proof_ops.into_iter().enumerate() {
        let should_include = match &op {
            Keyless::Append(output) => match output {
                Output::Event(event) => is_event_relevant_to_account(event, account),
                Output::Transaction(tx) => tx.public == *account,
                _ => false,
            },
            Keyless::Commit(_) => false,
        };
        if should_include {
            // Convert index to absolute location
            filtered_ops.push((events.progress.events_start_op + i as u64, op));
        }
    }

    // If no relevant events, skip this update entirely
    if filtered_ops.is_empty() {
        return None;
    }

    // Create a ProofStore directly from the pre-verified digests
    // Use the size from the original proof, not the operation count
    let proof_store = create_proof_store_from_digests(&events.events_proof, digests);

    // Generate a filtered proof for only the relevant locations
    let locations_to_include = filtered_ops.iter().map(|(loc, _)| *loc).collect::<Vec<_>>();
    let filtered_proof = create_multi_proof(&proof_store, &locations_to_include)
        .await
        .expect("failed to generate filtered proof");
    Some(Update::FilteredEvents(FilteredEvents {
        progress: events.progress,
        certificate: events.certificate,
        events_proof: filtered_proof,
        events_proof_ops: filtered_ops,
    }))
}

fn is_event_relevant_to_account(event: &Event, account: &PublicKey) -> bool {
    match event {
        Event::Generated {
            account: player, ..
        } => account == player,
        Event::Matched {
            player_a, player_b, ..
        } => player_a == account || player_b == account,
        Event::Locked {
            locker, observer, ..
        } => locker == account || observer == account,
        Event::Moved {
            player_a, player_b, ..
        } => player_a == account || player_b == account,
        Event::Settled {
            player_a, player_b, ..
        } => player_a == account || player_b == account,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use battleware_execution::mocks::{
        create_account_keypair, create_adbs, create_network_keypair, create_seed, execute_block,
    };
    use battleware_types::execution::{Instruction, Key, Stats, Transaction, Value};
    use commonware_cryptography::{Hasher, Sha256};
    use commonware_runtime::{deterministic::Runner, Runner as _};
    use commonware_storage::store::operation::Variable;
    use futures::executor::block_on;

    #[test]
    fn test_submit_seed() {
        let (network_secret, network_identity) = create_network_keypair();
        let simulator = Simulator::new(network_identity);
        let mut update_stream = simulator.update_subscriber();

        // Submit seed
        let seed = create_seed(&network_secret, 1);
        simulator.submit_seed(seed.clone());
        let received_update = block_on(async { update_stream.recv().await.unwrap() });
        match received_update {
            InternalUpdate::Seed(received_seed) => assert_eq!(received_seed, seed),
            _ => panic!("Expected seed update"),
        }
        assert_eq!(simulator.query_seed(&Query::Latest), Some(seed.clone()));
        assert_eq!(simulator.query_seed(&Query::Index(1)), Some(seed));

        // Submit another seed
        let seed = create_seed(&network_secret, 3);
        simulator.submit_seed(seed.clone());
        let received_update = block_on(async { update_stream.recv().await.unwrap() });
        match received_update {
            InternalUpdate::Seed(received_seed) => assert_eq!(received_seed, seed),
            _ => panic!("Expected seed update"),
        }
        assert_eq!(simulator.query_seed(&Query::Latest), Some(seed.clone()));
        assert_eq!(simulator.query_seed(&Query::Index(2)), None);
        assert_eq!(simulator.query_seed(&Query::Index(3)), Some(seed.clone()));
    }

    #[test]
    fn test_submit_transaction() {
        let (_, network_identity) = create_network_keypair();
        let simulator = Simulator::new(network_identity);
        let mut mempool_rx = simulator.mempool_subscriber();

        let (private, _) = create_account_keypair(1);
        let tx = Transaction::sign(&private, 1, Instruction::Generate);

        simulator.submit_transactions(vec![tx.clone()]);

        let received_txs = block_on(async { mempool_rx.recv().await.unwrap() });
        assert_eq!(received_txs.transactions.len(), 1);
        let received_tx = &received_txs.transactions[0];
        assert_eq!(received_tx.public, tx.public);
        assert_eq!(received_tx.nonce, tx.nonce);
    }

    #[test]
    fn test_submit_summary() {
        let executor = Runner::default();
        executor.start(|context| async move {
            // Initialize databases
            let (network_secret, network_identity) = create_network_keypair();
            let simulator = Simulator::new(network_identity);
            let (mut state, mut events) = create_adbs(&context).await;

            // Create mock transaction
            let (private, public) = create_account_keypair(1);
            let tx = Transaction::sign(&private, 0, Instruction::Generate);

            // Create summary using helper
            let (_, summary) = execute_block(
                &network_secret,
                network_identity,
                &mut state,
                &mut events,
                1, // view
                vec![tx],
            )
            .await;

            // Verify the summary
            let (state_digests, events_digests) = summary
                .verify(&network_identity)
                .expect("Summary verification failed");

            // Submit events
            let mut update_stream = simulator.update_subscriber();
            simulator.submit_events(summary.clone(), events_digests);

            // Wait for events
            let update_recv = update_stream.recv().await.unwrap();
            match update_recv {
                InternalUpdate::Events(events_recv, _) => {
                    assert!(events_recv.verify(&network_identity));
                    assert_eq!(events_recv.events_proof, summary.events_proof);
                    assert_eq!(events_recv.events_proof_ops, summary.events_proof_ops);
                }
                _ => panic!("Expected events update"),
            }

            // Submit state
            simulator.submit_state(summary.clone(), state_digests);

            // Query for state
            let account_key = Sha256::hash(&Key::Account(public.clone()).encode());
            let lookup = simulator.query_state(&account_key).unwrap();
            assert!(lookup.verify(&network_identity));
            let Variable::Update(_, Value::Account(account)) = lookup.operation else {
                panic!("account not found");
            };
            assert_eq!(account.nonce, 1);
            assert_eq!(account.battle, None);
            assert_eq!(account.stats, Stats::default());

            // Query for non-existent account
            let (_, other_public) = create_account_keypair(2);
            let other_key = Sha256::hash(&Key::Account(other_public).encode());
            assert!(simulator.query_state(&other_key).is_none());
        });
    }

    #[test]
    fn test_filtered_events() {
        let executor = Runner::default();
        executor.start(|context| async move {
            // Initialize
            let (network_secret, network_identity) = create_network_keypair();
            let simulator = Simulator::new(network_identity);
            let (mut state, mut events) = create_adbs(&context).await;

            // Create multiple accounts
            let (private1, public1) = create_account_keypair(1);
            let (private2, _public2) = create_account_keypair(2);
            let (private3, _public3) = create_account_keypair(3);

            // Create transactions from all accounts
            let txs = vec![
                Transaction::sign(&private1, 0, Instruction::Generate),
                Transaction::sign(&private2, 0, Instruction::Generate),
                Transaction::sign(&private3, 0, Instruction::Generate),
            ];

            // Execute block
            let (_, summary) = execute_block(
                &network_secret,
                network_identity,
                &mut state,
                &mut events,
                1, // view
                txs,
            )
            .await;

            // Submit the summary
            let (state_digests, events_digests) = summary.verify(&network_identity).unwrap();
            simulator.submit_events(summary.clone(), events_digests.clone());
            simulator.submit_state(summary.clone(), state_digests);

            // Store original count before moving
            let original_ops_count = summary.events_proof_ops.len();

            let events = Events {
                progress: summary.progress,
                certificate: summary.certificate,
                events_proof: summary.events_proof,
                events_proof_ops: summary.events_proof_ops,
            };

            // Apply filter
            let filtered = filter_updates_for_account(events, events_digests, &public1)
                .await
                .unwrap();

            // Verify filtered events
            match filtered {
                Update::FilteredEvents(filtered_events) => {
                    // Count how many events are included
                    let included_count = filtered_events.events_proof_ops.len();

                    // Verify we only have events related to account1
                    for (_loc, op) in &filtered_events.events_proof_ops {
                        if let Keyless::Append(Output::Event(Event::Generated {
                            account, ..
                        })) = op
                        {
                            assert_eq!(
                                account, &public1,
                                "Filtered events should only contain account1"
                            );
                        }
                    }

                    // We should have filtered out events for account2 and account3
                    assert!(
                        included_count > 0,
                        "Should have at least one included event"
                    );
                    assert!(
                        included_count < original_ops_count,
                        "Should have filtered out some events"
                    );

                    // Verify the proof still validates with multi-proof verification
                    assert!(
                        filtered_events.verify(&network_identity),
                        "Multi-proof verification should pass"
                    );
                }
                _ => panic!("Expected FilteredEvents"),
            }
        });
    }

    #[test]
    fn test_multiple_transactions_per_block() {
        let executor = Runner::default();
        executor.start(|context| async move {
            // Initialize
            let (network_secret, network_identity) = create_network_keypair();
            let simulator = Simulator::new(network_identity);
            let (mut state, mut events) = create_adbs(&context).await;

            // Create multiple accounts
            let accounts: Vec<_> = (0..5).map(create_account_keypair).collect();

            // Block 1: Multiple account generations in a single block
            let txs1: Vec<_> = accounts
                .iter()
                .map(|(private, _)| Transaction::sign(private, 0, Instruction::Generate))
                .collect();

            let (_, summary1) = execute_block(
                &network_secret,
                network_identity,
                &mut state,
                &mut events,
                1, // view
                txs1.clone(),
            )
            .await;

            // Verify and submit
            let (state_digests1, events_digests1) = summary1
                .verify(&network_identity)
                .expect("Summary 1 verification failed");
            simulator.submit_events(summary1.clone(), events_digests1);
            simulator.submit_state(summary1.clone(), state_digests1);

            // Verify height was inferred correctly (should be 1)
            assert_eq!(summary1.progress.height, 1);

            // Query each account to verify they were created
            for (_, public) in accounts.iter() {
                let account_key = Sha256::hash(&Key::Account(public.clone()).encode());
                let lookup = simulator.query_state(&account_key).unwrap();
                assert!(lookup.verify(&network_identity));
                let Variable::Update(_, Value::Account(account)) = lookup.operation else {
                    panic!("Account not found for {public:?}");
                };
                assert_eq!(account.nonce, 1);
                assert!(account.creature.is_some());
            }

            // Block 2: Multiple transactions from subset of accounts
            let txs2: Vec<_> = accounts
                .iter()
                .take(3)
                .map(|(private, _)| Transaction::sign(private, 1, Instruction::Generate))
                .collect();

            let (_, summary2) = execute_block(
                &network_secret,
                network_identity,
                &mut state,
                &mut events,
                5, // view
                txs2,
            )
            .await;

            // Verify and submit
            let (state_digests2, events_digests2) = summary2
                .verify(&network_identity)
                .expect("Summary 2 verification failed");
            simulator.submit_events(summary2.clone(), events_digests2);
            simulator.submit_state(summary2.clone(), state_digests2);

            // Verify height was inferred correctly (should be 2)
            assert_eq!(summary2.progress.height, 2);

            // Query accounts to verify nonce updates
            for (i, (_, public)) in accounts.iter().enumerate() {
                let account_key = Sha256::hash(&Key::Account(public.clone()).encode());
                let lookup = simulator.query_state(&account_key).unwrap();
                assert!(lookup.verify(&network_identity));
                let Variable::Update(_, Value::Account(account)) = lookup.operation else {
                    panic!("Account not found for {public:?}");
                };
                // First 3 accounts should have nonce 2, others still 1
                let expected_nonce = if i < 3 { 2 } else { 1 };
                assert_eq!(account.nonce, expected_nonce);
            }
        });
    }
}
