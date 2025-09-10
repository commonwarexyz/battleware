use std::collections::{BTreeMap, HashMap, HashSet};

use battleware_types::{
    api::Update,
    execution::{
        Account, Event, Instruction, Output, Seed, Transaction, Value, LOBBY_EXPIRY, TOTAL_MOVES,
    },
    NAMESPACE,
};
use commonware_codec::Encode;
use commonware_consensus::{
    threshold_simplex::types::{seed_namespace, view_message},
    Viewable,
};
use commonware_cryptography::{
    bls12381::{
        primitives::variant::{MinSig, Variant},
        tle::{encrypt, Block, Ciphertext},
    },
    ed25519::{PrivateKey, PublicKey},
    sha256::Digest,
    Signer,
};
use commonware_storage::store::operation::Keyless;
use commonware_utils::hex;
use rand::{rngs::OsRng, Rng};
use tracing::{debug, info, warn};

const REBROADCAST_EXPIRY: u64 = 64;

#[derive(Debug, Clone)]
pub enum Status {
    Uninitialized,
    Generating,
    Lobby {
        last_broadcast: u64,
    },
    Battle {
        battle_id: Digest,
        we_are_a: bool,
        move_counts: [u8; TOTAL_MOVES],
        round_expiry: u64,
    },
}

pub struct State {
    private_key: PrivateKey,
    public_key: PublicKey,
    nonce: u64,
    state: Status,

    last_view: u64,
    network_identity: <MinSig as Variant>::Public,

    txs: BTreeMap<u64, (Transaction, u64, usize)>,
}

impl State {
    pub fn new(private_key: PrivateKey, network_identity: <MinSig as Variant>::Public) -> Self {
        let public_key = private_key.public_key();
        Self {
            private_key,
            public_key,
            nonce: 0,
            state: Status::Uninitialized,
            last_view: 0,
            network_identity,
            txs: BTreeMap::new(),
        }
    }

    pub fn apply_tx(&mut self, confirmed: &Transaction) {
        if self.nonce <= confirmed.nonce {
            warn!(self.nonce, confirmed.nonce, "Update nonce");
            self.nonce = confirmed.nonce + 1;
        }
        self.txs.retain(|stored, _| stored > &confirmed.nonce);
    }

    pub fn apply_seed(&mut self, seed: &Seed) -> (Option<u64>, Vec<Transaction>) {
        if seed.view() > self.last_view {
            self.last_view = seed.view();
        }

        // Add stale transactions
        let mut transactions = Vec::new();
        for (_, (tx, last_broadcast, attempts)) in self.txs.iter_mut() {
            if *last_broadcast == 0 {
                // We may not have seen a view yet, we shouldn't just rebroadcast
                *last_broadcast = seed.view();
            } else if *last_broadcast + REBROADCAST_EXPIRY < seed.view() {
                *last_broadcast = seed.view();
                *attempts += 1;
                warn!(account = ?self.public_key, nonce = tx.nonce, instruction = ?tx.instruction, attempts, "Rebroadcasting");
                transactions.push(tx.clone());
            }
        }

        // Enqueue new transactions
        let (missing, instruction) = match &mut self.state {
            Status::Uninitialized => {
                self.state = Status::Generating;
                (None, Some(Instruction::Generate))
            }
            Status::Generating => (None, None),
            Status::Lobby { last_broadcast, .. } => {
                // If we should be in a match, attempt to broadcast
                if seed.view() < *last_broadcast + LOBBY_EXPIRY {
                    (None, None)
                } else {
                    *last_broadcast = seed.view();
                    debug!(account = ?self.public_key, "Match");
                    (None, Some(Instruction::Match))
                }
            }
            Status::Battle { round_expiry, .. } => {
                // If we have reached round expiry, broadcast settle
                if seed.view() > *round_expiry + REBROADCAST_EXPIRY {
                    (Some(*round_expiry), None)
                } else if seed.view() == *round_expiry {
                    debug!(account = ?self.public_key, round_expiry = ?round_expiry, "Settle");
                    (None, Some(Instruction::Settle(seed.signature)))
                } else {
                    (None, None)
                }
            }
        };
        if let Some(instruction) = instruction {
            transactions.push(self.create_transaction(instruction));
        }
        (missing, transactions)
    }

