use bytes::{Buf, BufMut};
use commonware_codec::{
    varint::UInt, Encode, EncodeSize, Error, FixedSize, RangeCfg, Read, ReadExt, ReadRangeExt,
    Write,
};
use commonware_consensus::threshold_simplex::types::{
    Activity as CActivity, Finalization as CFinalization, Notarization as CNotarization,
    Seed as CSeed, View,
};
use commonware_cryptography::{
    bls12381::{
        primitives::variant::{MinSig, Variant},
        tle::Ciphertext,
    },
    ed25519::{self, Batch, PublicKey},
    sha256::{Digest, Sha256},
    BatchVerifier, Committable, Digestible, Hasher, Signer, Verifier,
};
use commonware_utils::{modulo, union};
use std::{collections::BTreeSet, fmt::Debug, hash::Hash};

pub const MAX_LOBBY_SIZE: usize = 128;
pub const ALLOWED_MOVES: usize = 4;
pub const TOTAL_MOVES: usize = 1 + ALLOWED_MOVES; // Includes move 0 (no-op) + 4 actual moves
pub const MIN_HEALTH_POINTS: u8 = 75;
pub const TOTAL_SKILL_POINTS: u16 = 300;
pub const SKILLS: usize = 5;
pub const BASE_MOVE_LIMIT: u16 = 15;

pub const NAMESPACE: &[u8] = b"_BATTLEWARE";
pub const TRANSACTION_SUFFIX: &[u8] = b"_TX";
pub const MAX_BLOCK_TRANSACTIONS: usize = 100;
pub const MAX_BATTLE_ROUNDS: u8 = 15;
pub const LOBBY_EXPIRY: u64 = 25;
pub const MOVE_EXPIRY: u64 = 50;

pub type Seed = CSeed<MinSig>;
pub type Notarization = CNotarization<MinSig, Digest>;
pub type Finalization = CFinalization<MinSig, Digest>;
pub type Activity = CActivity<MinSig, Digest>;

pub type Identity = <MinSig as Variant>::Public;
pub type Evaluation = Identity;
pub type Signature = <MinSig as Variant>::Signature;

#[inline]
pub fn transaction_namespace(namespace: &[u8]) -> Vec<u8> {
    union(namespace, TRANSACTION_SUFFIX)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transaction {
    pub nonce: u64,
    pub instruction: Instruction,

    pub public: ed25519::PublicKey,
    pub signature: ed25519::Signature,
}

impl Transaction {
    fn payload(nonce: &u64, instruction: &Instruction) -> Vec<u8> {
        let mut payload = Vec::new();
        nonce.write(&mut payload);
        instruction.write(&mut payload);

        payload
    }

    pub fn sign(private: &ed25519::PrivateKey, nonce: u64, instruction: Instruction) -> Self {
        let signature = private.sign(
            Some(&transaction_namespace(NAMESPACE)),
            &Self::payload(&nonce, &instruction),
        );

        Self {
            nonce,
            instruction,
            public: private.public_key(),
            signature,
        }
    }

    pub fn verify(&self) -> bool {
        self.public.verify(
            Some(&transaction_namespace(NAMESPACE)),
            &Self::payload(&self.nonce, &self.instruction),
            &self.signature,
        )
    }

    pub fn verify_batch(&self, batch: &mut Batch) {
        batch.add(
            Some(&transaction_namespace(NAMESPACE)),
            &Self::payload(&self.nonce, &self.instruction),
            &self.public,
            &self.signature,
        );
    }
}

impl Write for Transaction {
    fn write(&self, writer: &mut impl BufMut) {
        self.nonce.write(writer);
        self.instruction.write(writer);
        self.public.write(writer);
        self.signature.write(writer);
    }
}

impl Read for Transaction {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let nonce = u64::read(reader)?;
        let instruction = Instruction::read(reader)?;
        let public = ed25519::PublicKey::read(reader)?;
        let signature = ed25519::Signature::read(reader)?;

        Ok(Self {
            nonce,
            instruction,
            public,
            signature,
        })
    }
}

impl EncodeSize for Transaction {
    fn encode_size(&self) -> usize {
        self.nonce.encode_size()
            + self.instruction.encode_size()
            + self.public.encode_size()
            + self.signature.encode_size()
    }
}

impl Digestible for Transaction {
    type Digest = Digest;

