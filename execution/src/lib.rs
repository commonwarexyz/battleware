use battleware_types::{
    execution::{
        Account, Creature, Event, Instruction, Key, Leaderboard, Outcome, Output, Transaction,
        Value, LOBBY_EXPIRY, MAX_BATTLE_ROUNDS, MAX_LOBBY_SIZE, MOVE_EXPIRY, TOTAL_MOVES,
    },
    Seed,
};
use bytes::{Buf, BufMut};
use commonware_codec::{Encode, EncodeSize, Error, Read, ReadExt, Write};
use commonware_consensus::threshold_simplex::types::View;
use commonware_cryptography::{
    bls12381::{
        primitives::variant::{MinSig, Variant},
        tle::{decrypt, Ciphertext},
    },
    ed25519::PublicKey,
    sha256::{Digest, Sha256},
    Hasher,
};
#[cfg(feature = "parallel")]
use commonware_runtime::ThreadPool;
use commonware_runtime::{Clock, Metrics, Spawner, Storage};
use commonware_storage::{adb::any::variable::Any, translator::Translator};
use rand::{rngs::StdRng, seq::SliceRandom, SeedableRng};
#[cfg(feature = "parallel")]
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    future::Future,
};

mod elo;
mod fixed;
pub mod state_transition;

#[cfg(any(test, feature = "mocks"))]
pub mod mocks;

pub type Adb<E, T> = Any<E, Digest, Value, Sha256, T>;

pub trait State {
    fn get(&self, key: &Key) -> impl Future<Output = Option<Value>>;
    fn insert(&mut self, key: Key, value: Value) -> impl Future<Output = ()>;
    fn delete(&mut self, key: &Key) -> impl Future<Output = ()>;

    fn apply(&mut self, changes: Vec<(Key, Status)>) -> impl Future<Output = ()> {
        async {
            for (key, status) in changes {
                match status {
                    Status::Update(value) => self.insert(key, value).await,
                    Status::Delete => self.delete(&key).await,
                }
            }
        }
    }
}

impl<E: Spawner + Metrics + Clock + Storage, T: Translator> State for Adb<E, T> {
    async fn get(&self, key: &Key) -> Option<Value> {
        let key = Sha256::hash(&key.encode());
        self.get(&key).await.unwrap()
    }

    async fn insert(&mut self, key: Key, value: Value) {
        let key = Sha256::hash(&key.encode());
        self.update(key, value).await.unwrap();
    }

    async fn delete(&mut self, key: &Key) {
        let key = Sha256::hash(&key.encode());
        self.delete(key).await.unwrap();
    }
}

#[derive(Default)]
pub struct Memory {
    state: HashMap<Key, Value>,
}

impl State for Memory {
    async fn get(&self, key: &Key) -> Option<Value> {
        self.state.get(key).cloned()
    }

    async fn insert(&mut self, key: Key, value: Value) {
        self.state.insert(key, value);
    }

    async fn delete(&mut self, key: &Key) {
        self.state.remove(key);
    }
}

#[derive(Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Status {
    Update(Value),
    Delete,
}

impl Write for Status {
    fn write(&self, writer: &mut impl BufMut) {
        match self {
            Status::Update(value) => {
                0u8.write(writer);
                value.write(writer);
            }
            Status::Delete => 1u8.write(writer),
        }
    }
}

impl Read for Status {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let kind = u8::read(reader)?;
        match kind {
            0 => Ok(Status::Update(Value::read(reader)?)),
            1 => Ok(Status::Delete),
            _ => Err(Error::InvalidEnum(kind)),
        }
    }
}

impl EncodeSize for Status {
    fn encode_size(&self) -> usize {
        1 + match self {
            Status::Update(value) => value.encode_size(),
            Status::Delete => 0,
        }
    }
}

pub async fn nonce<S: State>(state: &S, public: &PublicKey) -> u64 {
    let account =
        if let Some(Value::Account(account)) = state.get(&Key::Account(public.clone())).await {
            account
        } else {
            Account::default()
        };
    account.nonce
}

pub struct Noncer<'a, S: State> {
    state: &'a S,
    pending: BTreeMap<Key, Status>,
}

impl<'a, S: State> Noncer<'a, S> {
    pub fn new(state: &'a S) -> Self {
        Self {
            state,
            pending: BTreeMap::new(),
        }
    }

    pub async fn prepare(&mut self, transaction: &Transaction) -> bool {
        let mut account = if let Some(Value::Account(account)) =
            self.get(&Key::Account(transaction.public.clone())).await
        {
            account
        } else {
            Account::default()
        };

        // Ensure nonce is correct
        if account.nonce != transaction.nonce {
            return false;
        }

        // Increment nonce
        account.nonce += 1;
        self.insert(
            Key::Account(transaction.public.clone()),
            Value::Account(account),
        )
        .await;

        true
    }
}

impl<'a, S: State> State for Noncer<'a, S> {
    async fn get(&self, key: &Key) -> Option<Value> {
        match self.pending.get(key) {
            Some(Status::Update(value)) => Some(value.clone()),
            Some(Status::Delete) => None,
            None => self.state.get(key).await,
        }
    }

    async fn insert(&mut self, key: Key, value: Value) {
        self.pending.insert(key, Status::Update(value));
    }

    async fn delete(&mut self, key: &Key) {
        self.pending.insert(key.clone(), Status::Delete);
    }
}

#[derive(Hash, Eq, PartialEq)]
#[allow(clippy::large_enum_variant)]
enum Task {
    Seed(Seed),
    Decrypt(Seed, Ciphertext<MinSig>),
}

enum TaskResult {
    Seed(bool),
    Decrypt([u8; 32]),
}

pub struct Layer<'a, S: State> {
    state: &'a S,
    pending: BTreeMap<Key, Status>,

    master: <MinSig as Variant>::Public,
    namespace: Vec<u8>,

    seed: Seed,

    precomputations: HashMap<Task, TaskResult>,
}

impl<'a, S: State> Layer<'a, S> {
    pub fn new(
        state: &'a S,
        master: <MinSig as Variant>::Public,
        namespace: &[u8],
        seed: Seed,
    ) -> Self {
        let mut verified_seeds = HashSet::new();
        verified_seeds.insert(seed.clone());
        Self {
            state,
            pending: BTreeMap::new(),

            master,
            namespace: namespace.to_vec(),

            seed,

            precomputations: HashMap::new(),
        }
    }

    fn insert(&mut self, key: Key, value: Value) {
        self.pending.insert(key, Status::Update(value));
    }

    fn delete(&mut self, key: Key) {
        self.pending.insert(key, Status::Delete);
    }

    pub fn view(&self) -> View {
        self.seed.view
    }

    async fn prepare(&mut self, transaction: &Transaction) -> bool {
        // Get account
        let mut account = if let Some(Value::Account(account)) =
            self.get(&Key::Account(transaction.public.clone())).await
        {
            account
        } else {
            Account::default()
        };

        // Ensure nonce is correct
        if account.nonce != transaction.nonce {
            return false;
        }

        // Increment nonce
        account.nonce += 1;
        self.insert(
            Key::Account(transaction.public.clone()),
            Value::Account(account),
        );

        true
    }

    async fn extract(&mut self, transaction: &Transaction) -> Vec<Task> {
        match &transaction.instruction {
            Instruction::Generate => vec![],
            Instruction::Match => vec![],
            Instruction::Move(_) => vec![],
            Instruction::Settle(signature) => {
                // Get account
                let Some(Value::Account(account)) =
                    self.get(&Key::Account(transaction.public.clone())).await
                else {
                    return vec![];
                };

                // If not in a battle, not valid
                let Some(battle) = account.battle else {
                    return vec![];
                };

                // Get battle
                let Some(Value::Battle {
                    expiry,
                    player_a_pending,
                    player_b_pending,
                    ..
                }) = self.get(&Key::Battle(battle)).await
                else {
                    return vec![];
                };

                // If turn has not expired, not valid
                if expiry > self.seed.view {
                    return vec![];
                }

                // Extract seed and decryptions
                let seed = Seed::new(expiry, *signature);
                let mut ops = vec![Task::Seed(seed.clone())];
                if let Some(pending) = player_a_pending {
                    ops.push(Task::Decrypt(seed.clone(), pending));
                }
                if let Some(pending) = player_b_pending {
                    ops.push(Task::Decrypt(seed, pending));
                }

                ops
            }
        }
    }