    pub fn apply_event(&mut self, event: &Event) -> Option<Transaction> {
        let instruction = match event {
            Event::Generated { account, creature } if account == &self.public_key => {
                info!(account = ?self.public_key, creature = hex(&creature.encode()), "Generated");
                self.state = Status::Lobby {
                    last_broadcast: self.last_view,
                };
                debug!(account = ?self.public_key, "Match");
                Some(Instruction::Match)
            }
            Event::Matched {
                player_a,
                player_b,
                battle,
                expiry,
                player_a_creature,
                player_b_creature,
                ..
            } if player_a == &self.public_key || player_b == &self.public_key => {
                // Recognize battle state
                let we_are_a = player_a == &self.public_key;
                let opponent = if we_are_a { player_b } else { player_a };
                info!(account = ?self.public_key, ?opponent, "Matched");
                self.state = Status::Battle {
                    battle_id: *battle,
                    we_are_a,
                    move_counts: if we_are_a {
                        player_a_creature.get_move_usage_limits()
                    } else {
                        player_b_creature.get_move_usage_limits()
                    },
                    round_expiry: *expiry,
                };

                // Select move
                let play = self.pick_random_move();
                debug!(account = ?self.public_key, play, "Move");
                Some(Instruction::Move(
                    self.create_move_ciphertext(*expiry, play),
                ))
            }
            Event::Moved {
                battle,
                round,
                expiry,
                player_a_move_counts,
                player_b_move_counts,
                ..
            } => {
                let Status::Battle {
                    battle_id,
                    we_are_a,
                    move_counts,
                    round_expiry,
                    ..
                } = &mut self.state
                else {
                    return None;
                };
                if battle_id != battle {
                    return None;
                }
                info!(account = ?self.public_key, ?round, expiry = ?expiry, ?player_a_move_counts, ?player_b_move_counts, "Moved");
                *round_expiry = *expiry;
                if *we_are_a {
                    *move_counts = *player_a_move_counts;
                } else {
                    *move_counts = *player_b_move_counts;
                }

                // Select move
                let play = self.pick_random_move();
                debug!(account = ?self.public_key, play, "Move");
                Some(Instruction::Move(
                    self.create_move_ciphertext(*expiry, play),
                ))
            }
            Event::Settled {
                battle,
                player_a_new,
                player_b_new,
                ..
            } => {
                let Status::Battle {
                    battle_id,
                    we_are_a,
                    ..
                } = &self.state
                else {
                    return None;
                };
                if battle_id != battle {
                    return None;
                }
                let stats = if *we_are_a {
                    player_a_new
                } else {
                    player_b_new
                };
                info!(account = ?self.public_key, ?stats, "Settled");
                self.state = Status::Lobby {
                    last_broadcast: self.last_view,
                };
                debug!(account = ?self.public_key, "Match");
                Some(Instruction::Match)
            }
            _ => None,
        };
        instruction.map(|instruction| self.create_transaction(instruction))
    }

    fn create_transaction(&mut self, instruction: Instruction) -> Transaction {
        let nonce = self.nonce;
        self.nonce += 1;
        let tx = Transaction::sign(&self.private_key, nonce, instruction);
        self.txs.insert(nonce, (tx.clone(), self.last_view, 1));
        tx
    }

    fn pick_random_move(&self) -> u8 {
        OsRng.gen_range(1..=4)
    }

    fn create_move_ciphertext(&self, expiry: u64, move_data: u8) -> Ciphertext<MinSig> {
        let namespace = seed_namespace(NAMESPACE);
        let view_msg = view_message(expiry);

        let mut message = [0u8; 32];
        message[0] = move_data;

        encrypt::<_, MinSig>(
            &mut OsRng,
            self.network_identity,
            (Some(&namespace), &view_msg),
            &Block::new(message),
        )
    }

    fn refresh(&mut self, state: Account, battle: Option<Value>) {
        // Apply nonce
        if self.nonce < state.nonce {
            warn!(account = ?self.public_key, nonce = state.nonce, "Update nonce");
            self.nonce = state.nonce;
        }
        self.txs.retain(|stored, _| stored >= &state.nonce);

        // Apply state
        if let Some(battle) = battle {
            let Value::Battle {
                expiry,
                player_a,
                player_a_move_counts,
                player_b_move_counts,
                ..
            } = battle
            else {
                panic!("Battle should exist");
            };
            let we_are_a = player_a == self.public_key;
            self.state = Status::Battle {
                battle_id: state.battle.unwrap(),
                we_are_a,
                move_counts: if we_are_a {
                    player_a_move_counts
                } else {
                    player_b_move_counts
                },
                round_expiry: expiry,
            };
        } else {
            self.state = if state.creature.is_some() {
                // Trigger immediate match if we have a creature
                Status::Lobby { last_broadcast: 0 }
            } else {
                // If not creature, trigger generation (if already inflight, that's ok)
                Status::Uninitialized
            };
        }
    }
}

pub struct Engine {
    pub accounts: HashMap<PublicKey, State>,
    pub network_identity: <MinSig as Variant>::Public,
}

impl Engine {
    pub fn new(network_identity: <MinSig as Variant>::Public) -> Self {
        Self {
            accounts: HashMap::new(),
            network_identity,
        }
    }

    pub fn add_account(&mut self, private_key: PrivateKey) {
        self.accounts.insert(
            private_key.public_key(),
            State::new(private_key, self.network_identity),
        );
    }

    pub fn refresh_account(&mut self, account: PublicKey, state: Account, battle: Option<Value>) {
        self.accounts
            .get_mut(&account)
            .unwrap()
            .refresh(state, battle);
    }