    fn digest(&self) -> Digest {
        let mut hasher = Sha256::new();
        hasher.update(self.nonce.to_be_bytes().as_ref());
        hasher.update(self.instruction.encode().as_ref());
        hasher.update(self.public.as_ref());
        // We don't include the signature as part of the digest (any valid
        // signature will be valid for the transaction)
        hasher.finalize()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum Instruction {
    Generate,
    Match,
    Move(Ciphertext<MinSig>),
    Settle(Signature),
}

impl Write for Instruction {
    fn write(&self, writer: &mut impl BufMut) {
        match self {
            Self::Generate => 0u8.write(writer),
            Self::Match => 1u8.write(writer),
            Self::Move(ciphertext) => {
                2u8.write(writer);
                ciphertext.write(writer);
            }
            Self::Settle(signature) => {
                3u8.write(writer);
                signature.write(writer);
            }
        }
    }
}

impl Read for Instruction {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let instruction = match reader.get_u8() {
            0 => Self::Generate,
            1 => Self::Match,
            2 => Self::Move(Ciphertext::read(reader)?),
            3 => Self::Settle(Signature::read(reader)?),
            i => return Err(Error::InvalidEnum(i)),
        };

        Ok(instruction)
    }
}

impl EncodeSize for Instruction {
    fn encode_size(&self) -> usize {
        u8::SIZE
            + match self {
                Self::Generate | Self::Match => 0,
                Self::Move(ciphertext) => ciphertext.encode_size(),
                Self::Settle(signature) => signature.encode_size(),
            }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Creature {
    pub traits: [u8; Digest::SIZE],
}

impl Creature {
    /// Distributes skill points deterministically based on input hash
    /// Ensures health >= MIN_HEALTH_POINTS and other skills >= 1
    /// Total points sum to TOTAL_SKILL_POINTS
    fn distribute_skill_points(digest: &[u8; Digest::SIZE]) -> [u8; SKILLS] {
        // Start with minimum values
        let mut skills = [1u8; SKILLS];
        skills[0] = MIN_HEALTH_POINTS;

        // Calculate remaining points to distribute
        let min_sum: u16 = MIN_HEALTH_POINTS as u16 + ALLOWED_MOVES as u16; // health + 4 other skills at 1 each
        let remaining_points = TOTAL_SKILL_POINTS - min_sum;

        // Use hash bytes to deterministically distribute remaining points
        // First, calculate weights from hash
        let weights: Vec<u16> = digest[0..SKILLS].iter().map(|&b| b as u16 + 1).collect();
        let weight_sum: u16 = weights.iter().sum();

        // Distribute remaining points proportionally
        let mut distributed: u16 = 0;
        for i in 0..SKILLS {
            let additional = (remaining_points * weights[i] / weight_sum) as u8;
            let before = skills[i];
            skills[i] = skills[i].saturating_add(additional);
            let actual_added = (skills[i] - before) as u16;
            distributed += actual_added;
        }

        // Handle any rounding remainder by adding to skills in order
        let mut remainder = remaining_points - distributed;
        let mut iter = 0;
        while remainder > 0 {
            let idx = iter % SKILLS;
            let skill = &mut skills[idx];
            if let Some(new_skill) = skill.checked_add(1) {
                *skill = new_skill;
                remainder = remainder.saturating_sub(1);
            }
            iter += 1;
        }

        skills
    }

    /// Calculate action effectiveness for a given move
    /// Returns (is_defensive, effectiveness)
    fn calculate_action(traits: &[u8], index: u8, multiplier: u8) -> (bool, u8) {
        // If index is out of bounds or 0 (no move), return no action
        // Valid moves are 1-4 (inclusive)
        if index == 0 || index > ALLOWED_MOVES as u8 {
            return (false, 0);
        }

        // Scale effectiveness from 1/2 to full strength
        // Note: traits array starts at index 0, but move indices now start at 1
        let max_effectiveness = traits[index as usize];
        // multiplier ranges from 0 to u8::MAX, we want to map this to 0.5-1.0 of max_effectiveness
        // Formula: min + (multiplier/u8::MAX) * (max - min) where min = max/2
        let min_effectiveness = max_effectiveness / 2;
        let range = max_effectiveness - min_effectiveness;
        let scaled_effectiveness =
            min_effectiveness + ((range as u16 * multiplier as u16) / u8::MAX as u16) as u8;

        // Return scaled effectiveness (move 1 is defensive)
        if index == 1 {
            (true, scaled_effectiveness)
        } else {
            (false, scaled_effectiveness)
        }
    }

    pub fn new(actor: PublicKey, nonce: u64, seed: Signature) -> Self {
        // Compute raw traits from seed
        let mut hasher = Sha256::new();
        hasher.update(actor.as_ref());
        hasher.update(nonce.to_be_bytes().as_ref());
        hasher.update(seed.encode().as_ref());
        let mut traits = hasher.finalize().0;

        // Distribute skill points
        let skills = Self::distribute_skill_points(&traits);
        traits[..SKILLS].copy_from_slice(&skills);
        Self { traits }
    }

    pub fn health(&self) -> u8 {
        self.traits[0]
    }

    pub fn action(&self, index: u8, seed: Signature) -> (bool, u8) {
        // Compute effectiveness
        let mut hasher = Sha256::new();
        hasher.update(self.traits.as_ref());
        hasher.update(seed.encode().as_ref());
        let effectiveness = hasher.finalize().0;

        // Scale effectiveness
        Self::calculate_action(&self.traits, index, effectiveness[0])
    }

    // Get the max effectiveness values for all moves
    // Returns array indexed by move (0 = no-op, 1-4 = actual moves)
    // Each element is the max strength for that move
    pub fn get_move_strengths(&self) -> [u8; TOTAL_MOVES] {
        [
            0,              // Move 0: no-op
            self.traits[1], // Move 1: defense
            self.traits[2], // Move 2: attack 1
            self.traits[3], // Move 3: attack 2
            self.traits[4], // Move 4: attack 3
        ]
    }

    // Get move usage limits based on strength ranking
    // Returns array indexed by move (0 = no-op/unlimited, 1-4 = actual moves)
    // All moves get limited uses inversely proportional to their strength
    pub fn get_move_usage_limits(&self) -> [u8; TOTAL_MOVES] {
        // Extract move strengths
        let strengths = [
            self.traits[1], // Defense
            self.traits[2], // Attack 1
            self.traits[3], // Attack 2
            self.traits[4], // Attack 3
        ];

        // Find the weakest move's strength to use as reference
        let weakest_strength = *strengths.iter().min().unwrap() as u16;

        // Calculate limits for each move
        let mut limits = [0u8; TOTAL_MOVES];
        limits[0] = u8::MAX; // Move 0 (no-op) has unlimited uses

        for (i, &strength) in strengths.iter().enumerate() {
            // All moves get limited uses inversely proportional to their strength
            // Weakest moves get BASE_MOVE_LIMIT uses, stronger moves get fewer
            let limit = (BASE_MOVE_LIMIT * weakest_strength / strength as u16).clamp(1, 20) as u8;
            limits[i + 1] = limit; // +1 because index 0 is no-op
        }

        limits
    }
}

impl Write for Creature {
    fn write(&self, writer: &mut impl BufMut) {
        self.traits.write(writer);
    }
}

impl Read for Creature {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let traits = <[u8; Digest::SIZE]>::read(reader)?;
        Ok(Self { traits })
    }
}

impl FixedSize for Creature {
    const SIZE: usize = Digest::SIZE;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub parent: Digest,