    async fn apply(&mut self, transaction: &Transaction) -> Vec<Event> {
        // Get account
        let Some(Value::Account(mut account)) =
            self.get(&Key::Account(transaction.public.clone())).await
        else {
            panic!("Account should exist");
        };

        // Execute instruction
        let mut events = vec![];
        match &transaction.instruction {
            Instruction::Generate => {
                // If in a battle, not valid
                if account.battle.is_some() {
                    return events;
                }

                // Generate a new creature
                let creature = Creature::new(
                    transaction.public.clone(),
                    transaction.nonce,
                    self.seed.signature,
                );
                account.creature = Some(creature.clone());

                // Store update in account
                self.insert(
                    Key::Account(transaction.public.clone()),
                    Value::Account(account),
                );
                events.push(Event::Generated {
                    account: transaction.public.clone(),
                    creature,
                });
            }
            Instruction::Match => {
                // If not locked on a creature, not valid
                if account.creature.is_none() {
                    return events;
                }

                // If already in a battle, not valid
                if account.battle.is_some() {
                    return events;
                }

                // Get lobby
                let Some(Value::Lobby {
                    expiry,
                    mut players,
                }) = self.get(&Key::Lobby).await
                else {
                    // Create lobby
                    let mut players = BTreeSet::new();
                    players.insert(transaction.public.clone());
                    let lobby = Value::Lobby {
                        expiry: self
                            .seed
                            .view
                            .checked_add(LOBBY_EXPIRY)
                            .expect("view overflow"),
                        players,
                    };
                    self.insert(Key::Lobby, lobby);
                    return events;
                };

                // Add to lobby.
                //
                // If already in lobby, that's fine (may trigger matching).
                players.insert(transaction.public.clone());

                // If lobby has expired or is full, create matches
                if expiry < self.seed.view || players.len() >= MAX_LOBBY_SIZE {
                    // Get players
                    let mut players = players.iter().collect::<Vec<_>>();

                    // Randomly select trainers
                    let seed = Sha256::hash(self.seed.encode().as_ref());
                    let mut rng = StdRng::from_seed(seed.as_ref().try_into().unwrap());
                    players.shuffle(&mut rng);

                    // Create match
                    let iter = players.chunks_exact(2).map(|chunk| (chunk[0], chunk[1]));
                    let mut hasher = Sha256::new();
                    for (player_a, player_b) in iter {
                        // Compute battle key
                        hasher.update(self.seed.encode().as_ref());
                        hasher.update(player_a.as_ref());
                        hasher.update(player_b.as_ref());
                        let key = hasher.finalize();

                        // Add battle to player A
                        let Some(Value::Account(mut account_a)) =
                            self.get(&Key::Account(player_a.clone())).await
                        else {
                            panic!("Player A should have an account");
                        };
                        let player_a_creature = account_a.creature.as_ref().unwrap().clone();
                        let player_a_stats = account_a.stats.clone();
                        let player_a_health = player_a_creature.health();
                        account_a.battle = Some(key);
                        self.insert(Key::Account(player_a.clone()), Value::Account(account_a));

                        // Add battle to player B
                        let Some(Value::Account(mut account_b)) =
                            self.get(&Key::Account(player_b.clone())).await
                        else {
                            panic!("Player B should have an account");
                        };
                        let player_b_creature = account_b.creature.as_ref().unwrap().clone();
                        let player_b_stats = account_b.stats.clone();
                        let player_b_health = player_b_creature.health();
                        account_b.battle = Some(key);
                        self.insert(Key::Account(player_b.clone()), Value::Account(account_b));

                        // Add battle to state
                        let expiry = self
                            .seed
                            .view
                            .checked_add(MOVE_EXPIRY)
                            .expect("view overflow");
                        let value = Value::Battle {
                            expiry,
                            round: 0,
                            player_a: (*player_a).clone(),
                            player_a_max_health: player_a_health,
                            player_a_health,
                            player_a_pending: None,
                            player_a_move_counts: [0; TOTAL_MOVES],
                            player_b: (*player_b).clone(),
                            player_b_max_health: player_b_health,
                            player_b_health,
                            player_b_pending: None,
                            player_b_move_counts: [0; TOTAL_MOVES],
                        };
                        self.insert(Key::Battle(key), value);
                        events.push(Event::Matched {
                            battle: key,
                            expiry,
                            player_a: (*player_a).clone(),
                            player_a_creature,
                            player_a_stats,
                            player_b: (*player_b).clone(),
                            player_b_creature,
                            player_b_stats,
                        });
                    }

                    // If there are any remaining trainers, add them to the lobby for the next matching
                    let remainder = players.chunks_exact(2).remainder();
                    let mut new_players = BTreeSet::new();
                    if !remainder.is_empty() {
                        for player in remainder {
                            new_players.insert((*player).clone());
                        }
                    }

                    // Start lobby as soon as possible
                    let lobby = Value::Lobby {
                        expiry: self
                            .seed
                            .view
                            .checked_add(LOBBY_EXPIRY)
                            .expect("view overflow"),
                        players: new_players,
                    };
                    self.insert(Key::Lobby, lobby);
                } else {
                    // Update existing lobby with new trainer
                    let lobby = Value::Lobby { expiry, players };
                    self.insert(Key::Lobby, lobby);
                }
            }
            Instruction::Move(encrypted_move) => {
                // If not in a battle, not valid
                let Some(battle) = account.battle else {
                    return events;
                };

                // If the player has not yet moved, store the move
                let Some(Value::Battle {
                    expiry,
                    round,
                    player_a,
                    player_a_max_health,
                    player_a_health,
                    mut player_a_pending,
                    player_a_move_counts,
                    player_b,
                    player_b_max_health,
                    player_b_health,
                    mut player_b_pending,
                    player_b_move_counts,
                }) = self.get(&Key::Battle(battle)).await
                else {
                    panic!("Battle should exist");
                };

                // If the turn has expired, not valid
                if expiry < self.seed.view {
                    return events;
                }

                // Store the move (ok to update encrypted value)
                if player_a == transaction.public && player_a_pending.is_none() {
                    player_a_pending = Some(encrypted_move.clone());
                } else if player_b == transaction.public && player_b_pending.is_none() {
                    player_b_pending = Some(encrypted_move.clone());
                } else {
                    return events;
                }

                // Emit event
                events.push(Event::Locked {
                    battle,
                    round,
                    locker: transaction.public.clone(),
                    observer: if player_a == transaction.public {
                        player_b.clone()
                    } else {
                        player_a.clone()
                    },
                    ciphertext: encrypted_move.clone(),
                });

                // Store update in battle
                let value = Value::Battle {
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
                };
                self.insert(Key::Battle(battle), value);
            }
            Instruction::Settle(signature) => {
                // If not in a battle, return false
                let Some(battle) = account.battle else {
                    return events; // Not in battle, no-op
                };

                // Get battle
                let Some(Value::Battle {
                    expiry,
                    mut round,
                    player_a,
                    player_a_max_health,
                    mut player_a_health,
                    player_a_pending,
                    mut player_a_move_counts,
                    player_b,
                    player_b_max_health,
                    mut player_b_health,
                    player_b_pending,
                    mut player_b_move_counts,
                }) = self.get(&Key::Battle(battle)).await
                else {
                    panic!("Battle should exist");
                };

                // If turn has not expired, not valid
                if expiry > self.seed.view {
                    return events;
                }

                // If the signature is not valid, return false
                let seed = Seed::new(expiry, *signature);
                if seed != self.seed {
                    match self.precomputations.get(&Task::Seed(seed.clone())) {
                        Some(TaskResult::Seed(result)) => {
                            if !result {
                                return events;
                            }
                        }
                        None => {
                            if !seed.verify(&self.namespace, &self.master) {
                                return events; // Invalid signature, no-op
                            }
                        }
                        _ => unreachable!(),
                    }
                }

                // Decrypt player A move
                let mut player_a_move = if let Some(player_a_pending) = player_a_pending {
                    // Use the signature that was verified for the battle expiry
                    match self
                        .precomputations
                        .get(&Task::Decrypt(seed.clone(), player_a_pending.clone()))
                    {
                        Some(TaskResult::Decrypt(result)) => result[0],
                        None => {
                            let raw: [u8; 32] = decrypt::<MinSig>(signature, &player_a_pending)
                                .map(|block| block.as_ref().try_into().unwrap())
                                .unwrap_or_else(|| [0; 32]);
                            raw[0]
                        }
                        _ => unreachable!(),
                    }
                } else {
                    // If no move, return 0
                    0
                };
                if player_a_move >= TOTAL_MOVES as u8 {
                    player_a_move = 0;
                }

                // Get player A creature
                let Some(Value::Account(account)) = self.get(&Key::Account(player_a.clone())).await
                else {
                    panic!("Player A should have an account");
                };
                let player_a_creature = account.creature.as_ref().unwrap();

                // Check move usage limit for player A
                let player_a_limits = player_a_creature.get_move_usage_limits();
                if player_a_move_counts[player_a_move as usize]
                    >= player_a_limits[player_a_move as usize]
                {
                    player_a_move = 0;
                }

                // Apply move
                let (player_a_defense, player_a_power) =
                    player_a_creature.action(player_a_move, self.seed.signature);

                // Decrypt player B move
                let mut player_b_move = if let Some(player_b_pending) = player_b_pending {
                    // If can't decrypt, return 0
                    match self
                        .precomputations
                        .get(&Task::Decrypt(seed, player_b_pending.clone()))
                    {
                        Some(TaskResult::Decrypt(result)) => result[0],
                        None => {
                            let raw: [u8; 32] = decrypt::<MinSig>(signature, &player_b_pending)
                                .map(|block| block.as_ref().try_into().unwrap())
                                .unwrap_or_else(|| [0; 32]);
                            raw[0]
                        }
                        _ => unreachable!(),
                    }
                } else {
                    // If no move, return 0
                    0
                };
                if player_b_move >= TOTAL_MOVES as u8 {
                    player_b_move = 0;
                }

                // Get player B creature
                let Some(Value::Account(account)) = self.get(&Key::Account(player_b.clone())).await
                else {
                    panic!("Player B should have an account");
                };
                let player_b_creature = account.creature.as_ref().unwrap();

                // Check move usage limit for player B
                let player_b_limits = player_b_creature.get_move_usage_limits();
                if player_b_move_counts[player_b_move as usize]
                    >= player_b_limits[player_b_move as usize]
                {
                    player_b_move = 0;
                }

                // Apply move
                let (player_b_defense, player_b_power) =
                    player_b_creature.action(player_b_move, self.seed.signature);

                // Apply impact
                // Track effective health (can go negative) for overkill calculation
                let mut player_a_effective_health = player_a_health as i16;
                let mut player_b_effective_health = player_b_health as i16;

                if player_a_defense && !player_b_defense {
                    // Player A restores health, then takes damage
                    player_a_health = player_a_health
                        .saturating_add(player_a_power)
                        .min(player_a_max_health);
                    player_a_effective_health = (player_a_health as i16) - (player_b_power as i16);
                    player_a_health = player_a_health.saturating_sub(player_b_power);
                } else if !player_a_defense && player_b_defense {
                    // Player B restores health, then takes damage
                    player_b_health = player_b_health
                        .saturating_add(player_b_power)
                        .min(player_b_max_health);
                    player_b_effective_health = (player_b_health as i16) - (player_a_power as i16);
                    player_b_health = player_b_health.saturating_sub(player_a_power);
                } else if player_a_defense && player_b_defense {
                    // Player A and B restore health
                    player_a_health = player_a_health
                        .saturating_add(player_a_power)
                        .min(player_a_max_health);
                    player_b_health = player_b_health
                        .saturating_add(player_b_power)
                        .min(player_b_max_health);
                    player_a_effective_health = player_a_health as i16;
                    player_b_effective_health = player_b_health as i16;
                } else {
                    // Player A and B take damage
                    player_a_effective_health = (player_a_health as i16) - (player_b_power as i16);
                    player_b_effective_health = (player_b_health as i16) - (player_a_power as i16);
                    player_a_health = player_a_health.saturating_sub(player_b_power);
                    player_b_health = player_b_health.saturating_sub(player_a_power);
                }
                // Increment move counts
                //
                // We should timeout before we ever overflow but might as well be defensive.
                player_a_move_counts[player_a_move as usize] =
                    player_a_move_counts[player_a_move as usize].saturating_add(1);
                player_b_move_counts[player_b_move as usize] =
                    player_b_move_counts[player_b_move as usize].saturating_add(1);

                // Increment round counter
                round += 1;

                // Compute next expiry
                let next_expiry = self
                    .seed
                    .view
                    .checked_add(MOVE_EXPIRY)
                    .expect("view overflow");
                events.push(Event::Moved {
                    battle,
                    round,
                    expiry: next_expiry,
                    player_a: player_a.clone(),
                    player_a_health,
                    player_a_move,
                    player_a_move_counts,
                    player_a_power,
                    player_b: player_b.clone(),
                    player_b_health,
                    player_b_move,
                    player_b_move_counts,
                    player_b_power,
                });

                // Determine whether the battle is over
                if player_a_health == 0 && player_b_health > 0 {
                    // Player B wins
                    self.delete(Key::Battle(battle));

                    // Get original stats
                    let Some(Value::Account(mut account_a)) =
                        self.get(&Key::Account(player_a.clone())).await
                    else {
                        panic!("Player A should have an account");
                    };
                    let old_account_a_stats = account_a.stats.clone();
                    let Some(Value::Account(mut account_b)) =
                        self.get(&Key::Account(player_b.clone())).await
                    else {
                        panic!("Player B should have an account");
                    };
                    let old_account_b_stats = account_b.stats.clone();

                    // Update losses
                    account_a.stats.losses = account_a.stats.losses.saturating_add(1);
                    account_a.battle = None;

                    // Update wins
                    account_b.stats.wins = account_b.stats.wins.saturating_add(1);
                    account_b.battle = None;

                    // Update ELO scores (player A has 0 or negative health, player B has some)
                    let max_health_a = account_a.creature.as_ref().unwrap().health();
                    let max_health_b = account_b.creature.as_ref().unwrap().health();
                    let (new_elo_a, new_elo_b) = elo::update(
                        account_a.stats.elo,
                        player_a_effective_health,
                        max_health_a,
                        account_b.stats.elo,
                        player_b_effective_health,
                        max_health_b,
                    );
                    account_a.stats.elo = new_elo_a;
                    let new_account_a_stats = account_a.stats.clone();
                    account_b.stats.elo = new_elo_b;
                    let new_account_b_stats = account_b.stats.clone();
                    self.insert(Key::Account(player_a.clone()), Value::Account(account_a));
                    self.insert(Key::Account(player_b.clone()), Value::Account(account_b));

                    // Update leaderboard
                    let mut leaderboard = match self.get(&Key::Leaderboard).await {
                        Some(Value::Leaderboard(lb)) => lb,
                        _ => Leaderboard::default(),
                    };
                    leaderboard.update(player_a.clone(), new_account_a_stats.clone());
                    leaderboard.update(player_b.clone(), new_account_b_stats.clone());
                    self.insert(Key::Leaderboard, Value::Leaderboard(leaderboard.clone()));

                    // Add event
                    events.push(Event::Settled {
                        battle,
                        round,
                        player_a,
                        player_a_old: old_account_a_stats,
                        player_a_new: new_account_a_stats,
                        player_b,
                        player_b_old: old_account_b_stats,
                        player_b_new: new_account_b_stats,
                        outcome: Outcome::PlayerB,
                        leaderboard,
                    });
                } else if player_b_health == 0 && player_a_health > 0 {
                    // Player A wins
                    self.delete(Key::Battle(battle));

                    // Get original stats
                    let Some(Value::Account(mut account_a)) =
                        self.get(&Key::Account(player_a.clone())).await
                    else {
                        panic!("Player A should have an account");
                    };
                    let old_account_a_stats = account_a.stats.clone();
                    let Some(Value::Account(mut account_b)) =
                        self.get(&Key::Account(player_b.clone())).await
                    else {
                        panic!("Player B should have an account");
                    };
                    let old_account_b_stats = account_b.stats.clone();

                    // Update losses
                    account_b.stats.losses = account_b.stats.losses.saturating_add(1);
                    account_b.battle = None;

                    // Update wins
                    account_a.stats.wins = account_a.stats.wins.saturating_add(1);
                    account_a.battle = None;

                    // Update ELO scores (player B has 0 or negative health, player A has some)
                    let max_health_a = account_a.creature.as_ref().unwrap().health();
                    let max_health_b = account_b.creature.as_ref().unwrap().health();
                    let (new_elo_a, new_elo_b) = elo::update(
                        account_a.stats.elo,
                        player_a_effective_health,
                        max_health_a,
                        account_b.stats.elo,
                        player_b_effective_health,
                        max_health_b,
                    );
                    account_a.stats.elo = new_elo_a;
                    let new_account_a_stats = account_a.stats.clone();
                    account_b.stats.elo = new_elo_b;
                    let new_account_b_stats = account_b.stats.clone();
                    self.insert(Key::Account(player_a.clone()), Value::Account(account_a));
                    self.insert(Key::Account(player_b.clone()), Value::Account(account_b));

                    // Update leaderboard
                    let mut leaderboard = match self.get(&Key::Leaderboard).await {
                        Some(Value::Leaderboard(lb)) => lb,
                        _ => Leaderboard::default(),
                    };
                    leaderboard.update(player_a.clone(), new_account_a_stats.clone());
                    leaderboard.update(player_b.clone(), new_account_b_stats.clone());
                    self.insert(Key::Leaderboard, Value::Leaderboard(leaderboard.clone()));

                    // Add event
                    events.push(Event::Settled {
                        battle,
                        round,
                        player_a,
                        player_a_old: old_account_a_stats,
                        player_a_new: new_account_a_stats,
                        player_b,
                        player_b_old: old_account_b_stats,
                        player_b_new: new_account_b_stats,
                        outcome: Outcome::PlayerA,
                        leaderboard,
                    });
                } else if round >= MAX_BATTLE_ROUNDS
                    || (player_a_health == 0 && player_b_health == 0)
                {
                    // Draw
                    self.delete(Key::Battle(battle));

                    // Get original stats
                    let Some(Value::Account(mut account_a)) =
                        self.get(&Key::Account(player_a.clone())).await
                    else {
                        panic!("Player A should have an account");
                    };
                    let old_account_a_stats = account_a.stats.clone();
                    let Some(Value::Account(mut account_b)) =
                        self.get(&Key::Account(player_b.clone())).await
                    else {
                        panic!("Player B should have an account");
                    };
                    let old_account_b_stats = account_b.stats.clone();

                    // Update draws
                    account_a.stats.draws = account_a.stats.draws.saturating_add(1);
                    account_a.battle = None;

                    // Update draws
                    account_b.stats.draws = account_b.stats.draws.saturating_add(1);
                    account_b.battle = None;

                    // Update ELO scores based on effective health (including overkill)
                    let max_health_a = account_a.creature.as_ref().unwrap().health();
                    let max_health_b = account_b.creature.as_ref().unwrap().health();
                    let (new_elo_a, new_elo_b) = elo::update(
                        account_a.stats.elo,
                        player_a_effective_health,
                        max_health_a,
                        account_b.stats.elo,
                        player_b_effective_health,
                        max_health_b,
                    );
                    account_a.stats.elo = new_elo_a;
                    let new_account_a_stats = account_a.stats.clone();
                    account_b.stats.elo = new_elo_b;
                    let new_account_b_stats = account_b.stats.clone();
                    self.insert(Key::Account(player_a.clone()), Value::Account(account_a));
                    self.insert(Key::Account(player_b.clone()), Value::Account(account_b));

                    // Update leaderboard
                    let mut leaderboard = match self.get(&Key::Leaderboard).await {
                        Some(Value::Leaderboard(lb)) => lb,
                        _ => Leaderboard::default(),
                    };
                    leaderboard.update(player_a.clone(), new_account_a_stats.clone());
                    leaderboard.update(player_b.clone(), new_account_b_stats.clone());
                    self.insert(Key::Leaderboard, Value::Leaderboard(leaderboard.clone()));

                    // Add event
                    events.push(Event::Settled {
                        battle,
                        round,
                        player_a,
                        player_a_old: old_account_a_stats,
                        player_a_new: new_account_a_stats,
                        player_b,
                        player_b_old: old_account_b_stats,
                        player_b_new: new_account_b_stats,
                        outcome: Outcome::Draw,
                        leaderboard,
                    });
                } else {
                    // Battle continues
                    self.insert(
                        Key::Battle(battle),
                        Value::Battle {
                            expiry: next_expiry,
                            round,
                            player_a,
                            player_a_max_health,
                            player_a_health,
                            player_a_pending: None,
                            player_a_move_counts,
                            player_b,
                            player_b_max_health,
                            player_b_health,
                            player_b_pending: None,
                            player_b_move_counts,
                        },
                    );
                }
            }
        }

        events
    }

