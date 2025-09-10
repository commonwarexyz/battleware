use crate::{events::Stream, Error, Result};
use battleware_types::{
    api::{Lookup, Pending, Submission, Summary, Update, UpdatesFilter},
    execution::{Key, Seed, Transaction},
    Identity,
};
use commonware_codec::{DecodeExt, Encode};
use commonware_cryptography::{Hasher, Sha256};
use commonware_utils::hex;
use reqwest::Client as HttpClient;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tracing::{debug, info};
use url::Url;

/// Timeout for connections and requests
const TIMEOUT: Duration = Duration::from_secs(30);

/// Battleware API client
#[derive(Clone)]
pub struct Client {
    pub base_url: Url,
    pub ws_url: Url,
    pub http_client: HttpClient,

    pub identity: Identity,
}

impl Client {
    /// Create a new client
    #[allow(clippy::result_large_err)]
    pub fn new(base_url: &str, identity: Identity) -> Self {
        let base_url = Url::parse(base_url).unwrap();

        // Convert http(s) to ws(s) for WebSocket URL
        let ws_scheme = match base_url.scheme() {
            "http" => "ws",
            "https" => "wss",
            scheme => {
                panic!("Invalid scheme: {scheme}");
            }
        };

        let mut ws_url = base_url.clone();
        ws_url.set_scheme(ws_scheme).unwrap();

        let http_client = HttpClient::builder().timeout(TIMEOUT).build().unwrap();

        Self {
            base_url,
            ws_url,
            http_client,
            identity,
        }
    }

    /// Submit a transaction
    pub async fn submit_transactions(&self, txs: Vec<Transaction>) -> Result<()> {
        let submission = Submission::Transactions(txs);
        self.submit(submission).await
    }

    pub async fn submit_summary(&self, summary: Summary) -> Result<()> {
        let submission = Submission::Summary(summary);
        self.submit(submission).await
    }

    pub async fn submit_seed(&self, seed: Seed) -> Result<()> {
        let submission = Submission::Seed(seed);
        self.submit(submission).await
    }

    async fn submit(&self, submission: Submission) -> Result<()> {
        let encoded = submission.encode();
        let url = self.base_url.join("submit")?;
        debug!("Submitting to {}", url);

        let response = self
            .http_client
            .post(url)
            .body(encoded.to_vec())
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(Error::Failed(response.status()));
        }
        Ok(())
    }

    /// Query state by key
    pub async fn query_state(&self, key: &Key) -> Result<Option<Lookup>> {
        // Make request
        let key_hash = Sha256::hash(&key.encode());
        let url = self.base_url.join(&format!(
            "state/{}",
            commonware_utils::hex(&key_hash.encode())
        ))?;
        let response = self.http_client.get(url).send().await?;

        // Parse response
        match response.status() {
            reqwest::StatusCode::OK => {
                let buf = response.bytes().await?.to_vec();
                let lookup = Lookup::decode(&mut buf.as_slice())?;

                // Verify the lookup
                if !lookup.verify(&self.identity) {
                    return Err(Error::InvalidSignature);
                }

                Ok(Some(lookup))
            }
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            _ => Err(Error::Failed(response.status())),
        }
    }

    /// Connect to the updates stream with the specified filter
    pub async fn connect_updates(&self, filter: UpdatesFilter) -> Result<Stream<Update>> {
        let filter = hex(&filter.encode());
        let ws_url = self.ws_url.join(&format!("updates/{filter}"))?;
        info!(
            "Connecting to WebSocket at {} with filter {:?}",
            ws_url, filter
        );

        let (ws_stream, _) = timeout(TIMEOUT, connect_async(ws_url.as_str()))
            .await
            .map_err(|_| Error::DialTimeout)??;
        info!("WebSocket connected");

        Ok(Stream::new_with_verifier(ws_stream, self.identity))
    }

    /// Connect to the mempool stream (transactions)
    pub async fn connect_mempool(&self) -> Result<Stream<Pending>> {
        let ws_url = self.ws_url.join("mempool")?;
        info!("Connecting to WebSocket at {}", ws_url);

        let (ws_stream, _) = timeout(TIMEOUT, connect_async(ws_url.as_str()))
            .await
            .map_err(|_| Error::DialTimeout)??;
        info!("WebSocket connected");

        Ok(Stream::new(ws_stream))
    }
}