    pub view: View,
    pub height: u64,

    pub transactions: Vec<Transaction>,

    digest: Digest,
}

impl Block {
    fn compute_digest(
        parent: &Digest,
        view: View,
        height: u64,
        transactions: &[Transaction],
    ) -> Digest {
        let mut hasher = Sha256::new();
        hasher.update(parent);
        hasher.update(&view.to_be_bytes());
        hasher.update(&height.to_be_bytes());
        for transaction in transactions {
            hasher.update(&transaction.digest());
        }
        hasher.finalize()
    }

    pub fn new(parent: Digest, view: View, height: u64, transactions: Vec<Transaction>) -> Self {
        assert!(transactions.len() <= MAX_BLOCK_TRANSACTIONS);
        let digest = Self::compute_digest(&parent, view, height, &transactions);
        Self {
            parent,
            view,
            height,
            transactions,
            digest,
        }
    }
}

impl Write for Block {
    fn write(&self, writer: &mut impl BufMut) {
        self.parent.write(writer);
        UInt(self.view).write(writer);
        UInt(self.height).write(writer);
        self.transactions.write(writer);
    }
}

impl Read for Block {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let parent = Digest::read(reader)?;
        let view = UInt::read(reader)?.into();
        let height = UInt::read(reader)?.into();
        let transactions = Vec::<Transaction>::read_cfg(
            reader,
            &(RangeCfg::from(0..=MAX_BLOCK_TRANSACTIONS), ()),
        )?;

        // Pre-compute the digest
        let digest = Self::compute_digest(&parent, view, height, &transactions);
        Ok(Self {
            parent,
            view,
            height,
            transactions,
            digest,
        })
    }
}

impl EncodeSize for Block {
    fn encode_size(&self) -> usize {
        self.parent.encode_size()
            + UInt(self.view).encode_size()
            + UInt(self.height).encode_size()
            + self.transactions.encode_size()
    }
}

impl Digestible for Block {
    type Digest = Digest;

    fn digest(&self) -> Digest {
        self.digest
    }
}

impl Committable for Block {
    type Commitment = Digest;

    fn commitment(&self) -> Digest {
        self.digest
    }
}

impl commonware_consensus::Block for Block {
    fn parent(&self) -> Digest {
        self.parent
    }