    pub fn apply_seed(&mut self, seed: Seed) -> (Vec<u64>, Vec<Transaction>) {
        let mut requested = HashSet::new();
        let mut transactions = Vec::new();
        for (_, account) in self.accounts.iter_mut() {
            let (missing, new_transactions) = account.apply_seed(&seed);
            if let Some(missing) = missing {
                requested.insert(missing);
            }
            transactions.extend(new_transactions);
        }
        (requested.into_iter().collect(), transactions)
    }

    pub fn apply_txs(&mut self, txs: Vec<Transaction>) {
        for tx in txs {
            let Some(account) = self.accounts.get_mut(&tx.public) else {
                continue;
            };
            account.apply_tx(&tx);
        }
    }

    pub fn apply_events(&mut self, events: Vec<Event>) -> Vec<Transaction> {
        let mut transactions = Vec::new();
        for (_, account) in self.accounts.iter_mut() {
            for event in &events {
                if let Some(transaction) = account.apply_event(event) {
                    transactions.push(transaction);
                }
            }
        }
        transactions
    }

    pub fn apply(&mut self, update: Update) -> (Vec<u64>, Vec<Transaction>) {
        match update {
            Update::Seed(seed) => self.apply_seed(seed),
            Update::Events(summary) => {
                let mut events = Vec::new();
                let mut txs = Vec::new();
                for op in summary.events_proof_ops.into_iter() {
                    let Keyless::Append(output) = op else {
                        continue;
                    };
                    match output {
                        Output::Event(event) => events.push(event),
                        Output::Transaction(tx) => txs.push(tx),
                        _ => continue,
                    }
                }
                self.apply_txs(txs);
                let txs = self.apply_events(events);
                (vec![], txs)
            }
            _ => (vec![], vec![]),
        }
    }

    pub fn accounts(&self) -> usize {
        self.accounts.len()
    }

    pub fn stuck(&self, attempts: usize) -> Vec<PublicKey> {
        let mut resettable = Vec::new();
        for (_, account) in self.accounts.iter() {
            for (_, (_, _, broadcasts)) in account.txs.iter() {
                if *broadcasts > attempts {
                    resettable.push(account.public_key.clone());
                }
            }
        }
        resettable
    }

    pub fn stats(&self) -> (usize, usize, usize, usize) {
        let mut uninitialized = 0;
        let mut generating = 0;
        let mut lobby = 0;
        let mut battle = 0;
        for (_, account) in self.accounts.iter() {
            match &account.state {
                Status::Uninitialized => uninitialized += 1,
                Status::Generating => generating += 1,
                Status::Lobby { .. } => lobby += 1,
                Status::Battle { .. } => battle += 1,
            }
        }
        (uninitialized, generating, lobby, battle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use battleware_execution::mocks::{
        create_account_keypair, create_adbs, create_network_keypair, execute_block,
    };
    use battleware_types::{api::Events, execution::Key};
    use commonware_cryptography::{Hasher, Sha256};
    use commonware_macros::test_traced;
    use commonware_runtime::{deterministic, Runner};
    use std::mem::take;

    #[test_traced("INFO")]
    fn play_complete_game() {
        let runner = deterministic::Runner::seeded(2);
        runner.start(|ctx| async move {
            // Create network keypair
            let (network_secret, network_identity) = create_network_keypair();

            // Create state databases
            let (mut state, mut events) = create_adbs(&ctx).await;

            // Create bot engine
            let mut bot = Engine::new(network_identity);

            // Add two accounts that will battle each other
            let (private_a, public_a) = create_account_keypair(1);
            let (private_b, public_b) = create_account_keypair(2);
            bot.add_account(private_a.clone());
            bot.add_account(private_b.clone());

            // Run until accounts have settled games
            let mut txs = Vec::new();
            let mut seeds = HashMap::new();
            let mut view = 1;
            loop {
                // Apply block
                let this_txs = take(&mut txs);
                let (seed, summary) = execute_block(
                    &network_secret,
                    network_identity,
                    &mut state,
                    &mut events,
                    view,
                    this_txs,
                )
                .await;
                seeds.insert(view, seed.clone());

                // Get new txs
                let (requested, new_txs) = bot.apply(Update::Seed(seed));
                assert!(requested.is_empty());
                txs.extend(new_txs);
                let (requested, new_txs) = bot.apply(Update::Events(Events {
                    progress: summary.progress,
                    certificate: summary.certificate,
                    events_proof: summary.events_proof,
                    events_proof_ops: summary.events_proof_ops,
                }));
                assert!(requested.is_empty());
                txs.extend(new_txs);
                view += 1;

                // Check if accounts have settled games
                let public_a_key = Sha256::hash(&Key::Account(public_a.clone()).encode());
                let Ok(Some(Value::Account(account_a))) = state.get(&public_a_key).await else {
                    continue;
                };
                let public_b_key = Sha256::hash(&Key::Account(public_b.clone()).encode());
                let Ok(Some(Value::Account(account_b))) = state.get(&public_b_key).await else {
                    continue;
                };
                if account_a.stats.wins + account_a.stats.losses + account_a.stats.draws > 0
                    && account_b.stats.wins + account_b.stats.losses + account_b.stats.draws > 0
                {
                    break;
                }
            }
        });
    }
}