    pub async fn execute(
        &mut self,
        #[cfg(feature = "parallel")] pool: ThreadPool,
        transactions: Vec<Transaction>,
    ) -> (Vec<Output>, BTreeMap<PublicKey, u64>) {
        // Iterate over all transactions with valid nonces (applying in the process) and extract expensive cryptographic operations
        let mut processed_nonces = BTreeMap::new();
        let mut seed_ops = HashSet::new();
        let mut decrypt_ops = HashSet::new();
        let mut valid_transactions = Vec::new();
        for tx in transactions {
            // Must be applied in order to ensure blocks with multiple transactions from same
            // account are handled properly.
            if !self.prepare(&tx).await {
                continue;
            }

            // Track the next nonce for this public key
            processed_nonces.insert(tx.public.clone(), tx.nonce.saturating_add(1));

            // Extract operations
            let ops = self.extract(&tx).await;
            for op in ops {
                match op {
                    Task::Seed(_) => seed_ops.insert(op),
                    Task::Decrypt(_, _) => decrypt_ops.insert(op),
                };
            }
            valid_transactions.push(tx);
        }

        // Execute operations
        macro_rules! process_ops {
            ($iter:ident) => {{
                // Verify all seeds
                let mut results: HashMap<Task, TaskResult> = seed_ops
                    .$iter()
                    .map(|op| match op {
                        Task::Seed(ref seed) => {
                            if self.seed == *seed {
                                return (op, TaskResult::Seed(true));
                            }
                            let result = seed.verify(&self.namespace, &self.master);
                            (op, TaskResult::Seed(result))
                        }
                        _ => unreachable!(),
                    })
                    .collect();

                // Only decrypt for valid signatures
                let decrypt_results: HashMap<Task, TaskResult> = decrypt_ops
                    .$iter()
                    .flat_map(|op| match op {
                        Task::Decrypt(ref seed, ref ciphertext) => {
                            // If seed is invalid, skip decryption (decryption won't be needed)
                            if !matches!(
                                results.get(&Task::Seed(seed.clone())).unwrap(), // we should never be missing a seed
                                TaskResult::Seed(true)
                            ) {
                                return None;
                            }

                            // If seed is valid, decrypt
                            let result = decrypt::<MinSig>(&seed.signature, ciphertext)
                                .map(|block| block.as_ref().try_into().unwrap())
                                .unwrap_or_else(|| [0; 32]);
                            Some((op, TaskResult::Decrypt(result)))
                        }
                        _ => unreachable!(),
                    })
                    .collect();

                // Merge results
                results.extend(decrypt_results);
                results
            }};
        }
        #[cfg(feature = "parallel")]
        let precomputations = pool.install(|| process_ops!(into_par_iter));
        #[cfg(not(feature = "parallel"))]
        let precomputations = process_ops!(into_iter);

        // Store precomputations
        self.precomputations = precomputations;

        // Apply transactions (using cached operation results)
        let mut events = Vec::new();
        for tx in valid_transactions {
            events.extend(self.apply(&tx).await.into_iter().map(Output::Event));
            events.push(Output::Transaction(tx));
        }

        (events, processed_nonces)
    }

    pub fn commit(self) -> Vec<(Key, Status)> {
        self.pending.into_iter().collect()
    }
}

impl<'a, S: State> State for Layer<'a, S> {
    async fn get(&self, key: &Key) -> Option<Value> {
        match self.pending.get(key) {
            Some(Status::Update(value)) => Some(value.clone()),
            Some(Status::Delete) => None,
            None => self.state.get(key).await,
        }
    }

    async fn insert(&mut self, key: Key, value: Value) {
        self.pending.insert(key, Status::Update(value));
    }