    fn height(&self) -> u64 {
        self.height
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notarized {
    pub proof: CNotarization<MinSig, Digest>,
    pub block: Block,
}

impl Notarized {
    pub fn new(proof: CNotarization<MinSig, Digest>, block: Block) -> Self {
        Self { proof, block }
    }

    pub fn verify(&self, namespace: &[u8], identity: &<MinSig as Variant>::Public) -> bool {
        self.proof.verify(namespace, identity)
    }
}

impl Write for Notarized {
    fn write(&self, buf: &mut impl BufMut) {
        self.proof.write(buf);
        self.block.write(buf);
    }
}

impl Read for Notarized {
    type Cfg = ();

    fn read_cfg(buf: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let proof = CNotarization::<MinSig, Digest>::read(buf)?;
        let block = Block::read(buf)?;

        // Ensure the proof is for the block
        if proof.proposal.payload != block.digest() {
            return Err(Error::Invalid(
                "types::Notarized",
                "Proof payload does not match block digest",
            ));
        }
        Ok(Self { proof, block })
    }
}

impl EncodeSize for Notarized {
    fn encode_size(&self) -> usize {
        self.proof.encode_size() + self.block.encode_size()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finalized {
    pub proof: CFinalization<MinSig, Digest>,
    pub block: Block,
}

impl Finalized {
    pub fn new(proof: CFinalization<MinSig, Digest>, block: Block) -> Self {
        Self { proof, block }
    }

    pub fn verify(&self, namespace: &[u8], identity: &<MinSig as Variant>::Public) -> bool {
        self.proof.verify(namespace, identity)
    }
}

impl Write for Finalized {
    fn write(&self, buf: &mut impl BufMut) {
        self.proof.write(buf);
        self.block.write(buf);
    }
}

impl Read for Finalized {
    type Cfg = ();

    fn read_cfg(buf: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let proof = Finalization::read(buf)?;
        let block = Block::read(buf)?;

        // Ensure the proof is for the block
        if proof.proposal.payload != block.digest() {
            return Err(Error::Invalid(
                "types::Finalized",
                "Proof payload does not match block digest",
            ));
        }
        Ok(Self { proof, block })
    }
}

impl EncodeSize for Finalized {
    fn encode_size(&self) -> usize {
        self.proof.encode_size() + self.block.encode_size()
    }
}

/// The leader for a given seed is determined by the modulo of the seed with the number of participants.
pub fn leader_index(seed: &[u8], participants: usize) -> usize {
    modulo(seed, participants as u64) as usize
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stats {
    pub elo: u16,
    pub wins: u32,
    pub losses: u32,
    pub draws: u32,
}

impl Stats {
    pub fn plays(&self) -> u64 {
        self.wins as u64 + self.losses as u64 + self.draws as u64
    }
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            elo: 1000,
            wins: 0,
            losses: 0,
            draws: 0,
        }
    }
}

impl Write for Stats {
    fn write(&self, writer: &mut impl BufMut) {
        self.elo.write(writer);
        self.wins.write(writer);
        self.losses.write(writer);
        self.draws.write(writer);
    }
}

impl Read for Stats {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        Ok(Self {
            elo: u16::read(reader)?,
            wins: u32::read(reader)?,
            losses: u32::read(reader)?,
            draws: u32::read(reader)?,
        })
    }
}

impl FixedSize for Stats {
    const SIZE: usize = u16::SIZE + u32::SIZE * 3;
}

#[derive(Clone, Default, Eq, PartialEq, Debug)]
pub struct Account {
    pub nonce: u64,

    pub creature: Option<Creature>,
    pub battle: Option<Digest>,

    pub stats: Stats,
}

impl Write for Account {
    fn write(&self, writer: &mut impl BufMut) {
        self.nonce.write(writer);
        self.creature.write(writer);
        self.battle.write(writer);
        self.stats.write(writer);
    }
}

impl Read for Account {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let nonce = u64::read(reader)?;
        let creature = Option::<Creature>::read(reader)?;
        let battle = Option::<Digest>::read(reader)?;
        let stats = Stats::read(reader)?;

        Ok(Self {
            nonce,
            creature,
            battle,
            stats,
        })
    }
}

impl EncodeSize for Account {
    fn encode_size(&self) -> usize {
        self.nonce.encode_size()
            + self.creature.encode_size()
            + self.battle.encode_size()
            + self.stats.encode_size()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Leaderboard {
    pub players: Vec<(PublicKey, Stats)>,
}

impl Leaderboard {
    pub fn update(&mut self, player: PublicKey, stats: Stats) {
        // Update player (if they already exist)
        if let Some(index) = self.players.iter().position(|(p, _)| p == &player) {
            // If an update drops a players score considerably, they may no longer actually be in the top 10
            // but we don't have a reference to the other scores, so we can't replace them. The next player
            // that settles with a score higher will replace them.
            self.players[index] = (player, stats);
        } else {
            // Add the player to the leaderboard
            self.players.push((player, stats));
        }

        // Sort the leaderboard
        self.players.sort_by(|a, b| b.1.elo.cmp(&a.1.elo));

        // Keep only the top 10 players
        self.players.truncate(10);
    }
}

impl Write for Leaderboard {
    fn write(&self, writer: &mut impl BufMut) {
        self.players.write(writer);
    }
}

impl Read for Leaderboard {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        Ok(Self {
            players: Vec::<_>::read_range(reader, 0..=10)?,
        })
    }
}

impl EncodeSize for Leaderboard {
    fn encode_size(&self) -> usize {
        self.players.encode_size()
    }
}

#[derive(Hash, Eq, PartialEq, Ord, PartialOrd, Clone)]
pub enum Key {
    Account(PublicKey),
    Lobby,
    Battle(Digest),
    Leaderboard,
}

impl Write for Key {
    fn write(&self, writer: &mut impl BufMut) {
        match self {
            Self::Account(account) => {
                0u8.write(writer);
                account.write(writer);
            }
            Self::Lobby => 1u8.write(writer),
            Self::Battle(battle) => {
                2u8.write(writer);
                battle.write(writer);
            }
            Self::Leaderboard => 3u8.write(writer),
        }
    }
}

impl Read for Key {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let key = match reader.get_u8() {
            0 => Self::Account(PublicKey::read(reader)?),
            1 => Self::Lobby,
            2 => Self::Battle(Digest::read(reader)?),
            3 => Self::Leaderboard,
            i => return Err(Error::InvalidEnum(i)),
        };

