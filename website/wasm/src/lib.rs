#[cfg(feature = "testing")]
use battleware_execution::mocks;
#[cfg(feature = "testing")]
use battleware_types::api::Summary;
use battleware_types::{
    api::{Lookup, Submission, Update, UpdatesFilter},
    execution::{
        transaction_namespace, Creature, Event, Instruction, Key, Leaderboard, Outcome, Output,
        Seed, Transaction as ExecutionTransaction, Value, NAMESPACE,
    },
    Identity, Query,
};
use commonware_codec::{Encode, ReadExt};
use commonware_consensus::threshold_simplex::types::{seed_namespace, view_message};
#[cfg(feature = "testing")]
use commonware_cryptography::bls12381::primitives::ops;
use commonware_cryptography::bls12381::{
    primitives::variant::MinSig,
    tle::{encrypt, Block},
};
use commonware_cryptography::{ed25519, Hasher, PrivateKeyExt, Sha256, Signer as _};
#[cfg(feature = "testing")]
use commonware_runtime::{deterministic::Runner, Runner as _};
use commonware_storage::store::operation::{Keyless, Variable};
use rand::rngs::OsRng;
#[cfg(feature = "testing")]
use rand::SeedableRng;
#[cfg(feature = "testing")]
use rand_chacha::ChaCha20Rng;
use serde::Serialize;
use serde_wasm_bindgen::Serializer;
use wasm_bindgen::prelude::*;

/// Helper to convert serde_json::Value to a plain JavaScript object
fn to_object(value: &serde_json::Value) -> Result<JsValue, JsValue> {
    value
        .serialize(&Serializer::json_compatible())
        .map_err(|e| JsValue::from_str(&format!("Failed to serialize: {e}")))
}

/// The key to use for signing transactions.
#[wasm_bindgen]
pub struct Signer {
    private_key: ed25519::PrivateKey,
    public_key: ed25519::PublicKey,
}

#[wasm_bindgen]
impl Signer {
    /// Generate a new signer from a random private key.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Result<Signer, JsValue> {
        let private_key = ed25519::PrivateKey::from_rng(&mut OsRng);
        let public_key = private_key.public_key();

        Ok(Signer {
            private_key,
            public_key,
        })
    }

    /// Create a signer from an encoded private key.
    #[wasm_bindgen]
    pub fn from_bytes(private_key_bytes: &[u8]) -> Result<Signer, JsValue> {
        let mut buf = private_key_bytes;
        let private_key = ed25519::PrivateKey::read(&mut buf)
            .map_err(|e| JsValue::from_str(&format!("Failed to create private key: {e:?}")))?;
        let public_key = private_key.public_key();

        Ok(Signer {
            private_key,
            public_key,
        })
    }

    /// Get the public key.
    #[wasm_bindgen(getter)]
    pub fn public_key(&self) -> Vec<u8> {
        self.public_key.as_ref().to_vec()
    }

    /// Get the public key as a hex string.
    #[wasm_bindgen(getter)]
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.public_key.as_ref())
    }

    /// Get the private key.
    #[wasm_bindgen(getter)]
    pub fn private_key(&self) -> Vec<u8> {
        self.private_key.as_ref().to_vec()
    }

    /// Get the private key as a hex string.
    #[wasm_bindgen(getter)]
    pub fn private_key_hex(&self) -> String {
        hex::encode(self.private_key.as_ref())
    }

    /// Sign a message.
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        self.private_key
            .sign(Some(&transaction_namespace(NAMESPACE)), message)
            .encode()
            .to_vec()
    }
}

/// An onchain transaction.
#[wasm_bindgen]
pub struct Transaction {
    inner: ExecutionTransaction,
}

#[wasm_bindgen]
impl Transaction {
    /// Sign a new generate transaction.
    #[wasm_bindgen]
    pub fn generate(signer: &Signer, nonce: u64) -> Result<Transaction, JsValue> {
        let instruction = Instruction::Generate;
        let tx = ExecutionTransaction::sign(&signer.private_key, nonce, instruction);
        Ok(Transaction { inner: tx })
    }

