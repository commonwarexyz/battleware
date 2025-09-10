mod state_machine;

use anyhow::Result;
use battleware_client::Client;
use battleware_types::{
    api::{Update, UpdatesFilter, MAX_SUBMISSION_TRANSACTIONS},
    execution::{Account, Key, Value},
    Query,
};
use commonware_cryptography::{
    bls12381::primitives::variant::{MinSig, Variant},
    ed25519::{PrivateKey, PublicKey},
    PrivateKeyExt, Signer,
};
use commonware_macros::select;
use commonware_runtime::{Clock, Metrics, Spawner};
use commonware_storage::store::operation::Variable;
use futures::future::join_all;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use state_machine::Engine as BotEngine;
use std::{
    collections::{hash_map::Entry, HashMap},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use tracing::{error, info, warn};

const CONNECTION_STALE_THRESHOLD: Duration = Duration::from_secs(10);
const UPLOADS_OUTSTANDING_WAIT_THRESHOLD: Duration = Duration::from_millis(250);
const STUCK_THRESHOLD: usize = 3;
const STUCK_EXIT_THRESHOLD: usize = 20;
const BATCH_FETCH_SIZE: usize = 50;
const MAX_UPLOADS_OUTSTANDING: usize = 5;
pub const SEED_LENGTH: usize = 32;

/// Configuration for the randotron engine
pub struct EngineConfig {
    pub num_keys: usize,
    pub network_identity: <MinSig as Variant>::Public,
    pub seed: [u8; SEED_LENGTH],
}

/// Configuration for randotron deployment (from config file)
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub num_keys: usize,
    pub base_url: String,
    pub network_identity: String,
    pub log_level: String,
    pub seed: String,
    pub worker_threads: usize,
}

/// Main engine for running the randotron
pub struct Engine<E: Clock + Spawner + Metrics> {
    context: E,
    config: EngineConfig,
    client: Client,
    bot: BotEngine,
    last_account_batch: Option<Instant>,
}

impl<E: Clock + Spawner + Metrics> Engine<E> {
    pub async fn new(context: E, config: EngineConfig, client: Client) -> Result<Self> {
        // Initialize bot engine
        let bot = BotEngine::new(config.network_identity);

        Ok(Self {
            context,
            config,
            client,
            bot,
            last_account_batch: None,
        })
    }

    async fn load_account(
        client: &Client,
        account: &PublicKey,
    ) -> Result<Option<(Account, Option<Value>)>> {
        // Fetch current state
        let Some(state) = client.query_state(&Key::Account(account.clone())).await? else {
            error!("Account {} not found", account);
            return Ok(None);
        };
        let Variable::Update(_, Value::Account(state)) = state.operation else {
            panic!("Expected account update");
        };

        // If account in battle, fetch battle info
        let battle = if let Some(battle) = state.battle {
            // Both of these errors can happen during asynchrony
            let Some(battle) = client.query_state(&Key::Battle(battle)).await? else {
                return Err(anyhow::anyhow!("Battle not found"));
            };
            let Variable::Update(_, battle) = battle.operation else {
                return Err(anyhow::anyhow!("Expected battle update"));
            };
            Some(battle)
        } else {
            None
        };

        Ok(Some((state, battle)))
    }

    async fn load_accounts(&mut self, accounts: Vec<PublicKey>) {
        // Process accounts in batches
        for chunk in accounts.chunks(BATCH_FETCH_SIZE) {
            // Load batch concurrently
            let client = &self.client;
            let jobs = chunk.iter().map(|public| async move {
                Self::load_account(client, public)
                    .await
                    .unwrap_or_else(|e| {
                        warn!(?public, ?e, "Failed to load account");
                        None
                    })
            });
            let results = join_all(jobs).await;

            // Process results and refresh bot state
            for (public, result) in chunk.iter().cloned().zip(results) {
                let Some((state, battle)) = result else {
                    continue;
                };
                self.bot.refresh_account(public, state, battle);
            }
        }
    }