        Ok(key)
    }
}

impl EncodeSize for Key {
    fn encode_size(&self) -> usize {
        u8::SIZE
            + match self {
                Self::Account(_) => PublicKey::SIZE,
                Self::Lobby => 0,
                Self::Battle(_) => Digest::SIZE,
                Self::Leaderboard => 0,
            }
    }
}

#[derive(Clone, Eq, PartialEq, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Value {
    Account(Account),
    Lobby {
        expiry: u64,

        players: BTreeSet<PublicKey>,
    },
    Battle {
        expiry: u64,
        round: u8,

        player_a: PublicKey,
        player_a_max_health: u8,
        player_a_health: u8,
        player_a_pending: Option<Ciphertext<MinSig>>,
        player_a_move_counts: [u8; TOTAL_MOVES],

        player_b: PublicKey,
        player_b_max_health: u8,
        player_b_health: u8,
        player_b_pending: Option<Ciphertext<MinSig>>,
        player_b_move_counts: [u8; TOTAL_MOVES],
    },
    Commit {
        height: u64,
        start: u64,
    },
    Leaderboard(Leaderboard),
}

impl Write for Value {
    fn write(&self, writer: &mut impl BufMut) {
        match self {
            Self::Account(account) => {
                0u8.write(writer);
                account.write(writer);
            }
            Self::Lobby { expiry, players } => {
                1u8.write(writer);
                expiry.write(writer);
                players.write(writer);
            }
            Self::Battle {
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
                2u8.write(writer);
                expiry.write(writer);
                round.write(writer);
                player_a.write(writer);
                player_a_max_health.write(writer);
                player_a_health.write(writer);
                player_a_pending.write(writer);
                player_a_move_counts.write(writer);
                player_b.write(writer);
                player_b_max_health.write(writer);
                player_b_health.write(writer);
                player_b_pending.write(writer);
                player_b_move_counts.write(writer);
            }
            Self::Commit { height, start } => {
                3u8.write(writer);
                height.write(writer);
                start.write(writer);
            }
            Self::Leaderboard(leaderboard) => {
                4u8.write(writer);
                leaderboard.write(writer);
            }
        }
    }
}

impl Read for Value {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let value = match reader.get_u8() {
            0 => Self::Account(Account::read(reader)?),
            1 => Self::Lobby {
                expiry: u64::read(reader)?,
                players: BTreeSet::<PublicKey>::read_cfg(
                    reader,
                    &(RangeCfg::from(0..=MAX_LOBBY_SIZE), ()),
                )?,
            },
            2 => Self::Battle {
                expiry: u64::read(reader)?,
                round: u8::read(reader)?,
                player_a: PublicKey::read(reader)?,
                player_a_max_health: u8::read(reader)?,
                player_a_health: u8::read(reader)?,
                player_a_pending: Option::<Ciphertext<MinSig>>::read(reader)?,
                player_a_move_counts: <[u8; TOTAL_MOVES]>::read(reader)?,
                player_b: PublicKey::read(reader)?,
                player_b_max_health: u8::read(reader)?,
                player_b_health: u8::read(reader)?,
                player_b_pending: Option::<Ciphertext<MinSig>>::read(reader)?,
                player_b_move_counts: <[u8; TOTAL_MOVES]>::read(reader)?,
            },
            3 => Self::Commit {
                height: u64::read(reader)?,
                start: u64::read(reader)?,
            },
            4 => Self::Leaderboard(Leaderboard::read(reader)?),
            i => return Err(Error::InvalidEnum(i)),
        };

        Ok(value)
    }
}

impl EncodeSize for Value {
    fn encode_size(&self) -> usize {
        u8::SIZE
            + match self {
                Self::Account(account) => account.encode_size(),
                Self::Lobby { expiry, players } => expiry.encode_size() + players.encode_size(),
                Self::Battle {
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
                    expiry.encode_size()
                        + round.encode_size()
                        + player_a.encode_size()
                        + player_a_max_health.encode_size()
                        + player_a_health.encode_size()
                        + player_a_pending.encode_size()
                        + player_a_move_counts.encode_size()
                        + player_b.encode_size()
                        + player_b_max_health.encode_size()
                        + player_b_health.encode_size()
                        + player_b_pending.encode_size()
                        + player_b_move_counts.encode_size()
                }
                Self::Commit { height, start } => height.encode_size() + start.encode_size(),
                Self::Leaderboard(leaderboard) => leaderboard.encode_size(),
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    PlayerA,
    PlayerB,
    Draw,
}

impl Write for Outcome {
    fn write(&self, writer: &mut impl BufMut) {
        match self {
            Self::PlayerA => 0u8.write(writer),
            Self::PlayerB => 1u8.write(writer),
            Self::Draw => 2u8.write(writer),
        }
    }
}

impl Read for Outcome {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let outcome = match reader.get_u8() {
            0 => Self::PlayerA,
            1 => Self::PlayerB,
            2 => Self::Draw,
            i => return Err(Error::InvalidEnum(i)),
        };

        Ok(outcome)
    }
}

impl FixedSize for Outcome {
    const SIZE: usize = u8::SIZE;
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::large_enum_variant)]
pub enum Event {
    Generated {
        account: PublicKey,
        creature: Creature,
    },
    Matched {
        battle: Digest,
        expiry: u64,
        player_a: PublicKey,
        player_a_creature: Creature,
        player_a_stats: Stats,
        player_b: PublicKey,
        player_b_creature: Creature,
        player_b_stats: Stats,
    },
    Locked {
        battle: Digest,
        round: u8,
        locker: PublicKey,
        observer: PublicKey,
        ciphertext: Ciphertext<MinSig>,
    },
    Moved {
        battle: Digest,
        round: u8,
        expiry: u64,
        player_a: PublicKey,
        player_a_health: u8,
        player_a_move: u8,
        player_a_move_counts: [u8; TOTAL_MOVES],
        player_a_power: u8,
        player_b: PublicKey,
        player_b_health: u8,
        player_b_move: u8,
        player_b_move_counts: [u8; TOTAL_MOVES],
        player_b_power: u8,
    },
    Settled {
        battle: Digest,
        round: u8,
        player_a: PublicKey,
        player_a_old: Stats,
        player_a_new: Stats,
        player_b: PublicKey,
        player_b_old: Stats,
        player_b_new: Stats,
        outcome: Outcome,
        leaderboard: Leaderboard,
    },
}