    /// Sign a new match transaction.
    #[wasm_bindgen]
    pub fn match_tx(signer: &Signer, nonce: u64) -> Result<Transaction, JsValue> {
        let instruction = Instruction::Match;
        let tx = ExecutionTransaction::sign(&signer.private_key, nonce, instruction);
        Ok(Transaction { inner: tx })
    }

    /// Sign a new move transaction.
    #[wasm_bindgen]
    pub fn move_tx(
        signer: &Signer,
        nonce: u64,
        identity: &[u8],
        expiry: u64,
        move_index: u8,
    ) -> Result<Transaction, JsValue> {
        // Parse identity
        let identity = decode_bls_public(identity)?;

        // Create seed namespace and view message
        let seed_ns = seed_namespace(NAMESPACE);
        let view_msg = view_message(expiry);

        // Create a 32-byte move message with the move data
        let mut message = [0u8; 32];
        message[0] = move_index; // First byte is the actual move

        // Encrypt the move
        let ciphertext = encrypt::<_, MinSig>(
            &mut OsRng,
            identity,
            (Some(&seed_ns), &view_msg[..]),
            &Block::new(message),
        );

        let instruction = Instruction::Move(ciphertext);
        let tx = ExecutionTransaction::sign(&signer.private_key, nonce, instruction);
        Ok(Transaction { inner: tx })
    }

    #[wasm_bindgen]
    pub fn settle_tx(signer: &Signer, nonce: u64, seed: &[u8]) -> Result<Transaction, JsValue> {
        // Parse the full seed structure
        let mut buf = seed;
        let seed = Seed::read(&mut buf)
            .map_err(|e| JsValue::from_str(&format!("Failed to parse seed: {e:?}")))?;

        // Extract just the signature from the seed
        let instruction = Instruction::Settle(seed.signature);
        let tx = ExecutionTransaction::sign(&signer.private_key, nonce, instruction);
        Ok(Transaction { inner: tx })
    }

    /// Encode the transaction.
    #[wasm_bindgen]
    pub fn encode(&self) -> Vec<u8> {
        self.inner.encode().to_vec()
    }
}

/// Encode an account key.
#[wasm_bindgen]
pub fn encode_account_key(public_key: &[u8]) -> Result<Vec<u8>, JsValue> {
    let mut buf = public_key;
    let pk = ed25519::PublicKey::read(&mut buf)
        .map_err(|e| JsValue::from_str(&format!("Invalid public key: {e:?}")))?;
    let key = Key::Account(pk);
    Ok(key.encode().to_vec())
}

/// Encode a battle key.
#[wasm_bindgen]
pub fn encode_battle_key(digest: &[u8]) -> Result<Vec<u8>, JsValue> {
    let key = Key::Battle(commonware_cryptography::sha256::Digest(
        digest.try_into().expect("Invalid digest length"),
    ));
    Ok(key.encode().to_vec())
}

/// Encode a leaderboard key.
#[wasm_bindgen]
pub fn encode_leaderboard_key() -> Vec<u8> {
    Key::Leaderboard.encode().to_vec()
}

/// Encode UpdatesFilter::All
#[wasm_bindgen]
pub fn encode_updates_filter_all() -> Vec<u8> {
    UpdatesFilter::All.encode().to_vec()
}

/// Encode UpdatesFilter::Account
#[wasm_bindgen]
pub fn encode_updates_filter_account(public_key: &[u8]) -> Result<Vec<u8>, JsValue> {
    let mut buf = public_key;
    let pk = ed25519::PublicKey::read(&mut buf)
        .map_err(|e| JsValue::from_str(&format!("Invalid public key: {e:?}")))?;
    Ok(UpdatesFilter::Account(pk).encode().to_vec())
}

/// Hash a key for state queries.
#[wasm_bindgen]
pub fn hash_key(key: &[u8]) -> Vec<u8> {
    let digest = Sha256::hash(key);
    digest.encode().to_vec()
}

/// Encode a query for the latest state.
#[wasm_bindgen]
pub fn encode_query_latest() -> Vec<u8> {
    let query = Query::Latest;
    query.encode().to_vec()
}

/// Encode a query for a specific index.
#[wasm_bindgen]
pub fn encode_query_index(index: u64) -> Vec<u8> {
    let query = Query::Index(index);
    query.encode().to_vec()
}