    pub async fn run(mut self) {
        // Create new RNG (must recreate to start with same accounts each loop)
        let mut rng = ChaCha20Rng::from_seed(self.config.seed);

        // Subscribe to all updates
        let mut stream = self
            .client
            .connect_updates(UpdatesFilter::All)
            .await
            .expect("failed to connect to updates");

        // Loop until stream is closed
        let mut seed_cache = HashMap::new();
        let uploads_outstanding = Arc::new(AtomicUsize::new(0));
        loop {
            // Check if we need to add more accounts
            let now = Instant::now();
            if self.bot.accounts() < self.config.num_keys {
                let should_add = self
                    .last_account_batch
                    .is_none_or(|last| now.duration_since(last) >= Duration::from_secs(1));
                if should_add {
                    // Generate private key
                    let private = PrivateKey::from_rng(&mut rng);
                    let public = private.public_key();
                    self.bot.add_account(private);

                    // Load accounts
                    self.load_accounts(vec![public]).await;
                    info!(
                        elapsed = ?now.elapsed(),
                        total = self.bot.accounts(),
                        "Initialized account",
                    );

                    // Update last batch time
                    self.last_account_batch = Some(Instant::now());
                }
            }

            // Process update
            let (is_seed, requested, mut txs) = select! {
                update = stream.next() => {
                    let update = update.expect("stream closed").expect("failed to handle update");
                    let is_seed = matches!(update, Update::Seed(_));
                    let (requested, txs) = self.bot.apply(update);
                    (is_seed, requested, txs)
                },
                _ = self.context.sleep(CONNECTION_STALE_THRESHOLD) => {
                    warn!("Connection stale");
                    return;
                }
            };

            // Collect necessary seeds
            if is_seed && requested.is_empty() && !seed_cache.is_empty() {
                // No missing seeds necessary, ok to clear
                warn!(size = seed_cache.len(), "Cleared missing seeds");
                seed_cache.clear();
            }
            for index in requested {
                // Fetch missing seed
                if let Entry::Vacant(entry) = seed_cache.entry(index) {
                    let Ok(Some(seed)) = self.client.query_seed(Query::Index(index)).await else {
                        warn!("Failed to request seed: {}", index);
                        continue;
                    };
                    entry.insert(seed);
                }

                // Apply seed (don't worry about missing seeds, we can get them next time if still needed)
                let (_, new_txs) = self.bot.apply_seed(seed_cache[&index].clone());
                txs.extend(new_txs);
            }

            // Wait for uploads to be under max
            loop {
                let outstanding = uploads_outstanding.load(Ordering::Relaxed);
                if outstanding < MAX_UPLOADS_OUTSTANDING {
                    break;
                }
                warn!(outstanding, "Waiting for uploads to be under max");
                self.context.sleep(UPLOADS_OUTSTANDING_WAIT_THRESHOLD).await;
            }

            // Submit transactions (fire-and-forget)
            while !txs.is_empty() {
                uploads_outstanding.fetch_add(1, Ordering::Relaxed);
                let chunk: Vec<_> = txs
                    .drain(..MAX_SUBMISSION_TRANSACTIONS.min(txs.len()))
                    .collect();
                self.context.with_label("submit").spawn({
                    let client = self.client.clone();
                    let uploads_outstanding = uploads_outstanding.clone();
                    move |_| async move {
                        if let Err(e) = client.submit_transactions(chunk).await {
                            warn!("Failed to submit transaction: {}", e);
                        }
                        uploads_outstanding.fetch_sub(1, Ordering::Relaxed);
                    }
                });
            }

            // Check for stuck accounts
            //
            // This can occur when we reload an account and assign it to the "battle" state even though
            // the battle has just ended (can occur because of asynchrony)
            let stuck = self.bot.stuck(STUCK_THRESHOLD);
            assert!(
                stuck.len() <= STUCK_EXIT_THRESHOLD,
                "Exceeded stuck threshold"
            );
            if !stuck.is_empty() {
                warn!(?stuck, "Refreshing stuck accounts");
                self.load_accounts(stuck).await;
            }

            // Get stats
            let (uninitialized, generating, lobby, battle) = self.bot.stats();
            info!(uninitialized, generating, lobby, battle, "Stats");
        }
    }
}