impl Write for Event {
    fn write(&self, writer: &mut impl BufMut) {
        match self {
            Self::Generated { account, creature } => {
                0u8.write(writer);
                account.write(writer);
                creature.write(writer);
            }
            Self::Matched {
                battle,
                expiry,
                player_a,
                player_a_creature,
                player_a_stats,
                player_b,
                player_b_creature,
                player_b_stats,
            } => {
                1u8.write(writer);
                battle.write(writer);
                expiry.write(writer);
                player_a.write(writer);
                player_a_creature.write(writer);
                player_a_stats.write(writer);
                player_b.write(writer);
                player_b_creature.write(writer);
                player_b_stats.write(writer);
            }
            Self::Locked {
                battle,
                round,
                locker,
                observer,
                ciphertext,
            } => {
                2u8.write(writer);
                battle.write(writer);
                round.write(writer);
                locker.write(writer);
                observer.write(writer);
                ciphertext.write(writer);
            }
            Self::Moved {
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
                3u8.write(writer);
                battle.write(writer);
                round.write(writer);
                expiry.write(writer);
                player_a.write(writer);
                player_a_health.write(writer);
                player_a_move.write(writer);
                player_a_move_counts.write(writer);
                player_a_power.write(writer);
                player_b.write(writer);
                player_b_health.write(writer);
                player_b_move.write(writer);
                player_b_move_counts.write(writer);
                player_b_power.write(writer);
            }
            Self::Settled {
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
                4u8.write(writer);
                battle.write(writer);
                round.write(writer);
                player_a.write(writer);
                player_a_old.write(writer);
                player_a_new.write(writer);
                player_b.write(writer);
                player_b_old.write(writer);
                player_b_new.write(writer);
                outcome.write(writer);
                leaderboard.write(writer);
            }
        }
    }
}

impl Read for Event {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let event = match reader.get_u8() {
            0 => Self::Generated {
                account: PublicKey::read(reader)?,
                creature: Creature::read(reader)?,
            },
            1 => Self::Matched {
                battle: Digest::read(reader)?,
                expiry: u64::read(reader)?,
                player_a: PublicKey::read(reader)?,
                player_a_creature: Creature::read(reader)?,
                player_a_stats: Stats::read(reader)?,
                player_b: PublicKey::read(reader)?,
                player_b_creature: Creature::read(reader)?,
                player_b_stats: Stats::read(reader)?,
            },
            2 => Self::Locked {
                battle: Digest::read(reader)?,
                round: u8::read(reader)?,
                locker: PublicKey::read(reader)?,
                observer: PublicKey::read(reader)?,
                ciphertext: Ciphertext::<MinSig>::read(reader)?,
            },
            3 => Self::Moved {
                battle: Digest::read(reader)?,
                round: u8::read(reader)?,
                expiry: u64::read(reader)?,
                player_a: PublicKey::read(reader)?,
                player_a_health: u8::read(reader)?,
                player_a_move: u8::read(reader)?,
                player_a_move_counts: <[u8; TOTAL_MOVES]>::read(reader)?,
                player_a_power: u8::read(reader)?,
                player_b: PublicKey::read(reader)?,
                player_b_health: u8::read(reader)?,
                player_b_move: u8::read(reader)?,
                player_b_move_counts: <[u8; TOTAL_MOVES]>::read(reader)?,
                player_b_power: u8::read(reader)?,
            },
            4 => Self::Settled {
                battle: Digest::read(reader)?,
                round: u8::read(reader)?,
                player_a: PublicKey::read(reader)?,
                player_a_old: Stats::read(reader)?,
                player_a_new: Stats::read(reader)?,
                player_b: PublicKey::read(reader)?,
                player_b_old: Stats::read(reader)?,
                player_b_new: Stats::read(reader)?,
                outcome: Outcome::read(reader)?,
                leaderboard: Leaderboard::read(reader)?,
            },
            i => return Err(Error::InvalidEnum(i)),
        };

        Ok(event)
    }
}