fn process_leaderboard(leaderboard: &Leaderboard) -> Vec<serde_json::Value> {
    leaderboard
        .players
        .iter()
        .map(|(public_key, stats)| {
            serde_json::json!([
                public_key.encode().to_vec(),
                {
                    "elo": stats.elo,
                    "wins": stats.wins,
                    "losses": stats.losses,
                    "draws": stats.draws
                }
            ])
        })
        .collect()
}

// Helper function to convert Value to JSON
fn decode_value(value: Value) -> Result<JsValue, JsValue> {
    // Convert to JSON
    let json = match value {
        Value::Account(account) => {
            serde_json::json!({
                "type": "Account",
                "nonce": account.nonce,
                "creature": account.creature.map(|c| {
                    serde_json::json!({
                        "traits": c.traits.to_vec()
                    })
                }),
                "battle": account.battle.map(|b| hex::encode(b.encode())),
                "elo": account.stats.elo,
                "wins": account.stats.wins,
                "losses": account.stats.losses,
                "draws": account.stats.draws
            })
        }
        Value::Lobby { expiry, players } => {
            serde_json::json!({
                "type": "Lobby",
                "expiry": expiry,
                "players": players.iter().map(|p| hex::encode(p.encode())).collect::<Vec<_>>()
            })
        }
        Value::Battle {
            expiry,
            round,
            player_a,
            player_a_max_health,
            player_a_health,
            player_a_pending,
            player_a_move_counts,
            player_b,
            player_b_max_health,
            player_b_health,
            player_b_pending,
            player_b_move_counts,
        } => {
            serde_json::json!({
                "type": "Battle",
                "expiry": expiry,
                "round": round,
                "player_a": hex::encode(player_a.encode()),
                "player_a_max_health": player_a_max_health,
                "player_a_health": player_a_health,
                "player_a_pending": player_a_pending.is_some(),
                "player_a_move_counts": player_a_move_counts.to_vec(),
                "player_b": hex::encode(player_b.encode()),
                "player_b_max_health": player_b_max_health,
                "player_b_health": player_b_health,
                "player_b_pending": player_b_pending.is_some(),
                "player_b_move_counts": player_b_move_counts.to_vec()
            })
        }
        Value::Commit { height, start: _ } => {
            serde_json::json!({
                "type": "Height",
                "height": height
            })
        }
        Value::Leaderboard(leaderboard) => {
            serde_json::json!({
                "type": "Leaderboard",
                "players": process_leaderboard(&leaderboard)
            })
        }
    };

    to_object(&json)
}

/// Decode a lookup response from the simulator.
/// The identity_bytes should be the simulator's identity for verification.
#[wasm_bindgen]
pub fn decode_lookup(lookup_bytes: &[u8], identity_bytes: &[u8]) -> Result<JsValue, JsValue> {
    // Decode the lookup
    let mut buf = lookup_bytes;
    let lookup = Lookup::read(&mut buf)
        .map_err(|e| JsValue::from_str(&format!("Failed to decode lookup: {e:?}")))?;

    // Decode the identity for verification
    let mut id_buf = identity_bytes;
    let identity = Identity::read(&mut id_buf)
        .map_err(|e| JsValue::from_str(&format!("Failed to decode identity: {e:?}")))?;

    // Verify the lookup
    if !lookup.verify(&identity) {
        return Err(JsValue::from_str("Lookup verification failed"));
    }

    // Extract the value from the operation
    let value = match lookup.operation {
        Variable::Update(_, value) => value,
        _ => return Err(JsValue::from_str("Expected Update operation in lookup")),
    };

    // Convert to JSON (reuse the logic from decode_value)
    decode_value(value)
}

/// Helper function to decode and verify a seed
fn decode_seed_internal(seed: Seed, identity: &Identity) -> Result<JsValue, JsValue> {
    // Verify the seed signature
    if !seed.verify(NAMESPACE, identity) {
        return Err(JsValue::from_str("invalid seed"));
    }

    // Include raw bytes for settle operations
    let bytes = seed.encode().to_vec();

    // Create response using serde_json for consistency
    let response = serde_json::json!({
        "type": "Seed",
        "view": seed.view,
        "bytes": bytes
    });

    to_object(&response)
}

