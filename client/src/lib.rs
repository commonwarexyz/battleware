pub mod client;
pub mod consensus;
pub mod events;

pub use client::Client;
pub use events::Stream;
use thiserror::Error;

/// Error type for client operations.
#[derive(Error, Debug)]
pub enum Error {
    #[error("reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("tungstenite error: {0}")]
    Tungstenite(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("failed: {0}")]
    Failed(reqwest::StatusCode),
    #[error("invalid data: {0}")]
    InvalidData(#[from] commonware_codec::Error),
    #[error("invalid signature")]
    InvalidSignature,
    #[error("unexpected response")]
    UnexpectedResponse,
    #[error("connection closed")]
    ConnectionClosed,
    #[error("URL parse error: {0}")]
    Url(#[from] url::ParseError),
    #[error("dial timeout")]
    DialTimeout,
}

/// Result type for client operations.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;
    use battleware_execution::mocks::{
        create_account_keypair, create_adbs, create_network_keypair, create_seed, execute_block,
    };
    use battleware_simulator::{Api, Simulator};
    use battleware_types::{
        api::{Update, UpdatesFilter},
        execution::{Instruction, Key, Stats, Transaction, Value},
        Identity, Query, Seed,
    };
    use commonware_consensus::Viewable;
    use commonware_cryptography::bls12381::primitives::group::Private;
    use commonware_runtime::{deterministic::Runner, Runner as _};
    use commonware_storage::store::operation::Variable;
    use std::{net::SocketAddr, sync::Arc};
    use tokio::time::{sleep, Duration};

    struct TestContext {
        network_secret: Private,
        network_identity: Identity,
        simulator: Arc<Simulator>,
        base_url: String,
        server_handle: tokio::task::JoinHandle<()>,
    }

    impl TestContext {
        async fn new() -> Self {
            let (network_secret, network_identity) = create_network_keypair();
            let simulator = Arc::new(Simulator::new(network_identity));
            let api = Api::new(simulator.clone());

            // Start server on random port
            let addr = SocketAddr::from(([127, 0, 0, 1], 0));
            let router = api.router();
            let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
            let actual_addr = listener.local_addr().unwrap();
            let base_url = format!("http://{actual_addr}");

            let server_handle = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });

            // Give server time to start
            sleep(Duration::from_millis(100)).await;

            Self {
                network_secret,
                network_identity,
                simulator,
                base_url,
                server_handle,
            }
        }

        fn create_client(&self) -> Client {
            Client::new(&self.base_url, self.network_identity)
        }