impl EncodeSize for Event {
    fn encode_size(&self) -> usize {
        u8::SIZE
            + match self {
                Self::Generated { account, creature } => {
                    account.encode_size() + creature.encode_size()
                }
                Self::Matched {
                    battle,
                    expiry,
                    player_a,
                    player_a_creature,
                    player_a_stats,
                    player_b,
                    player_b_creature,
                    player_b_stats,
                } => {
                    battle.encode_size()
                        + expiry.encode_size()
                        + player_a.encode_size()
                        + player_a_creature.encode_size()
                        + player_a_stats.encode_size()
                        + player_b.encode_size()
                        + player_b_creature.encode_size()
                        + player_b_stats.encode_size()
                }
                Self::Locked {
                    battle,
                    round,
                    locker,
                    observer,
                    ciphertext,
                } => {
                    battle.encode_size()
                        + round.encode_size()
                        + locker.encode_size()
                        + observer.encode_size()
                        + ciphertext.encode_size()
                }
                Self::Moved {
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
                    battle.encode_size()
                        + round.encode_size()
                        + expiry.encode_size()
                        + player_a.encode_size()
                        + player_a_health.encode_size()
                        + player_a_move.encode_size()
                        + player_a_move_counts.encode_size()
                        + player_a_power.encode_size()
                        + player_b.encode_size()
                        + player_b_health.encode_size()
                        + player_b_move.encode_size()
                        + player_b_move_counts.encode_size()
                        + player_b_power.encode_size()
                }
                Self::Settled {
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
                    battle.encode_size()
                        + round.encode_size()
                        + player_a.encode_size()
                        + player_a_old.encode_size()
                        + player_a_new.encode_size()
                        + player_b.encode_size()
                        + player_b_old.encode_size()
                        + player_b_new.encode_size()
                        + outcome.encode_size()
                        + leaderboard.encode_size()
                }
            }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Output {
    Event(Event),
    Transaction(Transaction),
    Commit { height: u64, start: u64 },
}

impl Write for Output {
    fn write(&self, writer: &mut impl BufMut) {
        match self {
            Self::Event(event) => {
                0u8.write(writer);
                event.write(writer);
            }
            Self::Transaction(transaction) => {
                1u8.write(writer);
                transaction.write(writer);
            }
            Self::Commit { height, start } => {
                2u8.write(writer);
                height.write(writer);
                start.write(writer);
            }
        }
    }
}

impl Read for Output {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let kind = u8::read(reader)?;
        match kind {
            0 => Ok(Self::Event(Event::read(reader)?)),
            1 => Ok(Self::Transaction(Transaction::read(reader)?)),
            2 => Ok(Self::Commit {
                height: u64::read(reader)?,
                start: u64::read(reader)?,
            }),
            _ => Err(Error::InvalidEnum(kind)),
        }
    }
}

impl EncodeSize for Output {
    fn encode_size(&self) -> usize {
        1 + match self {
            Self::Event(event) => event.encode_size(),
            Self::Transaction(transaction) => transaction.encode_size(),
            Self::Commit { height, start } => height.encode_size() + start.encode_size(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub view: View,
    pub height: u64,
    pub block_digest: Digest,
    pub state_root: Digest,
    pub state_start_op: u64,
    pub state_end_op: u64,
    pub events_root: Digest,
    pub events_start_op: u64,
    pub events_end_op: u64,
}

impl Progress {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        view: View,
        height: u64,
        block_digest: Digest,
        state_root: Digest,
        state_start_op: u64,
        state_end_op: u64,
        events_root: Digest,
        events_start_op: u64,
        events_end_op: u64,
    ) -> Self {
        Self {
            view,
            height,
            block_digest,
            state_root,
            state_start_op,
            state_end_op,
            events_root,
            events_start_op,
            events_end_op,
        }
    }
}

impl Write for Progress {
    fn write(&self, writer: &mut impl BufMut) {
        self.view.write(writer);
        self.height.write(writer);
        self.block_digest.write(writer);
        self.state_root.write(writer);
        self.state_start_op.write(writer);
        self.state_end_op.write(writer);
        self.events_root.write(writer);
        self.events_start_op.write(writer);
        self.events_end_op.write(writer);
    }
}

impl Read for Progress {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        Ok(Self {
            view: View::read(reader)?,
            height: u64::read(reader)?,
            block_digest: Digest::read(reader)?,
            state_root: Digest::read(reader)?,
            state_start_op: u64::read(reader)?,
            state_end_op: u64::read(reader)?,
            events_root: Digest::read(reader)?,
            events_start_op: u64::read(reader)?,
            events_end_op: u64::read(reader)?,
        })
    }
}

impl FixedSize for Progress {
    const SIZE: usize = View::SIZE
        + u64::SIZE
        + Digest::SIZE
        + Digest::SIZE
        + u64::SIZE
        + u64::SIZE
        + Digest::SIZE
        + u64::SIZE
        + u64::SIZE;
}

impl Digestible for Progress {
    type Digest = Digest;