/// Decode and verify a seed.
#[wasm_bindgen]
pub fn decode_seed(seed: &[u8], identity: &[u8]) -> Result<JsValue, JsValue> {
    let mut buf = seed;
    let seed = Seed::read(&mut buf)
        .map_err(|e| JsValue::from_str(&format!("Failed to decode seed: {e:?}")))?;

    // Decode the identity (BLS public key)
    let identity = decode_bls_public(identity)?;

    decode_seed_internal(seed, &identity)
}

/// Helper function to convert an Event to JSON
fn decode_event(event: &Event) -> Result<serde_json::Value, JsValue> {
    let json = match event {
        Event::Generated { account, creature } => {
            serde_json::json!({
                "type": "Generated",
                "account": hex::encode(account.encode()),
                "creature": {
                    "traits": creature.traits.to_vec()
                }
            })
        }
        Event::Matched {
            battle,
            expiry,
            player_a,
            player_a_creature,
            player_a_stats,
            player_b,
            player_b_creature,
            player_b_stats,
        } => {
            serde_json::json!({
                "type": "Matched",
                "battle": hex::encode(battle.encode()),
                "expiry": expiry,
                "player_a": hex::encode(player_a.encode()),
                "player_a_creature": {
                    "traits": player_a_creature.traits.to_vec()
                },
                "player_a_stats": {
                    "elo": player_a_stats.elo,
                    "wins": player_a_stats.wins,
                    "losses": player_a_stats.losses,
                    "draws": player_a_stats.draws
                },
                "player_b": hex::encode(player_b.encode()),
                "player_b_creature": {
                    "traits": player_b_creature.traits.to_vec()
                },
                "player_b_stats": {
                    "elo": player_b_stats.elo,
                    "wins": player_b_stats.wins,
                    "losses": player_b_stats.losses,
                    "draws": player_b_stats.draws
                }
            })
        }
        Event::Moved {
            battle,
            round,
            expiry,
            player_a,
            player_a_health,
            player_a_move,
            player_a_move_counts,
            player_a_power,
            player_b,
            player_b_health,
            player_b_move,
            player_b_move_counts,
            player_b_power,
        } => {
            serde_json::json!({
                "type": "Moved",
                "battle": hex::encode(battle.encode()),
                "round": round,
                "expiry": expiry,
                "player_a": hex::encode(player_a.encode()),
                "player_a_health": player_a_health,
                "player_a_move": player_a_move,
                "player_a_move_counts": player_a_move_counts.to_vec(),
                "player_a_power": player_a_power,
                "player_b": hex::encode(player_b.encode()),
                "player_b_health": player_b_health,
                "player_b_move": player_b_move,
                "player_b_move_counts": player_b_move_counts.to_vec(),
                "player_b_power": player_b_power
            })
        }
        Event::Settled {
            battle,
            round,
            player_a,
            player_a_old,
            player_a_new,
            player_b,
            player_b_old,
            player_b_new,
            outcome,
            leaderboard,
        } => {
            let outcome_str = match outcome {
                Outcome::PlayerA => "PlayerA",
                Outcome::PlayerB => "PlayerB",
                Outcome::Draw => "Draw",
            };

            serde_json::json!({
                "type": "Settled",
                "battle": hex::encode(battle.encode()),
                "round": round,
                "player_a": hex::encode(player_a.encode()),
                "player_a_old": {
                    "elo": player_a_old.elo,
                    "wins": player_a_old.wins,
                    "losses": player_a_old.losses,
                    "draws": player_a_old.draws
                },
                "player_a_new": {
                    "elo": player_a_new.elo,
                    "wins": player_a_new.wins,
                    "losses": player_a_new.losses,
                    "draws": player_a_new.draws
                },
                "player_b": hex::encode(player_b.encode()),
                "player_b_old": {
                    "elo": player_b_old.elo,
                    "wins": player_b_old.wins,
                    "losses": player_b_old.losses,
                    "draws": player_b_old.draws
                },
                "player_b_new": {
                    "elo": player_b_new.elo,
                    "wins": player_b_new.wins,
                    "losses": player_b_new.losses,
                    "draws": player_b_new.draws
                },
                "outcome": outcome_str,
                "leaderboard": process_leaderboard(leaderboard)
            })
        }
        Event::Locked {
            battle,
            round,
            locker,
            observer,
            ciphertext,
        } => {
            serde_json::json!({
                "type": "Locked",
                "battle": hex::encode(battle.encode()),
                "round": round,
                "locker": hex::encode(locker.encode()),
                "observer": hex::encode(observer.encode()),
                "ciphertext": hex::encode(ciphertext.encode())
            })
        }
    };
    Ok(json)
}