    async fn delete(&mut self, key: &Key) {
        self.pending.insert(key.clone(), Status::Delete);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use commonware_cryptography::bls12381::tle::{encrypt, Block, Ciphertext};
    use commonware_cryptography::{
        bls12381::primitives::{ops, variant::MinSig},
        ed25519, PrivateKeyExt, Signer,
    };
    use commonware_runtime::deterministic::Runner;
    use commonware_runtime::Runner as _;
    use rand::{rngs::StdRng, SeedableRng};
    use std::collections::HashMap;

    const TEST_NAMESPACE: &[u8] = b"test-namespace";

    struct MockState {
        data: HashMap<Key, Value>,
    }

    impl MockState {
        fn new() -> Self {
            Self {
                data: HashMap::new(),
            }
        }
    }

    impl State for MockState {
        async fn get(&self, key: &Key) -> Option<Value> {
            self.data.get(key).cloned()
        }

        async fn insert(&mut self, key: Key, value: Value) {
            self.data.insert(key, value);
        }

        async fn delete(&mut self, key: &Key) {
            self.data.remove(key);
        }
    }

    // Master keypair for all timelock operations in tests
    fn create_network_keypair() -> (
        commonware_cryptography::bls12381::primitives::group::Private,
        <MinSig as commonware_cryptography::bls12381::primitives::variant::Variant>::Public,
    ) {
        let mut rng = StdRng::seed_from_u64(0);
        ops::keypair::<_, MinSig>(&mut rng)
    }

    fn create_seed(
        network_secret: &commonware_cryptography::bls12381::primitives::group::Private,
        view: u64,
    ) -> Seed {
        use commonware_consensus::threshold_simplex::types::{seed_namespace, view_message};
        let seed_namespace = seed_namespace(TEST_NAMESPACE);
        let message = view_message(view);
        Seed::new(
            view,
            ops::sign_message::<MinSig>(network_secret, Some(&seed_namespace), &message),
        )
    }

    fn create_test_move_ciphertext(
        master_public: <MinSig as commonware_cryptography::bls12381::primitives::variant::Variant>::Public,
        next_expiry: u64,
        move_data: u8,
    ) -> Ciphertext<MinSig> {
        use commonware_consensus::threshold_simplex::types::{seed_namespace, view_message};

        // Target needs to match what the seed signature signs over
        let seed_namespace = seed_namespace(TEST_NAMESPACE);
        let view_msg = view_message(next_expiry);

        // Create a 32-byte move message with the move data
        let mut message = [0u8; 32];
        message[0] = move_data; // First byte is the actual move

        let mut rng = StdRng::seed_from_u64(42); // Different seed for encryption randomness
        encrypt::<_, MinSig>(
            &mut rng,
            master_public,
            (Some(&seed_namespace), &view_msg),
            &Block::new(message),
        )
    }

    #[allow(dead_code)]
    fn decrypt_test_move(
        network_secret: &commonware_cryptography::bls12381::primitives::group::Private,
        next_expiry: u64,
        ciphertext: &Ciphertext<MinSig>,
    ) -> Option<u8> {
        use commonware_cryptography::bls12381::tle::decrypt;

        let seed = create_seed(network_secret, next_expiry);
        decrypt::<MinSig>(&seed.signature, ciphertext).map(|block| block.as_ref()[0])
    }

    fn create_test_actor(seed: u64) -> (ed25519::PrivateKey, ed25519::PublicKey) {
        let private = ed25519::PrivateKey::from_seed(seed);
        let public = private.public_key();
        (private, public)
    }

    #[test]
    fn test_invalid_nonce_dropped() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);

            let (signer, _) = create_test_actor(1);

            // Try to prepare with wrong nonce (should fail)
            let tx = Transaction::sign(&signer, 1, Instruction::Generate);
            assert!(!layer.prepare(&tx).await);

            // Prepare with correct nonce
            let tx = Transaction::sign(&signer, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);

            // The nonce should now be 1 in the pending state
            // Try to use old nonce again in the same layer (should fail)
            let tx = Transaction::sign(&signer, 0, Instruction::Generate);
            assert!(!layer.prepare(&tx).await);

            // Should succeed with new nonce
            let tx = Transaction::sign(&signer, 1, Instruction::Generate);
            assert!(layer.prepare(&tx).await);