    fn digest(&self) -> Digest {
        Sha256::hash(&self.encode())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_distribute_skill_points() {
        let test_digests = [
            [0u8; Digest::SIZE],
            [255u8; Digest::SIZE],
            [128u8; Digest::SIZE],
        ];

        for digest in test_digests {
            let skills = Creature::distribute_skill_points(&digest);
            assert!(skills[0] >= MIN_HEALTH_POINTS);
            for skill in skills.iter().skip(1) {
                assert!(*skill >= 1);
            }
            let sum: u16 = skills.iter().map(|&s| s as u16).sum();
            assert_eq!(sum, TOTAL_SKILL_POINTS);
        }
    }

    #[test]
    fn test_distribute_skill_points_deterministic() {
        let digest = [42u8; Digest::SIZE];
        let skills1 = Creature::distribute_skill_points(&digest);
        let skills2 = Creature::distribute_skill_points(&digest);
        assert_eq!(skills1, skills2, "Not deterministic");
    }

    #[test]
    fn test_distribute_skill_points_unbalanced_health() {
        let mut digest = [0u8; Digest::SIZE];
        digest[0] = 255;
        digest[1] = 0;
        digest[2] = 0;
        digest[3] = 0;
        digest[4] = 0;
        let skills = Creature::distribute_skill_points(&digest);

        assert!(skills[0] >= MIN_HEALTH_POINTS);
        for skill in skills.iter().skip(1) {
            assert!(*skill >= 1);
        }
        let sum: u16 = skills.iter().map(|&s| s as u16).sum();
        assert_eq!(sum, TOTAL_SKILL_POINTS);
    }

    #[test]
    fn test_distribute_skill_points_unbalanced_attack() {
        let mut digest = [0u8; Digest::SIZE];
        digest[0] = 0;
        digest[1] = 0;
        digest[2] = 0;
        digest[3] = 0;
        digest[4] = 255;
        let skills = Creature::distribute_skill_points(&digest);

        assert!(skills[0] >= MIN_HEALTH_POINTS);
        for skill in skills.iter().skip(1) {
            assert!(*skill >= 1);
        }
        let sum: u16 = skills.iter().map(|&s| s as u16).sum();
        assert_eq!(sum, TOTAL_SKILL_POINTS);
    }

    #[test]
    fn test_calculate_action() {
        // Test traits with different strengths
        let traits = {
            let mut t = [0u8; Digest::SIZE];
            t[0] = 100; // health
            t[1] = 50; // defense
            t[2] = 60; // attack 1
            t[3] = 70; // attack 2
            t[4] = 80; // attack 3
            t
        };

        // Test invalid indices
        assert_eq!(Creature::calculate_action(&traits, 0, 128), (false, 0));
        assert_eq!(Creature::calculate_action(&traits, 5, 128), (false, 0));

        // Test defensive move (index 1)
        let (is_defensive, effectiveness) = Creature::calculate_action(&traits, 1, 128);
        assert!(is_defensive);
        assert!(effectiveness >= 25);
        assert!(effectiveness <= 50);

        // Test attack moves (indices 2-4)
        for i in 2..=4 {
            let (is_defensive, effectiveness) = Creature::calculate_action(&traits, i, 128);
            assert!(!is_defensive);
            let max_eff = traits[i as usize];
            assert!(effectiveness >= max_eff / 2 && effectiveness <= max_eff);
        }

        let (_, min_eff) = Creature::calculate_action(&traits, 2, 0);
        let (_, max_eff) = Creature::calculate_action(&traits, 2, 255);

        assert_eq!(min_eff, traits[2] / 2); // Minimum effectiveness
        assert_eq!(max_eff, traits[2]); // Maximum effectiveness
    }

    #[test]
    fn test_get_move_usage_limits() {
        // Create a creature with known traits
        let mut creature = Creature {
            traits: [0u8; Digest::SIZE],
        };
        creature.traits[0] = 100; // health
        creature.traits[1] = 20; // defense (weakest)
        creature.traits[2] = 40; // attack 1
        creature.traits[3] = 60; // attack 2
        creature.traits[4] = 80; // attack 3 (strongest)

        let limits = creature.get_move_usage_limits();

        // Move 0 (no-op) should have unlimited uses
        assert_eq!(limits[0], u8::MAX);

        // All actual moves (1-4) should have limited uses
        for limit in limits.iter().skip(1) {
            assert!(*limit >= 1 && *limit <= 20);
        }

        // Defense (weakest, move 1) should have most uses (BASE_MOVE_LIMIT)
        assert_eq!(limits[1], 15);

        // Stronger moves should have fewer uses
        assert!(limits[1] > limits[2]); // defense > attack 1
        assert!(limits[2] > limits[3]); // attack 1 > attack 2
        assert!(limits[3] > limits[4]); // attack 2 > attack 3
    }

    #[test]
    fn test_get_move_usage_limits_equal_strengths() {
        // Test with equal strength moves
        let mut creature = Creature {
            traits: [0u8; Digest::SIZE],
        };
        creature.traits[0] = 100; // health
        creature.traits[1] = 50; // all moves equal strength
        creature.traits[2] = 50;
        creature.traits[3] = 50;
        creature.traits[4] = 50;

        let limits = creature.get_move_usage_limits();

        // Move 0 (no-op) should have unlimited uses
        assert_eq!(limits[0], u8::MAX);

        // All actual moves (1-4) should have the same limit (BASE_MOVE_LIMIT)
        for limit in limits.iter().skip(1) {
            assert_eq!(*limit, 15);
        }
    }

    #[test]
    fn test_get_move_usage_limits_edge_cases() {
        // Test with extreme differences in strength
        let mut creature = Creature {
            traits: [0u8; Digest::SIZE],
        };
        creature.traits[0] = 100; // health
        creature.traits[1] = 10; // defense (weakest)
        creature.traits[2] = 255; // attack 1 (very strong)
        creature.traits[3] = 128; // attack 2 (medium)
        creature.traits[4] = 200; // attack 3 (strong)

        let limits = creature.get_move_usage_limits();

        // Move 0 (no-op) should have unlimited uses
        assert_eq!(limits[0], u8::MAX);

        // All actual moves (1-4) should be clamped between 1 and 20
        for limit in limits.iter().skip(1) {
            assert!(*limit >= 1);
            assert!(*limit <= 20);
        }

        // Defense (weakest, move 1) should have BASE_MOVE_LIMIT
        assert_eq!(limits[1], 15);

        // Very strong moves should be clamped at minimum
        assert_eq!(limits[2], 1); // attack 1 (move 2) is so strong it hits the minimum

        // Check relative ordering based on strength
        // Calculation: 15 * 10 / 255 = 0.58, clamped to 1
        // Calculation: 15 * 10 / 128 = 1.17, clamped to 1
        // Calculation: 15 * 10 / 200 = 0.75, clamped to 1
        // All strong attacks get clamped to minimum
        assert_eq!(limits[2], 1); // attack 1 (move 2)
        assert_eq!(limits[3], 1); // attack 2 (move 3)
        assert_eq!(limits[4], 1); // attack 3 (move 4)
        assert_eq!(limits[2], 1); // attack 2
        assert_eq!(limits[3], 1); // attack 3
    }
}