// Creature generation
#[wasm_bindgen]
pub fn generate_creature_from_traits(traits: &[u8]) -> Result<JsValue, JsValue> {
    let creature = Creature {
        traits: traits.try_into().expect("Invalid traits length"),
    };

    // Calculate derived stats
    let health = creature.health();
    let move_strengths = creature.get_move_strengths();
    let move_limits = creature.get_move_usage_limits();
    let json = serde_json::json!({
        "traits": creature.traits.to_vec(),
        "health": health,
        "moves": [
            {
                "index": 0,
                "name": "No-op",
                "strength": move_strengths[0],
                "usage_limit": move_limits[0],
                "is_defense": false
            },
            {
                "index": 1,
                "name": "Defense",
                "strength": move_strengths[1],
                "usage_limit": move_limits[1],
                "is_defense": true
            },
            {
                "index": 2,
                "name": "Attack 1",
                "strength": move_strengths[2],
                "usage_limit": move_limits[2],
                "is_defense": false
            },
            {
                "index": 3,
                "name": "Attack 2",
                "strength": move_strengths[3],
                "usage_limit": move_limits[3],
                "is_defense": false
            },
            {
                "index": 4,
                "name": "Attack 3",
                "strength": move_strengths[4],
                "usage_limit": move_limits[4],
                "is_defense": false
            }
        ]
    });

    to_object(&json)
}

/// Decode a BLS public key.
fn decode_bls_public(bytes: &[u8]) -> Result<Identity, JsValue> {
    let mut buf = bytes;
    let identity = Identity::read(&mut buf)
        .map_err(|e| JsValue::from_str(&format!("Failed to decode BLS public key: {e:?}")))?;
    Ok(identity)
}

#[cfg(feature = "testing")]
#[wasm_bindgen]
pub fn get_identity(seed: u64) -> Vec<u8> {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let (_, identity) = ops::keypair::<_, MinSig>(&mut rng);
    identity.encode().to_vec()
}

#[cfg(feature = "testing")]
#[wasm_bindgen]
pub fn encode_seed(seed: u64, view: u64) -> Vec<u8> {
    let mut rng = ChaCha20Rng::seed_from_u64(seed);
    let (network_secret, _) = ops::keypair::<_, MinSig>(&mut rng);

    let seed_namespace = seed_namespace(NAMESPACE);
    let message = view_message(view);
    let sig = ops::sign_message::<MinSig>(&network_secret, Some(&seed_namespace), &message);
    let seed = Seed::new(view, sig);

    seed.encode().to_vec()
}

/// Create a test summary with transactions for testing.
/// This creates a summary that processes the given transactions and updates state accordingly.
#[cfg(feature = "testing")]
#[wasm_bindgen]
pub fn execute_block(network_secret: u64, view: u64, tx_bytes: &[u8]) -> Result<Vec<u8>, JsValue> {
    // Create master keypair from seed
    let mut rng = ChaCha20Rng::seed_from_u64(network_secret);
    let (network_secret, network_identity) = ops::keypair::<_, MinSig>(&mut rng);

    // Decode all transactions from the buffer (its ok to be empty)
    let mut transactions = Vec::new();
    let mut buf = tx_bytes;
    while !buf.is_empty() {
        match ExecutionTransaction::read(&mut buf) {
            Ok(tx) => transactions.push(tx),
            Err(_) => break, // End of transactions
        }
    }

    // Create summary in deterministic runtime
    let executor = Runner::default();
    let (_, summary) = executor.start(|context| async move {
        let (mut state, mut events) = mocks::create_adbs(&context).await;
        mocks::execute_block(
            &network_secret,
            network_identity,
            &mut state,
            &mut events,
            view,
            transactions,
        )
        .await
    });

    Ok(summary.encode().to_vec())
}