        fn create_seed(&self, view: u64) -> Seed {
            create_seed(&self.network_secret, view)
        }
    }

    impl Drop for TestContext {
        fn drop(&mut self) {
            self.server_handle.abort();
        }
    }

    #[tokio::test]
    async fn test_client_seed_operations() {
        let ctx = TestContext::new().await;
        let client = ctx.create_client();

        // Upload seed
        let seed = ctx.create_seed(1);
        client.submit_seed(seed.clone()).await.unwrap();

        // Get seed by index
        let retrieved = client.query_seed(Query::Index(1)).await.unwrap();
        assert_eq!(retrieved, Some(seed.clone()));

        // Get latest seed
        let latest = client.query_seed(Query::Latest).await.unwrap();
        assert_eq!(latest, Some(seed));

        // Upload another seed
        let seed2 = ctx.create_seed(5);
        client.submit_seed(seed2.clone()).await.unwrap();

        // Get latest should now return seed2
        let latest = client.query_seed(Query::Latest).await.unwrap();
        assert_eq!(latest, Some(seed2.clone()));

        // Get specific seed by index
        let retrieved = client.query_seed(Query::Index(5)).await.unwrap();
        assert_eq!(retrieved, Some(seed2));

        // Query for non-existent seed
        let result = client.query_seed(Query::Index(3)).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_client_transaction_submission() {
        let ctx = TestContext::new().await;
        let client = ctx.create_client();

        // Create and submit transaction
        let (private, _) = create_account_keypair(1);
        let tx = Transaction::sign(&private, 0, Instruction::Generate);

        // Should succeed even though transaction isn't processed yet
        client.submit_transactions(vec![tx]).await.unwrap();

        // Submit another transaction with higher nonce
        let tx2 = Transaction::sign(&private, 1, Instruction::Generate);
        client.submit_transactions(vec![tx2]).await.unwrap();
    }

    #[tokio::test]
    async fn test_client_summary_submission() {
        // Setup server outside deterministic runtime
        let ctx = TestContext::new().await;
        let client = ctx.create_client();
        let network_secret = ctx.network_secret.clone();
        let network_identity = ctx.network_identity;

        // Create transaction
        let (private, _) = create_account_keypair(1);
        let tx = Transaction::sign(&private, 0, Instruction::Generate);

        // Create summary in deterministic runtime
        let executor = Runner::default();
        let (_, summary) = executor.start(|context| async move {
            let (mut state, mut events) = create_adbs(&context).await;
            execute_block(
                &network_secret,
                network_identity,
                &mut state,
                &mut events,
                1, // view
                vec![tx],
            )
            .await
        });

        // Submit summary
        client.submit_summary(summary).await.unwrap();
    }

    #[tokio::test]
    async fn test_client_state_query() {
        // Setup server outside deterministic runtime
        let ctx = TestContext::new().await;
        let client = ctx.create_client();
        let network_secret = ctx.network_secret.clone();
        let network_identity = ctx.network_identity;
        let simulator = ctx.simulator.clone();

        // Create and process transaction
        let (private, public) = create_account_keypair(1);
        let tx = Transaction::sign(&private, 0, Instruction::Generate);

        // Create summary in deterministic runtime
        let executor = Runner::default();
        let (_, summary) = executor.start(|context| async move {
            let (mut state, mut events) = create_adbs(&context).await;
            execute_block(
                &network_secret,
                network_identity,
                &mut state,
                &mut events,
                1, // view
                vec![tx],
            )
            .await
        });

        // Submit to simulator
        let (state_digests, events_digests) = summary.verify(&network_identity).unwrap();
        simulator.submit_events(summary.clone(), events_digests);
        simulator.submit_state(summary, state_digests);

        // Query for account state
        let account_key = Key::Account(public.clone());
        let lookup = client.query_state(&account_key).await.unwrap();

        assert!(lookup.is_some());
        let lookup = lookup.unwrap();
        assert!(lookup.verify(&network_identity));

        // Verify account data
        let Variable::Update(_, Value::Account(account)) = lookup.operation else {
            panic!("Expected account value");
        };
        assert_eq!(account.nonce, 1);
        assert_eq!(account.stats, Stats::default());

        // Query for non-existent account
        let (_, other_public) = create_account_keypair(2);
        let other_key = Key::Account(other_public);
        let result = client.query_state(&other_key).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_client_updates_stream() {
        // Setup server outside deterministic runtime
        let ctx = TestContext::new().await;
        let client = ctx.create_client();
        let network_secret = ctx.network_secret.clone();
        let network_identity = ctx.network_identity;
        let simulator = ctx.simulator.clone();

        // Connect to updates stream for all events
        let mut stream = client.connect_updates(UpdatesFilter::All).await.unwrap();

        // Test seed update
        let seed = ctx.create_seed(10);
        simulator.submit_seed(seed.clone());

        let update = stream.next().await.unwrap().unwrap();
        match update {
            Update::Seed(received_seed) => {
                assert_eq!(received_seed, seed);
            }
            _ => panic!("Expected seed update"),
        }

        // Test events update
        let (private, _) = create_account_keypair(1);
        let tx = Transaction::sign(&private, 0, Instruction::Generate);

        // Create summary in deterministic runtime
        let executor = Runner::default();
        let (_, summary) = executor.start(|context| async move {
            let (mut state, mut events) = create_adbs(&context).await;
            execute_block(
                &network_secret,
                network_identity,
                &mut state,
                &mut events,
                1, // view
                vec![tx],
            )
            .await
        });

        // Submit events to simulator
        let (_state_digests, events_digests) = summary.verify(&network_identity).unwrap();
        simulator.submit_events(summary.clone(), events_digests);

        // Receive event update from stream
        let update = stream.next().await.unwrap().unwrap();
        match update {
            Update::Events(event) => {
                assert!(event.verify(&network_identity));
                assert_eq!(event.progress.height, 1);
                assert_eq!(event.events_proof_ops, summary.events_proof_ops);
            }
            _ => panic!("Expected events update"),
        }
    }

    #[tokio::test]
    async fn test_client_mempool_stream() {
        let ctx = TestContext::new().await;
        let client = ctx.create_client();

        // Connect to mempool stream
        let mut stream = client.connect_mempool().await.unwrap();

        // Submit transaction through simulator
        let (private, _) = create_account_keypair(1);
        let tx = Transaction::sign(&private, 0, Instruction::Generate);
        ctx.simulator.submit_transactions(vec![tx.clone()]);

        // Receive transaction from stream
        let received_txs = stream.next().await.unwrap().unwrap();
        assert_eq!(received_txs.transactions.len(), 1);
        let received_tx = &received_txs.transactions[0];
        assert_eq!(received_tx.public, tx.public);
        assert_eq!(received_tx.nonce, tx.nonce);
    }

    #[tokio::test]
    async fn test_client_get_current_view() {
        let ctx = TestContext::new().await;
        let client = ctx.create_client();

        // Submit a seed
        let seed = ctx.create_seed(42);
        ctx.simulator.submit_seed(seed);

        // Get current view
        let view = client.query_seed(Query::Latest).await.unwrap().unwrap();
        assert_eq!(view.view(), 42);
    }

    #[tokio::test]
    async fn test_client_query_seed() {
        let ctx = TestContext::new().await;
        let client = ctx.create_client();

        // Submit seed
        let seed = ctx.create_seed(15);
        ctx.simulator.submit_seed(seed.clone());

        // Query existing seed
        let result = client.query_seed(Query::Index(15)).await.unwrap();
        assert_eq!(result, Some(seed));

        // Query non-existent seed
        let result = client.query_seed(Query::Index(999)).await.unwrap();
        assert!(result.is_none());
    }
}