            // Consume the layer at the end of the test
            let _ = layer.commit();
        });
    }

    #[test]
    fn test_must_have_creature_before_battle() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);

            let (signer, _) = create_test_actor(1);

            // Prepare account
            let tx = Transaction::sign(&signer, 0, Instruction::Match);
            assert!(layer.prepare(&tx).await);

            // Try to match without creature
            let events = layer.apply(&tx).await;
            assert!(events.is_empty()); // Should produce no events

            // Generate creature first
            let tx = Transaction::sign(&signer, 1, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert_eq!(events.len(), 1);
            assert!(matches!(events[0], Event::Generated { .. }));

            // Now matching should work (adds to lobby)
            // Continue with the same layer - nonce is already incremented from prepare
            let tx = Transaction::sign(&signer, 2, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert!(events.is_empty()); // Just adds to lobby, no match yet

            // Consume the layer at the end of the test
            let _ = layer.commit();
        });
    }

    #[test]
    fn test_cannot_enter_multiple_concurrent_battles() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);

            let (signer_a, _) = create_test_actor(1);
            let (signer_b, _) = create_test_actor(2);

            // Prepare accounts and generate creatures
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Match both players - first player creates lobby
            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Commit and create new layer with advanced view to expire the lobby
            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);

            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert_eq!(events.len(), 1);
            assert!(matches!(events[0], Event::Matched { .. }));

            // Try to match again while in battle (should fail)
            let tx = Transaction::sign(&signer_a, 2, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert!(events.is_empty()); // Should not be able to match again

            // Consume the layer at the end of the test
            let _ = layer.commit();
        });
    }

    #[test]
    fn test_can_only_swap_creatures_when_not_in_battle() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);

            let (signer_a, _) = create_test_actor(1);
            let (signer_b, _) = create_test_actor(2);

            // Prepare and generate creature for actor A
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Can generate new creature when not in battle
            let tx = Transaction::sign(&signer_a, 1, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert_eq!(events.len(), 1);
            assert!(matches!(events[0], Event::Generated { .. }));

            // Prepare actor B and match them
            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let tx = Transaction::sign(&signer_a, 2, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Commit and create new layer with advanced view to expire the lobby
            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);

            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert_eq!(events.len(), 1);
            assert!(matches!(events[0], Event::Matched { .. }));

            // Try to generate new creature while in battle (should fail)
            let tx = Transaction::sign(&signer_a, 3, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert!(events.is_empty());

            // Consume the layer at the end of the test
            let _ = layer.commit();
        });
    }

    #[test]
    fn test_cannot_send_multiple_moves_in_single_round() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);

            let (signer_a, _) = create_test_actor(1);
            let (signer_b, _) = create_test_actor(2);

            // Setup battle
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Commit and create new layer with advanced view to expire the lobby
            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);

            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert_eq!(events.len(), 1);

            // Send first move - use the battle expiry as the timelock target
            let battle_expiry = layer
                .view()
                .checked_add(MOVE_EXPIRY)
                .expect("view overflow");
            let move1 = create_test_move_ciphertext(master_public, battle_expiry, 1);
            let tx = Transaction::sign(&signer_a, 2, Instruction::Move(move1.clone()));
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Try to send second move in same round (should fail)
            let move2 = create_test_move_ciphertext(master_public, battle_expiry, 2);
            let tx = Transaction::sign(&signer_a, 3, Instruction::Move(move2));
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert!(events.is_empty());

            // Consume the layer at the end of the test
            let _ = layer.commit();
        });
    }

    #[test]
    fn test_one_player_can_win_if_other_doesnt_play() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);

            let (signer_a, _) = create_test_actor(1);
            let (signer_b, _) = create_test_actor(2);

            // Setup battle
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Commit and create new layer with advanced view to expire the lobby
            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);

            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert_eq!(events.len(), 1);

            // Get the battle expiry
            let battle_expiry = layer
                .view()
                .checked_add(MOVE_EXPIRY)
                .expect("view overflow");

            // Only player A sends a move (offensive move)
            let move_a = create_test_move_ciphertext(master_public, battle_expiry, 2);
            let tx = Transaction::sign(&signer_a, 2, Instruction::Move(move_a));
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Commit and create new layer with advanced view past expiry
            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, battle_expiry + 1);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);

            let signature = create_seed(&network_secret, battle_expiry);
            let tx = Transaction::sign(&signer_a, 3, Instruction::Settle(signature.signature));
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;

            // Should have a move event (player B defaults to no move)
            assert!(events.iter().any(|e| matches!(e, Event::Moved { .. })));

            // Continue playing until someone wins
            // Player B still doesn't play, so eventually they should lose

            // Consume the layer at the end of the test
            let _ = layer.commit();
        });
    }

    #[test]
    fn test_scores_update_correctly_on_game_over() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);

            let (signer_a, actor_a) = create_test_actor(1);
            let (signer_b, actor_b) = create_test_actor(2);

            // Setup battle
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Get initial account states
            let account_a_initial = layer.get(&Key::Account(actor_a.clone())).await.unwrap();
            let account_b_initial = layer.get(&Key::Account(actor_b.clone())).await.unwrap();

            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Commit and create new layer with advanced view to expire the lobby
            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);

            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert_eq!(events.len(), 1);

            let _battle_digest = if let Event::Matched { battle, .. } = &events[0] {
                *battle
            } else {
                panic!("Expected matched event");
            };

            // Play until someone loses all health
            let mut round = 0;
            loop {
                // Get current battle expiry
                let battle_expiry = layer
                    .view()
                    .checked_add(MOVE_EXPIRY)
                    .expect("view overflow");

                // Both players make offensive moves
                let move_a = create_test_move_ciphertext(master_public, battle_expiry, 3);
                let move_b = create_test_move_ciphertext(master_public, battle_expiry, 3);

                let tx = Transaction::sign(&signer_a, 2 + round * 2, Instruction::Move(move_a));
                assert!(layer.prepare(&tx).await);
                layer.apply(&tx).await;

                let tx = Transaction::sign(&signer_b, 2 + round, Instruction::Move(move_b));
                assert!(layer.prepare(&tx).await);
                layer.apply(&tx).await;

                // Commit and create new layer with advanced view past expiry
                let changes = layer.commit();
                state.apply(changes).await;
                let new_seed = create_seed(&network_secret, battle_expiry + 1);
                layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);

                let signature = create_seed(&network_secret, battle_expiry);
                let tx = Transaction::sign(
                    &signer_a,
                    3 + round * 2,
                    Instruction::Settle(signature.signature),
                );
                assert!(layer.prepare(&tx).await);
                let events = layer.apply(&tx).await;

                // Check if game ended
                if let Some(settled_event) =
                    events.iter().find(|e| matches!(e, Event::Settled { .. }))
                {
                    // Verify scores updated
                    let account_a_final = layer.get(&Key::Account(actor_a.clone())).await.unwrap();
                    let account_b_final = layer.get(&Key::Account(actor_b.clone())).await.unwrap();

                    if let (
                        Value::Account(mut acc_a_init),
                        Value::Account(mut acc_a_final),
                        Value::Account(mut acc_b_init),
                        Value::Account(mut acc_b_final),
                    ) = (
                        account_a_initial.clone(),
                        account_a_final,
                        account_b_initial.clone(),
                        account_b_final,
                    ) {
                        // Check win/loss/draw counters
                        assert!(
                            acc_a_final.stats.wins > acc_a_init.stats.wins
                                || acc_a_final.stats.losses > acc_a_init.stats.losses
                                || acc_a_final.stats.draws > acc_a_init.stats.draws
                        );
                        assert!(
                            acc_b_final.stats.wins > acc_b_init.stats.wins
                                || acc_b_final.stats.losses > acc_b_init.stats.losses
                                || acc_b_final.stats.draws > acc_b_init.stats.draws
                        );

                        // Check ELO changed
                        assert!(
                            acc_a_final.stats.elo != acc_a_init.stats.elo
                                || acc_b_final.stats.elo != acc_b_init.stats.elo
                        );

                        // Battle should be cleared
                        assert!(acc_a_final.battle.is_none());
                        assert!(acc_b_final.battle.is_none());

                        // Verify old/new Elo scores in Settled event are properly populated
                        if let Event::Settled {
                            player_a,
                            player_a_old,
                            player_a_new,
                            player_b_old,
                            player_b_new,
                            ..
                        } = settled_event
                        {
                            // Swap results if player A is not the signer
                            if player_a != &signer_a.public_key() {
                                let acc_temp_init = acc_a_init;
                                let acc_temp_final = acc_a_final;
                                acc_a_init = acc_b_init;
                                acc_a_final = acc_b_final;
                                acc_b_init = acc_temp_init;
                                acc_b_final = acc_temp_final;
                            }

                            // Verify old Elo scores match initial account states
                            assert_eq!(
                                player_a_old.elo, acc_a_init.stats.elo,
                                "Player A old Elo should match initial Elo"
                            );
                            assert_eq!(
                                player_b_old.elo, acc_b_init.stats.elo,
                                "Player B old Elo should match initial Elo"
                            );

                            // Verify new Elo scores match final account states
                            assert_eq!(
                                player_a_new.elo, acc_a_final.stats.elo,
                                "Player A new Elo should match final Elo"
                            );
                            assert_eq!(
                                player_b_new.elo, acc_b_final.stats.elo,
                                "Player B new Elo should match final Elo"
                            );

                            // Verify Elo scores actually changed
                            assert_ne!(
                                player_a_old.elo, player_a_new.elo,
                                "Player A Elo should have changed"
                            );
                            assert_ne!(
                                player_b_old.elo, player_b_new.elo,
                                "Player B Elo should have changed"
                            );
                        }
                    }
                    break;
                }

                round += 1;
                if round > 10 {
                    panic!("Game should have ended by now");
                }
            }

            // Consume the layer at the end of the test
            let _ = layer.commit();
        });
    }

    #[test]
    fn test_invalid_signature_does_nothing() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);

            let (signer_a, actor_a) = create_test_actor(1);
            let (signer_b, _) = create_test_actor(2);

            // Setup battle with moves
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Commit and create new layer with advanced view to expire the lobby
            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);

            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert_eq!(events.len(), 1);

            // Get the battle expiry
            let battle_expiry = layer
                .view()
                .checked_add(MOVE_EXPIRY)
                .expect("view overflow");

            // Send moves
            let move_a = create_test_move_ciphertext(master_public, battle_expiry, 1);
            let move_b = create_test_move_ciphertext(master_public, battle_expiry, 2);

            let tx = Transaction::sign(&signer_a, 2, Instruction::Move(move_a));
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let tx = Transaction::sign(&signer_b, 2, Instruction::Move(move_b));
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Commit and create new layer with advanced view past expiry
            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, battle_expiry + 1);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);

            let wrong_signature =
                ops::sign_message::<MinSig>(&network_secret, Some(b"test"), b"wrong");

            let tx = Transaction::sign(&signer_a, 3, Instruction::Settle(wrong_signature));
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert!(events.is_empty()); // Should produce no events

            // Battle should still be active with pending moves
            let battle_key = layer.get(&Key::Account(actor_a.clone())).await.unwrap();
            if let Value::Account(account) = battle_key {
                assert!(account.battle.is_some());
            }

            // Consume the layer at the end of the test
            let _ = layer.commit();
        });
    }

    #[test]
    fn test_decryption_failure_defaults_to_no_move() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);

            let (signer_a, _) = create_test_actor(1);
            let (signer_b, _) = create_test_actor(2);

            // Setup battle
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Commit and create new layer with advanced view to expire the lobby
            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);

            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert_eq!(events.len(), 1);

            // Extract battle info from the event to know actual player assignments
            let (_, actual_player_a, _) = match &events[0] {
                Event::Matched {
                    battle,
                    expiry: _,
                    player_a,
                    player_a_creature: _,
                    player_a_stats: _,
                    player_b,
                    player_b_creature: _,
                    player_b_stats: _,
                } => (*battle, player_a.clone(), player_b.clone()),
                _ => panic!("Expected Matched event"),
            };

            // Get the battle expiry
            let battle_expiry = layer
                .view()
                .checked_add(MOVE_EXPIRY)
                .expect("view overflow");

            // Create moves based on actual player assignments
            // We want the player in position A to have a bad move that fails decryption
            let (bad_move_signer, bad_move_nonce, good_move_signer, good_move_nonce) =
                if actual_player_a == signer_a.public_key() {
                    // signer_a is in position A, signer_b is in position B
                    (&signer_a, 2, &signer_b, 2)
                } else {
                    // signer_b is in position A, signer_a is in position B
                    (&signer_b, 2, &signer_a, 2)
                };

            // Create a ciphertext that will fail decryption (wrong target)
            let bad_move = create_test_move_ciphertext(master_public, 9999, 3); // Wrong expiry
            let good_move = create_test_move_ciphertext(master_public, battle_expiry, 2);

            let tx =
                Transaction::sign(bad_move_signer, bad_move_nonce, Instruction::Move(bad_move));
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let tx = Transaction::sign(
                good_move_signer,
                good_move_nonce,
                Instruction::Move(good_move),
            );
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Commit and create new layer with advanced view past expiry
            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, battle_expiry + 1);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);

            let signature = create_seed(&network_secret, battle_expiry);

            let tx = Transaction::sign(&signer_a, 3, Instruction::Settle(signature.signature));
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;

            // Find the move event
            let move_event = events
                .iter()
                .find(|e| matches!(e, Event::Moved { .. }))
                .unwrap();

            if let Event::Moved {
                player_a_move,
                player_b_move,
                ..
            } = move_event
            {
                // Player A's move should default to 0 (decryption failed)
                assert_eq!(*player_a_move, 0);
                // Player B's move should be 2 (successful decryption)
                assert_eq!(*player_b_move, 2);
            }

            // Consume the layer at the end of the test
            let _ = layer.commit();
        });
    }

    #[test]
    fn test_move_usage_limits_respected() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let (signer_a, _) = create_test_actor(1);
            let (signer_b, _) = create_test_actor(2);

            // Setup battle
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let changes = layer.commit();
            state.apply(changes).await;

            // Match
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let changes = layer.commit();
            state.apply(changes).await;

            // Commit and create new layer with advanced view to expire the lobby
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            let Some(Event::Matched { player_a, .. }) = events.first() else {
                panic!("Expected Matched event");
            };
            let mut nonce_a = 2;
            let mut nonce_b = 2;

            // Determine which signer is player_a in the battle
            let is_signer_a_player_a = signer_a.public_key() == *player_a;

            // We'll have player_a use their strongest move repeatedly
            // Get player_a's account and find their strongest move
            let Some(Value::Account(account_player_a)) =
                layer.get(&Key::Account(player_a.clone())).await
            else {
                panic!("Player A account should exist");
            };
            let creature_player_a = account_player_a.creature.as_ref().unwrap();
            let move_limits = creature_player_a.get_move_usage_limits();

            // Find the strongest move for player_a (non-zero, lowest limit)
            let mut strongest_move = 0;
            let mut min_limit = u8::MAX;
            for (i, &limit) in move_limits.iter().enumerate().skip(1) {
                if limit < min_limit {
                    min_limit = limit;
                    strongest_move = i as u8;
                }
            }

            // Play rounds using the strongest move until we exceed its limit
            let battle_key = account_player_a.battle.unwrap();

            // Play rounds up to the limit
            for _ in 0..min_limit {
                let battle_expiry = layer
                    .view()
                    .checked_add(MOVE_EXPIRY)
                    .expect("view overflow");

                // player_a always uses their strongest move, player_b uses defense
                let (move_signer_a, move_signer_b) = if is_signer_a_player_a {
                    // signer_a is player_a, uses strongest move
                    // signer_b is player_b, uses defense (1)
                    (
                        create_test_move_ciphertext(master_public, battle_expiry, strongest_move),
                        create_test_move_ciphertext(master_public, battle_expiry, 1),
                    )
                } else {
                    // signer_a is player_b, uses defense (1)
                    // signer_b is player_a, uses strongest move
                    (
                        create_test_move_ciphertext(master_public, battle_expiry, 1),
                        create_test_move_ciphertext(master_public, battle_expiry, strongest_move),
                    )
                };

                let tx = Transaction::sign(&signer_a, nonce_a, Instruction::Move(move_signer_a));
                assert!(layer.prepare(&tx).await);
                let events = layer.apply(&tx).await;
                assert!(events.iter().any(|e| matches!(e, Event::Locked { .. })));
                nonce_a += 1;
                let tx = Transaction::sign(&signer_b, nonce_b, Instruction::Move(move_signer_b));
                assert!(layer.prepare(&tx).await);
                let events = layer.apply(&tx).await;
                assert!(events.iter().any(|e| matches!(e, Event::Locked { .. })));
                nonce_b += 1;

                // Settle the round
                let changes = layer.commit();
                state.apply(changes).await;
                let new_seed = create_seed(&network_secret, battle_expiry + 1);
                layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
                let signature = create_seed(&network_secret, battle_expiry);
                let tx =
                    Transaction::sign(&signer_a, nonce_a, Instruction::Settle(signature.signature));
                assert!(layer.prepare(&tx).await);
                let events = layer.apply(&tx).await;
                nonce_a += 1;

                // Verify move was applied
                let move_event = events
                    .iter()
                    .find(|e| matches!(e, Event::Moved { .. }))
                    .unwrap();
                if let Event::Moved {
                    player_a_move,
                    player_b_move,
                    ..
                } = move_event
                {
                    // player_a always uses strongest_move, player_b always uses 1
                    assert_eq!(*player_a_move, strongest_move);
                    assert_eq!(*player_b_move, 1);
                }

                // Check if battle is over
                if layer.get(&Key::Battle(battle_key)).await.is_none() {
                    break;
                }
            }

            // If battle is still ongoing, try to use the strongest move one more time
            if let Some(Value::Battle { .. }) = layer.get(&Key::Battle(battle_key)).await {
                let battle_expiry = layer
                    .view()
                    .checked_add(MOVE_EXPIRY)
                    .expect("view overflow");

                // Try to use the strongest move again (should exceed limit)
                let (move_signer_a, move_signer_b) = if is_signer_a_player_a {
                    // signer_a is player_a, tries to use strongest move again
                    // signer_b is player_b, uses defense (1)
                    (
                        create_test_move_ciphertext(master_public, battle_expiry, strongest_move),
                        create_test_move_ciphertext(master_public, battle_expiry, 1),
                    )
                } else {
                    // signer_a is player_b, uses defense (1)
                    // signer_b is player_a, tries to use strongest move again
                    (
                        create_test_move_ciphertext(master_public, battle_expiry, 1),
                        create_test_move_ciphertext(master_public, battle_expiry, strongest_move),
                    )
                };

                let tx = Transaction::sign(&signer_a, nonce_a, Instruction::Move(move_signer_a));
                assert!(layer.prepare(&tx).await);
                layer.apply(&tx).await;
                nonce_a += 1;
                let tx = Transaction::sign(&signer_b, nonce_b, Instruction::Move(move_signer_b));
                assert!(layer.prepare(&tx).await);
                layer.apply(&tx).await;

                // Settle the round
                let changes = layer.commit();
                state.apply(changes).await;
                let new_seed = create_seed(&network_secret, battle_expiry + 1);
                layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
                let signature = create_seed(&network_secret, battle_expiry);
                let tx =
                    Transaction::sign(&signer_a, nonce_a, Instruction::Settle(signature.signature));
                assert!(layer.prepare(&tx).await);
                let events = layer.apply(&tx).await;

                // Verify move was defaulted to 0 (no move) due to limit exceeded
                let move_event = events
                    .iter()
                    .find(|e| matches!(e, Event::Moved { .. }))
                    .unwrap();
                if let Event::Moved {
                    player_a_move,
                    player_b_move,
                    ..
                } = move_event
                {
                    // player_a's move should be 0 (exceeded limit), player_b should still use 1
                    assert_eq!(*player_a_move, 0);
                    assert_eq!(*player_b_move, 1);
                }
            }

            let _ = layer.commit();
        });
    }

    #[test]
    fn test_move_usage_counts_persist_across_rounds() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let (signer_a, actor_a) = create_test_actor(1);
            let (signer_b, _) = create_test_actor(2);

            // Setup battle
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            let Some(Event::Matched { player_a, .. }) = events.first() else {
                panic!("Expected Matched event");
            };

            let Some(Value::Account(account)) = layer.get(&Key::Account(actor_a.clone())).await
            else {
                panic!("Account should exist");
            };
            let battle_key = account.battle.unwrap();

            // Play first round with move 2
            let battle_expiry = layer
                .view()
                .checked_add(MOVE_EXPIRY)
                .expect("view overflow");
            let move_a = create_test_move_ciphertext(master_public, battle_expiry, 3);
            let move_b = create_test_move_ciphertext(master_public, battle_expiry, 1);

            let tx = Transaction::sign(&signer_a, 2, Instruction::Move(move_a));
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert!(events.iter().any(|e| matches!(e, Event::Locked { .. })));
            let tx = Transaction::sign(&signer_b, 2, Instruction::Move(move_b));
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert!(events.iter().any(|e| matches!(e, Event::Locked { .. })));

            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, battle_expiry + 1);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
            let signature = create_seed(&network_secret, battle_expiry);
            let tx = Transaction::sign(&signer_a, 3, Instruction::Settle(signature.signature));
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;

            // Check if battle ended
            let battle_ended = events.iter().any(|e| matches!(e, Event::Settled { .. }));
            if battle_ended {
                // Battle ended after first round, test is complete
                let _ = layer.commit();
                return;
            }

            // Check move counts are updated
            let Some(Value::Battle {
                player_a_move_counts,
                player_b_move_counts,
                ..
            }) = layer.get(&Key::Battle(battle_key)).await
            else {
                panic!("Battle should exist");
            };
            if signer_a.public_key() == *player_a {
                assert_eq!(
                    player_a_move_counts[3], 1,
                    "Move 2 should have been used once"
                );
            } else {
                assert_eq!(
                    player_b_move_counts[3], 1,
                    "Move 2 should have been used once"
                );
            }

            // Play second round with move 3
            let battle_expiry = layer
                .view()
                .checked_add(MOVE_EXPIRY)
                .expect("view overflow");
            let move_a = create_test_move_ciphertext(master_public, battle_expiry, 4);
            let move_b = create_test_move_ciphertext(master_public, battle_expiry, 1);

            let tx = Transaction::sign(&signer_a, 4, Instruction::Move(move_a));
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_b, 3, Instruction::Move(move_b));
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, battle_expiry + 1);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
            let signature = create_seed(&network_secret, battle_expiry);
            let tx = Transaction::sign(&signer_a, 5, Instruction::Settle(signature.signature));
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Check move counts persist
            if let Some(Value::Battle {
                player_a_move_counts,
                player_b_move_counts,
                ..
            }) = layer.get(&Key::Battle(battle_key)).await
            {
                if signer_a.public_key() == *player_a {
                    assert_eq!(player_a_move_counts[3], 1, "Move 2 count should persist");
                    assert_eq!(
                        player_a_move_counts[4], 1,
                        "Move 3 should have been used once"
                    );
                } else {
                    assert_eq!(player_b_move_counts[3], 1, "Move 2 count should persist");
                    assert_eq!(
                        player_b_move_counts[4], 1,
                        "Move 3 should have been used once"
                    );
                }
            }

            let _ = layer.commit();
        });
    }

    #[test]
    fn test_creature_strength_method() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let (network_secret, _) = create_network_keypair();
            let (_, actor) = create_test_actor(1);
            let seed = create_seed(&network_secret, 1);
            let creature = Creature::new(actor, 0, seed.signature);

            // Test that health() returns the first trait
            assert_eq!(creature.health(), creature.traits[0]);
        });
    }

    #[test]
    fn test_creature_get_move_strengths() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let (network_secret, _) = create_network_keypair();
            let (_, actor) = create_test_actor(1);
            let seed = create_seed(&network_secret, 1);
            let creature = Creature::new(actor, 0, seed.signature);

            let move_strengths = creature.get_move_strengths();

            // Verify correct mapping
            assert_eq!(move_strengths[0], 0); // No-op
            assert_eq!(move_strengths[1], creature.traits[1]); // Defense
            assert_eq!(move_strengths[2], creature.traits[2]); // Attack 1
            assert_eq!(move_strengths[3], creature.traits[3]); // Attack 2
            assert_eq!(move_strengths[4], creature.traits[4]); // Attack 3
        });
    }

    #[test]
    fn test_creature_actions() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let (network_secret, _) = create_network_keypair();
            let (_, actor) = create_test_actor(1);
            let seed = create_seed(&network_secret, 1);
            let creature = Creature::new(actor, 0, seed.signature);

            assert_eq!(creature.action(0, seed.signature), (false, 0));
            assert_eq!(
                creature.action(TOTAL_MOVES as u8, seed.signature),
                (false, 0)
            );
            assert_eq!(creature.action(u8::MAX, seed.signature), (false, 0));
        });
    }

    #[test]
    fn test_creature_action_minimum_effectiveness() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let (network_secret, _) = create_network_keypair();
            let (_, actor) = create_test_actor(1);
            let seed = create_seed(&network_secret, 1);
            let creature = Creature::new(actor, 0, seed.signature);

            // Test multiple seeds to ensure minimum is 1/2 of max
            for i in 0..100 {
                // Create different signatures for testing
                let (sk, _) = create_network_keypair();
                let test_seed = ops::sign_message::<MinSig>(&sk, Some(b"test"), &[i; 32]);

                // Check all moves
                for move_idx in 1..TOTAL_MOVES as u8 {
                    let (is_defense, power) = creature.action(move_idx, test_seed);
                    let max_power = creature.traits[move_idx as usize];
                    let min_expected = max_power / 2;

                    // Verify the power is at least half of max
                    assert!(power >= min_expected);

                    // Verify the power doesn't exceed max
                    assert!(power <= max_power);

                    // Verify defense flag is correct
                    assert_eq!(is_defense, move_idx == 1);
                }
            }
        });
    }

    #[test]
    fn test_generate_multiple_times() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let (signer, _) = create_test_actor(1);

            // Generate first creature
            let tx = Transaction::sign(&signer, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert_eq!(events.len(), 1);
            assert!(matches!(events[0], Event::Generated { .. }));

            // Try to generate again (should replace existing)
            let tx = Transaction::sign(&signer, 1, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert_eq!(events.len(), 1);
            assert!(matches!(events[0], Event::Generated { .. }));

            let _ = layer.commit();
        });
    }

    #[test]
    fn test_match_with_empty_lobby() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let (signer, actor) = create_test_actor(1);

            // Generate creature first
            let tx = Transaction::sign(&signer, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Try to match with empty lobby
            let tx = Transaction::sign(&signer, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;

            // Should be added to lobby
            assert_eq!(events.len(), 0); // No match event, just added to lobby

            // Verify actor is in lobby
            if let Some(Value::Lobby { players, .. }) = layer.get(&Key::Lobby).await {
                assert!(players.contains(&actor));
            } else {
                panic!("Lobby should exist");
            }

            let _ = layer.commit();
        });
    }

    #[test]
    fn test_move_with_no_battle() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let (signer, _) = create_test_actor(1);

            // Generate creature
            let tx = Transaction::sign(&signer, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Try to move without being in battle
            let encrypted_move = create_test_move_ciphertext(master_public, 100, 1);
            let tx = Transaction::sign(&signer, 1, Instruction::Move(encrypted_move));
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;

            // Should return empty events (no-op)
            assert_eq!(events.len(), 0);

            let _ = layer.commit();
        });
    }

    #[test]
    fn test_settle_with_no_battle() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let (signer, _) = create_test_actor(1);

            // Generate creature
            let tx = Transaction::sign(&signer, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Try to settle without being in battle
            let signature = create_seed(&network_secret, 100);
            let tx = Transaction::sign(&signer, 1, Instruction::Settle(signature.signature));
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;

            // Should return empty events (no-op)
            assert_eq!(events.len(), 0);

            let _ = layer.commit();
        });
    }

    #[test]
    fn test_move_when_turn_expired() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let (signer_a, _) = create_test_actor(1);
            let (signer_b, _) = create_test_actor(2);

            // Setup battle
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Advance time past move expiry
            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 202);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed); // Past MOVE_EXPIRY

            // Try to submit move after expiry
            let encrypted_move = create_test_move_ciphertext(master_public, 102 + MOVE_EXPIRY, 1);
            let tx = Transaction::sign(&signer_a, 2, Instruction::Move(encrypted_move));
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;

            // Should return empty events (expired)
            assert_eq!(events.len(), 0);

            let _ = layer.commit();
        });
    }

    #[test]
    fn test_settle_before_turn_expired() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let (signer_a, _) = create_test_actor(1);
            let (signer_b, _) = create_test_actor(2);

            // Setup battle
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let battle_expiry = layer
                .view()
                .checked_add(MOVE_EXPIRY)
                .expect("view overflow");

            // Submit moves
            let move_a = create_test_move_ciphertext(master_public, battle_expiry, 1);
            let move_b = create_test_move_ciphertext(master_public, battle_expiry, 2);
            let tx = Transaction::sign(&signer_a, 2, Instruction::Move(move_a));
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_b, 2, Instruction::Move(move_b));
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Try to settle before expiry (should fail)
            let signature = create_seed(&network_secret, battle_expiry);
            let tx = Transaction::sign(&signer_a, 3, Instruction::Settle(signature.signature));
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;

            // Should return empty events (not expired yet)
            assert_eq!(events.len(), 0);

            let _ = layer.commit();
        });
    }

    #[test]
    fn test_double_move_submission() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let (signer_a, _) = create_test_actor(1);
            let (signer_b, _) = create_test_actor(2);

            // Setup battle
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let battle_expiry = layer
                .view()
                .checked_add(MOVE_EXPIRY)
                .expect("view overflow");

            // Submit first move
            let move_a1 = create_test_move_ciphertext(master_public, battle_expiry, 1);
            let tx = Transaction::sign(&signer_a, 2, Instruction::Move(move_a1));
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Try to submit second move (should fail)
            let move_a2 = create_test_move_ciphertext(master_public, battle_expiry, 2);
            let tx = Transaction::sign(&signer_a, 3, Instruction::Move(move_a2));
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;

            // Should return empty events (already moved)
            assert_eq!(events.len(), 0);

            let _ = layer.commit();
        });
    }

    #[test]
    fn test_battle_with_all_health_scenarios() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let (signer_a, actor_a) = create_test_actor(1);
            let (signer_b, actor_b) = create_test_actor(2);

            // Setup battle
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Get initial healths to calculate number of rounds needed
            let Some(Value::Account(account_a)) = layer.get(&Key::Account(actor_a.clone())).await
            else {
                panic!("Account A should exist");
            };
            let Some(Value::Account(account_b)) = layer.get(&Key::Account(actor_b.clone())).await
            else {
                panic!("Account B should exist");
            };

            let _max_health_a = account_a.creature.as_ref().unwrap().health();
            let _max_health_b = account_b.creature.as_ref().unwrap().health();

            // Play rounds with both players attacking
            let mut nonce_a = 2;
            let mut nonce_b = 2;
            let mut round = 0;

            loop {
                let battle_expiry = layer
                    .view()
                    .checked_add(MOVE_EXPIRY)
                    .expect("view overflow");

                // Both players attack (not defend)
                let move_a = create_test_move_ciphertext(master_public, battle_expiry, 2);
                let move_b = create_test_move_ciphertext(master_public, battle_expiry, 3);

                let tx = Transaction::sign(&signer_a, nonce_a, Instruction::Move(move_a));
                assert!(layer.prepare(&tx).await);
                layer.apply(&tx).await;
                nonce_a += 1;
                let tx = Transaction::sign(&signer_b, nonce_b, Instruction::Move(move_b));
                assert!(layer.prepare(&tx).await);
                layer.apply(&tx).await;
                nonce_b += 1;

                // Settle
                let changes = layer.commit();
                state.apply(changes).await;
                let new_seed = create_seed(&network_secret, battle_expiry + 1);
                layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
                let signature = create_seed(&network_secret, battle_expiry);
                let tx =
                    Transaction::sign(&signer_a, nonce_a, Instruction::Settle(signature.signature));
                assert!(layer.prepare(&tx).await);
                let events = layer.apply(&tx).await;
                nonce_a += 1;

                // Check if battle ended
                let settled = events.iter().any(|e| matches!(e, Event::Settled { .. }));
                if settled {
                    // Verify proper ELO update occurred
                    let Some(Value::Account(final_a)) =
                        layer.get(&Key::Account(actor_a.clone())).await
                    else {
                        panic!("Account A should exist");
                    };
                    let Some(Value::Account(final_b)) =
                        layer.get(&Key::Account(actor_b.clone())).await
                    else {
                        panic!("Account B should exist");
                    };

                    // Check that ELO changed
                    assert_ne!(
                        final_a.stats.elo, account_a.stats.elo,
                        "ELO should have changed for player A"
                    );
                    assert_ne!(
                        final_b.stats.elo, account_b.stats.elo,
                        "ELO should have changed for player B"
                    );

                    // Check win/loss/draw counters
                    let total_games_a =
                        final_a.stats.wins + final_a.stats.losses + final_a.stats.draws;
                    let total_games_b =
                        final_b.stats.wins + final_b.stats.losses + final_b.stats.draws;
                    assert_eq!(total_games_a, 1, "Player A should have exactly 1 game");
                    assert_eq!(total_games_b, 1, "Player B should have exactly 1 game");

                    break;
                }

                round += 1;
                if round > 100 {
                    panic!("Battle should have ended by now");
                }
            }

            let _ = layer.commit();
        });
    }

    #[test]
    fn test_player_a_defends_player_b_attacks() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let (signer_a, _) = create_test_actor(1);
            let (signer_b, _) = create_test_actor(2);

            // Setup battle
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            let Some(Event::Matched { player_a, .. }) = events.first() else {
                panic!("Expected Matched event");
            };

            let battle_expiry = layer
                .view()
                .checked_add(MOVE_EXPIRY)
                .expect("view overflow");

            // Player A defends (move 0), Player B attacks (move 2)
            let move_a = create_test_move_ciphertext(master_public, battle_expiry, 1);
            let move_b = create_test_move_ciphertext(master_public, battle_expiry, 3);

            let tx = Transaction::sign(&signer_a, 2, Instruction::Move(move_a));
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_b, 2, Instruction::Move(move_b));
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, battle_expiry + 1);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
            let signature = create_seed(&network_secret, battle_expiry);
            let tx = Transaction::sign(&signer_a, 3, Instruction::Settle(signature.signature));
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;

            // Find the move event to verify impacts
            let move_event = events
                .iter()
                .find(|e| matches!(e, Event::Moved { .. }))
                .unwrap();
            if let Event::Moved {
                player_a_power,
                player_b_power,
                ..
            } = move_event
            {
                // Player A defended (positive impact), Player B attacked (negative impact)
                if signer_a.public_key() == *player_a {
                    assert!(*player_a_power > 0, "Defense should have positive impact");
                    assert!(*player_b_power > 0, "Attack should have negative impact");
                } else {
                    assert!(*player_b_power > 0, "Attack should have positive impact");
                    assert!(*player_a_power > 0, "Defense should have negative impact");
                }
            }

            let _ = layer.commit();
        });
    }

    #[test]
    fn test_battle_times_out_after_max_rounds() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let (signer_a, actor_a) = create_test_actor(1);
            let (signer_b, actor_b) = create_test_actor(2);

            // Generate creatures
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let changes = layer.commit();
            state.apply(changes).await;

            // Match players
            let seed = create_seed(&network_secret, 2);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let changes = layer.commit();
            state.apply(changes).await;

            // Jump ahead to view 3
            let view = 2 + LOBBY_EXPIRY + 1;
            let seed = create_seed(&network_secret, view);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            let Some(Event::Matched { battle, .. }) =
                events.iter().find(|e| matches!(e, Event::Matched { .. }))
            else {
                panic!("Battle should be matched");
            };
            let changes = layer.commit();
            state.apply(changes).await;

            // Simulate MAX_BATTLE_ROUNDS rounds where both players do nothing (move 0)
            let mut nonce_a = 2;
            let mut nonce_b = 2;

            let mut view = view + 1;
            for round in 0..MAX_BATTLE_ROUNDS {
                // Get battle expiry
                let Some(Value::Battle { expiry, .. }) = state.get(&Key::Battle(*battle)).await
                else {
                    panic!("Battle should exist");
                };

                // Both players make no move (move 0)
                let seed = create_seed(&network_secret, view);
                layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
                let move_a = create_test_move_ciphertext(master_public, expiry, 0);
                let move_b = create_test_move_ciphertext(master_public, expiry, 0);

                let tx = Transaction::sign(&signer_a, nonce_a, Instruction::Move(move_a));
                assert!(layer.prepare(&tx).await);
                let events = layer.apply(&tx).await;
                assert!(events.iter().any(|e| matches!(e, Event::Locked { .. })));
                nonce_a += 1;

                let tx = Transaction::sign(&signer_b, nonce_b, Instruction::Move(move_b));
                assert!(layer.prepare(&tx).await);
                let events = layer.apply(&tx).await;
                assert!(events.iter().any(|e| matches!(e, Event::Locked { .. })));
                nonce_b += 1;
                let changes = layer.commit();
                state.apply(changes).await;

                // Advance time and settle
                view = expiry + 1;
                let new_seed = create_seed(&network_secret, view);
                let new_layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
                layer = new_layer;

                let expiry_seed = create_seed(&network_secret, expiry);
                let tx = Transaction::sign(
                    &signer_a,
                    nonce_a,
                    Instruction::Settle(expiry_seed.signature),
                );
                assert!(layer.prepare(&tx).await);
                let events = layer.apply(&tx).await;
                nonce_a += 1;

                // Check if battle ended
                if let Some(Event::Settled { round, outcome, .. }) =
                    events.iter().find(|e| matches!(e, Event::Settled { .. }))
                {
                    assert_eq!(*round, MAX_BATTLE_ROUNDS);
                    assert_eq!(*outcome, Outcome::Draw);

                    // Verify both players' draw counts increased
                    let Some(Value::Account(account_a_final)) =
                        layer.get(&Key::Account(actor_a.clone())).await
                    else {
                        panic!("Account A should exist");
                    };
                    let Some(Value::Account(account_b_final)) =
                        layer.get(&Key::Account(actor_b.clone())).await
                    else {
                        panic!("Account B should exist");
                    };

                    assert_eq!(account_a_final.stats.draws, 1);
                    assert_eq!(account_b_final.stats.draws, 1);
                    assert!(account_a_final.battle.is_none());
                    assert!(account_b_final.battle.is_none());

                    return; // Test passed
                }

                // If we haven't reached MAX_BATTLE_ROUNDS yet, continue
                if round < MAX_BATTLE_ROUNDS - 1 {
                    assert!(
                        events.iter().any(|e| matches!(e, Event::Moved { .. })),
                        "Round {round}: Expected Moved event but got {events:?}",
                    );
                }

                // Update view and state for next iteration
                let changes = layer.commit();
                state.apply(changes).await;
                view += 1;
            }

            panic!("Battle should have ended in a draw after MAX_BATTLE_ROUNDS");
        });
    }

    #[test]
    fn test_out_of_range_moves_handled_as_no_move() {
        let executor = Runner::default();
        executor.start(|_| async move {
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let (signer_a, actor_a) = create_test_actor(1);
            let (signer_b, _) = create_test_actor(2);

            // Setup battle
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let changes = layer.commit();
            state.apply(changes).await;

            // Get the battle key and determine player positions
            let Some(Value::Account(account_a)) = state.get(&Key::Account(actor_a.clone())).await
            else {
                panic!("Account A should exist");
            };
            let battle_key = account_a.battle.unwrap();

            // Get battle to determine player positions
            let Some(Value::Battle { player_a, .. }) = state.get(&Key::Battle(battle_key)).await
            else {
                panic!("Battle should exist");
            };

            // Submit out-of-range moves
            let Some(Value::Battle { expiry, .. }) = state.get(&Key::Battle(battle_key)).await
            else {
                panic!("Battle should exist");
            };

            // Test various out-of-range values
            let out_of_range_moves = vec![
                5,   // Just past ALLOWED_MOVES (4)
                10,  // Way out of range
                100, // Very large
                255, // u8::MAX
            ];

            // Test each out-of-range move
            let mut next_expiry = expiry;
            let mut nonce_a = 2;
            let mut nonce_b = 2;
            for out_of_range_move in out_of_range_moves {
                // Create new layer
                let new_seed = create_seed(&network_secret, next_expiry);
                layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);

                // Player A (whichever signer is in that position) submits out-of-range move
                let move_out_of_range =
                    create_test_move_ciphertext(master_public, next_expiry, out_of_range_move);
                let tx =
                    Transaction::sign(&signer_a, nonce_a, Instruction::Move(move_out_of_range));
                assert!(layer.prepare(&tx).await);
                let events = layer.apply(&tx).await;
                assert!(events.iter().any(|e| matches!(e, Event::Locked { .. })));
                nonce_a += 1;

                // Player B submits valid defense move
                let move_valid = create_test_move_ciphertext(master_public, next_expiry, 1);
                let tx = Transaction::sign(&signer_b, nonce_b, Instruction::Move(move_valid));
                assert!(layer.prepare(&tx).await);
                let events = layer.apply(&tx).await;
                assert!(events.iter().any(|e| matches!(e, Event::Locked { .. })));
                nonce_b += 1;
                let changes = layer.commit();
                state.apply(changes).await;

                // Settle the round
                let settle_seed = create_seed(&network_secret, next_expiry + 1);
                layer = Layer::new(&state, master_public, TEST_NAMESPACE, settle_seed);
                let signature = create_seed(&network_secret, next_expiry);
                let tx =
                    Transaction::sign(&signer_a, nonce_a, Instruction::Settle(signature.signature));
                assert!(layer.prepare(&tx).await);
                let events = layer.apply(&tx).await;
                nonce_a += 1;

                // Check that the move was treated as no-op
                let Some(Event::Moved {
                    expiry,
                    player_a_move,
                    player_b_move,
                    ..
                }) = events.first()
                else {
                    panic!("Expected Moved event");
                };
                if signer_a.public_key() == player_a {
                    // Player A's out-of-range move should be treated as 0 (no move)
                    assert_eq!(*player_a_move, 0);
                    // Player B's valid move should remain unchanged
                    assert_eq!(*player_b_move, 1);
                } else {
                    assert_eq!(*player_a_move, 1);
                    assert_eq!(*player_b_move, 0);
                }
                next_expiry = *expiry;

                // Check that the battle is still ongoing (no one should have won from a no-op)
                let Some(Value::Account(account)) = layer.get(&Key::Account(actor_a.clone())).await
                else {
                    panic!("Account should exist");
                };
                assert!(account.battle.is_some(), "Battle should still be ongoing");

                // Update state
                let changes = layer.commit();
                state.apply(changes).await;
            }
        });
    }

    #[test]
    fn test_prefer_result_over_draw_when_player_killed_in_last_round() {
        let executor = Runner::default();
        executor.start(|_| async move {
            // Setup battle
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let (signer_a, actor_a) = create_test_actor(1);
            let (signer_b, actor_b) = create_test_actor(2);

            // Generate creatures
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;
            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Match players
            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Commit and advance view to expire lobby
            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);
            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;

            // Get battle info and determine which signer is which player
            let (battle, actual_player_a, _actual_player_b) = if let Event::Matched {
                battle,
                player_a,
                player_b,
                ..
            } = &events[0]
            {
                (*battle, player_a.clone(), player_b.clone())
            } else {
                panic!("Expected Matched event");
            };

            // Determine which signer corresponds to which battle position
            let is_signer_a_player_a = actual_player_a == actor_a;

            // Commit the state and directly modify the battle to set both players' health to 1
            let changes = layer.commit();
            state.apply(changes).await;

            // Get the battle and modify health values
            let Some(Value::Battle {
                expiry,
                round,
                player_a,
                player_a_max_health,
                player_a_pending,
                player_a_move_counts,
                player_b,
                player_b_max_health,
                player_b_pending,
                player_b_move_counts,
                ..
            }) = state.get(&Key::Battle(battle)).await
            else {
                panic!("Battle should exist");
            };
            state
                .insert(
                    Key::Battle(battle),
                    Value::Battle {
                        expiry,
                        round,
                        player_a: player_a.clone(),
                        player_a_max_health,
                        player_a_health: 1,
                        player_a_pending,
                        player_a_move_counts,
                        player_b: player_b.clone(),
                        player_b_max_health,
                        player_b_health: 1,
                        player_b_pending,
                        player_b_move_counts,
                    },
                )
                .await;

            // Simulate rounds until we're at the last round
            let mut nonce_a = 2;
            let mut nonce_b = 2;
            let mut view = 103;

            // Skip to just before the last round
            for _ in 0..(MAX_BATTLE_ROUNDS - 1) {
                let seed = create_seed(&network_secret, view);
                layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);

                let Some(Value::Battle { expiry, .. }) = layer.get(&Key::Battle(battle)).await
                else {
                    panic!("Battle should exist");
                };
                let battle_expiry = expiry;

                // Both players do nothing
                let move_a = create_test_move_ciphertext(master_public, battle_expiry, 0);
                let move_b = create_test_move_ciphertext(master_public, battle_expiry, 0);

                let tx = Transaction::sign(&signer_a, nonce_a, Instruction::Move(move_a));
                assert!(layer.prepare(&tx).await);
                layer.apply(&tx).await;
                nonce_a += 1;

                let tx = Transaction::sign(&signer_b, nonce_b, Instruction::Move(move_b));
                assert!(layer.prepare(&tx).await);
                layer.apply(&tx).await;
                nonce_b += 1;

                // Commit and advance to settle
                let changes = layer.commit();
                state.apply(changes).await;
                view = battle_expiry + 1;
                let seed = create_seed(&network_secret, view);
                layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);

                // Settle the round
                let signature = create_seed(&network_secret, battle_expiry);
                let tx =
                    Transaction::sign(&signer_a, nonce_a, Instruction::Settle(signature.signature));
                assert!(layer.prepare(&tx).await);
                let events = layer.apply(&tx).await;
                nonce_a += 1;

                // Verify battle continues
                assert!(events.iter().any(|e| matches!(e, Event::Moved { .. })));
                assert!(!events.iter().any(|e| matches!(e, Event::Settled { .. })));

                let changes = layer.commit();
                state.apply(changes).await;
                view += 1;
            }

            // Now we're at round MAX_BATTLE_ROUNDS (the last round)
            // Both players have health = 1
            let seed = create_seed(&network_secret, view);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);
            let Some(Value::Battle { expiry, .. }) = layer.get(&Key::Battle(battle)).await else {
                panic!("Battle should exist");
            };

            // We want battle's player_a to attack and player_b to do nothing
            // This should result in player_a winning (not a draw)
            if is_signer_a_player_a {
                // signer_a is player_a (the attacker)
                let attack_move = create_test_move_ciphertext(master_public, expiry, 2); // Any attack move
                let no_move = create_test_move_ciphertext(master_public, expiry, 0); // No action

                let tx = Transaction::sign(&signer_a, nonce_a, Instruction::Move(attack_move));
                assert!(layer.prepare(&tx).await);
                layer.apply(&tx).await;
                nonce_a += 1;

                let tx = Transaction::sign(&signer_b, nonce_b, Instruction::Move(no_move));
                assert!(layer.prepare(&tx).await);
                layer.apply(&tx).await;
            } else {
                // signer_b is player_a (the attacker)
                let no_move = create_test_move_ciphertext(master_public, expiry, 0); // No action
                let attack_move = create_test_move_ciphertext(master_public, expiry, 2); // Any attack move

                let tx = Transaction::sign(&signer_a, nonce_a, Instruction::Move(no_move));
                assert!(layer.prepare(&tx).await);
                layer.apply(&tx).await;
                nonce_a += 1;

                let tx = Transaction::sign(&signer_b, nonce_b, Instruction::Move(attack_move));
                assert!(layer.prepare(&tx).await);
                layer.apply(&tx).await;
            }

            // Commit and advance to settle
            let changes = layer.commit();
            state.apply(changes).await;
            view = expiry + 1;
            let new_seed = create_seed(&network_secret, view);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);

            // Settle the final round
            let signature = create_seed(&network_secret, expiry);
            let tx =
                Transaction::sign(&signer_a, nonce_a, Instruction::Settle(signature.signature));
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;

            // Verify the outcome
            let settled_event = events.iter().find(|e| matches!(e, Event::Settled { .. }));
            assert!(settled_event.is_some(), "Battle should have been settled");
            if let Some(Event::Settled { outcome, round, .. }) = settled_event {
                assert_eq!(
                    *round, MAX_BATTLE_ROUNDS,
                    "Battle should end at MAX_BATTLE_ROUNDS"
                );
                assert_eq!(
                    *outcome,
                    Outcome::PlayerA,
                    "Player A should win by killing Player B in the last round"
                );
            }

            // Verify final account states
            let final_account_a =
                if let Some(Value::Account(acc)) = layer.get(&Key::Account(actor_a)).await {
                    acc
                } else {
                    panic!("Account A not found after battle");
                };
            let final_account_b =
                if let Some(Value::Account(acc)) = layer.get(&Key::Account(actor_b)).await {
                    acc
                } else {
                    panic!("Account B not found after battle");
                };
            if is_signer_a_player_a {
                // signer_a was player_a (winner)
                assert_eq!(
                    final_account_a.stats.wins, 1,
                    "Signer A (as Player A) should have won"
                );
                assert_eq!(final_account_a.stats.draws, 0);
                assert_eq!(
                    final_account_b.stats.losses, 1,
                    "Signer B (as Player B) should have lost"
                );
                assert_eq!(final_account_b.stats.draws, 0);
            } else {
                // signer_b was player_a (winner)
                assert_eq!(
                    final_account_b.stats.wins, 1,
                    "Signer B (as Player A) should have won"
                );
                assert_eq!(final_account_b.stats.draws, 0);
                assert_eq!(
                    final_account_a.stats.losses, 1,
                    "Signer A (as Player B) should have lost"
                );
                assert_eq!(final_account_a.stats.draws, 0);
            }

            let _ = layer.commit();
        });
    }

    #[test]
    fn test_defense_moves_never_exceed_max_health() {
        let executor = Runner::default();
        executor.start(|_| async move {
            // This test verifies that using defense moves never causes health to exceed the creature's maximum health
            let mut state = MockState::new();
            let (network_secret, master_public) = create_network_keypair();
            let seed = create_seed(&network_secret, 1);
            let mut layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);

            let (signer_a, _) = create_test_actor(1);
            let (signer_b, _) = create_test_actor(2);

            // Generate creatures
            let tx = Transaction::sign(&signer_a, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            let tx = Transaction::sign(&signer_b, 0, Instruction::Generate);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Match players
            let tx = Transaction::sign(&signer_a, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            layer.apply(&tx).await;

            // Commit and advance view to expire lobby
            let changes = layer.commit();
            state.apply(changes).await;
            let new_seed = create_seed(&network_secret, 102);
            layer = Layer::new(&state, master_public, TEST_NAMESPACE, new_seed);

            let tx = Transaction::sign(&signer_b, 1, Instruction::Match);
            assert!(layer.prepare(&tx).await);
            let events = layer.apply(&tx).await;
            assert_eq!(events.len(), 1);

            // Get battle info
            let Some(Event::Matched { battle, .. }) = events.first() else {
                panic!("Expected Matched event");
            };

            // Play several rounds where both players use defense moves
            let mut nonce_a = 2;
            let mut nonce_b = 2;
            let mut view = 103;
            let changes = layer.commit();
            state.apply(changes).await;

            for round in 0..5 {
                let seed = create_seed(&network_secret, view);
                layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);

                let Some(Value::Battle { expiry, .. }) = layer.get(&Key::Battle(*battle)).await
                else {
                    panic!("Battle should exist");
                };
                let battle_expiry = expiry;

                // Both players use defense moves (move 1)
                let defense_move_a = create_test_move_ciphertext(master_public, battle_expiry, 1);
                let defense_move_b = create_test_move_ciphertext(master_public, battle_expiry, 1);

                let tx = Transaction::sign(&signer_a, nonce_a, Instruction::Move(defense_move_a));
                assert!(layer.prepare(&tx).await);
                layer.apply(&tx).await;
                nonce_a += 1;

                let tx = Transaction::sign(&signer_b, nonce_b, Instruction::Move(defense_move_b));
                assert!(layer.prepare(&tx).await);
                layer.apply(&tx).await;
                nonce_b += 1;

                // Commit and advance to settle
                let changes = layer.commit();
                state.apply(changes).await;
                view = battle_expiry + 1;
                let seed = create_seed(&network_secret, view);
                layer = Layer::new(&state, master_public, TEST_NAMESPACE, seed);

                // Settle the round
                let signature = create_seed(&network_secret, battle_expiry);
                let tx =
                    Transaction::sign(&signer_a, nonce_a, Instruction::Settle(signature.signature));
                assert!(layer.prepare(&tx).await);
                let events = layer.apply(&tx).await;
                nonce_a += 1;

                // Check health values after the round
                assert!(
                    events.iter().any(|e| matches!(e, Event::Moved { .. })),
                    "Should have a Moved event"
                );

                // Check the battle state to verify health values
                if let Some(Value::Battle {
                    player_a_health,
                    player_a_max_health,
                    player_b_health,
                    player_b_max_health,
                    ..
                }) = layer.get(&Key::Battle(*battle)).await
                {
                    // Verify that health never exceeds maximum
                    assert!(player_a_health <= player_a_max_health);
                    assert!(player_b_health <= player_b_max_health);
                } else {
                    panic!("Battle should exist after round {round}");
                }

                let changes = layer.commit();
                state.apply(changes).await;
                view += 1;
            }
        });
    }
}