/// Helper function to process an output into a JSON value
fn process_output(output: &Output) -> Result<serde_json::Value, JsValue> {
    match output {
        Output::Transaction(tx) => {
            let instruction = match &tx.instruction {
                Instruction::Generate => "Generate",
                Instruction::Match => "Match",
                Instruction::Move(_ciphertext) => "Move",
                Instruction::Settle(_signature) => "Settle",
            };
            Ok(serde_json::json!({
                "type": "Transaction",
                "nonce": tx.nonce,
                "public": hex::encode(&tx.public),
                "instruction": instruction
            }))
        }
        Output::Event(event) => decode_event(event),
        _ => Ok(serde_json::Value::Null),
    }
}

/// Helper function to process events (both regular and filtered)
fn process_events<'a, I>(ops_iter: I) -> Result<JsValue, JsValue>
where
    I: Iterator<Item = &'a Keyless<Output>>,
{
    // Process events - extract outputs
    let mut events_array = Vec::new();
    for op in ops_iter {
        if let Keyless::Append(output) = op {
            let json_value = process_output(output)?;
            if json_value.is_null() {
                continue;
            }
            events_array.push(json_value);
        }
    }

    // Create response using serde_json for consistency
    let response = serde_json::json!({
        "type": "Events",
        "events": events_array
    });

    to_object(&response)
}

/// Decode an Update (which can be either a Seed or Events).
#[wasm_bindgen]
pub fn decode_update(update: &[u8], identity: &[u8]) -> Result<JsValue, JsValue> {
    let mut buf = update;
    let update = Update::read(&mut buf)
        .map_err(|e| JsValue::from_str(&format!("Failed to decode update: {e:?}")))?;

    // Decode the identity (BLS public key)
    let identity = decode_bls_public(identity)?;

    match update {
        Update::Seed(seed) => decode_seed_internal(seed, &identity),
        Update::Events(events) => {
            // Verify the events signature and proof
            if !events.verify(&identity) {
                return Err(JsValue::from_str("Invalid events signature or proof"));
            }
            process_events(events.events_proof_ops.iter())
        }
        Update::FilteredEvents(events) => {
            // Verify the filtered events signature and proof
            if !events.verify(&identity) {
                return Err(JsValue::from_str(
                    "Invalid filtered events signature or proof",
                ));
            }
            process_events(events.events_proof_ops.iter().map(|(_, op)| op))
        }
    }
}

/// Wrap a transaction in a Submission enum for the /submit endpoint.
#[wasm_bindgen]
pub fn wrap_transaction_submission(transaction: &[u8]) -> Result<Vec<u8>, JsValue> {
    let mut buf = transaction;
    let tx = ExecutionTransaction::read(&mut buf)
        .map_err(|e| JsValue::from_str(&format!("Failed to decode transaction: {e:?}")))?;

    let submission = Submission::Transactions(vec![tx]);
    Ok(submission.encode().to_vec())
}

/// Wrap a summary in a Submission enum for the /submit endpoint.
#[wasm_bindgen]
#[cfg(feature = "testing")]
pub fn wrap_summary_submission(summary: &[u8]) -> Result<Vec<u8>, JsValue> {
    let mut buf = summary;
    let summary = Summary::read(&mut buf)
        .map_err(|e| JsValue::from_str(&format!("Failed to decode summary: {e:?}")))?;

    let submission = Submission::Summary(summary);
    Ok(submission.encode().to_vec())
}

/// Wrap a seed in a Submission enum for the /submit endpoint.
#[wasm_bindgen]
#[cfg(feature = "testing")]
pub fn wrap_seed_submission(seed: &[u8]) -> Result<Vec<u8>, JsValue> {
    let mut buf = seed;
    let seed = Seed::read(&mut buf)
        .map_err(|e| JsValue::from_str(&format!("Failed to decode seed: {e:?}")))?;

    let submission = Submission::Seed(seed);
    Ok(submission.encode().to_vec())
}
