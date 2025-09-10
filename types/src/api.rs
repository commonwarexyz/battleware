use crate::{
    execution::{Output, Progress, Seed, Transaction, Value},
    Identity, NAMESPACE,
};
use bytes::{Buf, BufMut};
use commonware_codec::{EncodeSize, Error, Read, ReadExt, ReadRangeExt, Write};
use commonware_consensus::aggregation::types::Certificate;
use commonware_cryptography::{
    bls12381::primitives::variant::MinSig, ed25519::PublicKey, sha256::Digest, Digestible, Sha256,
};
use commonware_storage::{
    adb::{verify::verify_proof_and_extract_digests, verify_multi_proof, verify_proof},
    mmr::{hasher::Standard, verification::Proof},
    store::operation::{Keyless, Variable},
};

/// Maximum number of transactions that can be submitted in a single submission
pub const MAX_SUBMISSION_TRANSACTIONS: usize = 128;

pub enum Query {
    Latest,
    Index(u64),
}

impl Write for Query {
    fn write(&self, writer: &mut impl BufMut) {
        match self {
            Query::Latest => 0u8.write(writer),
            Query::Index(index) => {
                1u8.write(writer);
                index.write(writer);
            }
        }
    }
}

impl Read for Query {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let kind = u8::read(reader)?;
        match kind {
            0 => Ok(Query::Latest),
            1 => Ok(Query::Index(u64::read(reader)?)),
            _ => Err(Error::InvalidEnum(kind)),
        }
    }
}

impl EncodeSize for Query {
    fn encode_size(&self) -> usize {
        1 + match self {
            Query::Latest => 0,
            Query::Index(index) => index.encode_size(),
        }
    }
}

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Summary {
    pub progress: Progress,
    pub certificate: Certificate<MinSig, Digest>,
    pub state_proof: Proof<Digest>,
    pub state_proof_ops: Vec<Variable<Digest, Value>>,
    pub events_proof: Proof<Digest>,
    pub events_proof_ops: Vec<Keyless<Output>>,
}

impl Summary {
    /// Verify the summary and return the digests from both state and events proofs.
    /// Returns (state_digests, events_digests) on success.
    #[allow(clippy::type_complexity)]
    pub fn verify(&self, identity: &Identity) -> Option<(Vec<(u64, Digest)>, Vec<(u64, Digest)>)> {
        // Verify the signature
        if !self.certificate.verify(NAMESPACE, identity) {
            return None;
        }
        if self.progress.digest() != self.certificate.item.digest {
            return None;
        }

        // Verify the state proof
        if self.progress.state_start_op + self.state_proof_ops.len() as u64
            != self.progress.state_end_op
        {
            return None;
        }
        let mut hasher = Standard::<Sha256>::new();
        let Ok(state_proof_digests) = verify_proof_and_extract_digests(
            &mut hasher,
            &self.state_proof,
            self.progress.state_start_op,
            &self.state_proof_ops,
            &self.progress.state_root,
        ) else {
            return None;
        };

        // Verify the events proof and extract digests
        if self.progress.events_start_op + self.events_proof_ops.len() as u64
            != self.progress.events_end_op
        {
            return None;
        }
        let Ok(events_proof_digests) = verify_proof_and_extract_digests(
            &mut hasher,
            &self.events_proof,
            self.progress.events_start_op,
            &self.events_proof_ops,
            &self.progress.events_root,
        ) else {
            return None;
        };

        Some((state_proof_digests, events_proof_digests))
    }
}

impl Write for Summary {
    fn write(&self, writer: &mut impl BufMut) {
        self.progress.write(writer);
        self.certificate.write(writer);
        self.state_proof.write(writer);
        self.state_proof_ops.write(writer);
        self.events_proof.write(writer);
        self.events_proof_ops.write(writer);
    }
}

impl Read for Summary {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let progress = Progress::read(reader)?;
        let certificate = Certificate::read(reader)?;
        let state_proof = Proof::read_cfg(reader, &500)?;
        let state_proof_ops = Vec::read_range(reader, 0..=500)?;
        let events_proof = Proof::read_cfg(reader, &500)?;
        let events_proof_ops = Vec::read_range(reader, 0..=500)?;
        Ok(Self {
            progress,
            certificate,
            state_proof,
            state_proof_ops,
            events_proof,
            events_proof_ops,
        })
    }
}

impl EncodeSize for Summary {
    fn encode_size(&self) -> usize {
        self.progress.encode_size()
            + self.certificate.encode_size()
            + self.state_proof.encode_size()
            + self.state_proof_ops.encode_size()
            + self.events_proof.encode_size()
            + self.events_proof_ops.encode_size()
    }
}

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Events {
    pub progress: Progress,
    pub certificate: Certificate<MinSig, Digest>,
    pub events_proof: Proof<Digest>,
    pub events_proof_ops: Vec<Keyless<Output>>,
}

impl Events {
    pub fn verify(&self, identity: &Identity) -> bool {
        // Verify the signature
        if !self.certificate.verify(NAMESPACE, identity) {
            return false;
        }
        if self.progress.digest() != self.certificate.item.digest {
            return false;
        }

        // Verify the events proof
        if self.progress.events_start_op + self.events_proof_ops.len() as u64
            != self.progress.events_end_op
        {
            return false;
        }
        let mut hasher = Standard::<Sha256>::new();
        verify_proof(
            &mut hasher,
            &self.events_proof,
            self.progress.events_start_op,
            &self.events_proof_ops,
            &self.progress.events_root,
        )
    }
}
impl Write for Events {
    fn write(&self, writer: &mut impl BufMut) {
        self.progress.write(writer);
        self.certificate.write(writer);
        self.events_proof.write(writer);
        self.events_proof_ops.write(writer);
    }
}

impl Read for Events {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let progress = Progress::read(reader)?;
        let certificate = Certificate::read(reader)?;
        let events_proof = Proof::read_cfg(reader, &500)?;
        let events_proof_ops = Vec::read_range(reader, 0..=500)?;
        Ok(Self {
            progress,
            certificate,
            events_proof,
            events_proof_ops,
        })
    }
}

impl EncodeSize for Events {
    fn encode_size(&self) -> usize {
        self.progress.encode_size()
            + self.certificate.encode_size()
            + self.events_proof.encode_size()
            + self.events_proof_ops.encode_size()
    }
}

pub struct Lookup {
    pub progress: Progress,
    pub certificate: Certificate<MinSig, Digest>,
    pub proof: Proof<Digest>,
    pub location: u64,
    pub operation: Variable<Digest, Value>,
}

impl Lookup {
    pub fn verify(&self, identity: &Identity) -> bool {
        // Verify the signature
        if !self.certificate.verify(NAMESPACE, identity) {
            return false;
        }
        if self.progress.digest() != self.certificate.item.digest {
            return false;
        }

        // Verify the proof
        let mut hasher = Standard::<Sha256>::new();
        verify_proof(
            &mut hasher,
            &self.proof,
            self.location,
            std::slice::from_ref(&self.operation),
            &self.progress.state_root,
        )
    }
}

impl Write for Lookup {
    fn write(&self, writer: &mut impl BufMut) {
        self.progress.write(writer);
        self.certificate.write(writer);
        self.proof.write(writer);
        self.location.write(writer);
        self.operation.write(writer);
    }
}

impl Read for Lookup {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let progress = Progress::read(reader)?;
        let certificate = Certificate::read(reader)?;
        let proof = Proof::read_cfg(reader, &500)?;
        let location = u64::read(reader)?;
        let operation = Variable::read(reader)?;
        Ok(Self {
            progress,
            certificate,
            proof,
            location,
            operation,
        })
    }
}

impl EncodeSize for Lookup {
    fn encode_size(&self) -> usize {
        self.progress.encode_size()
            + self.certificate.encode_size()
            + self.proof.encode_size()
            + self.location.encode_size()
            + self.operation.encode_size()
    }
}

#[derive(Clone, Debug)]
pub struct FilteredEvents {
    pub progress: Progress,
    pub certificate: Certificate<MinSig, Digest>,
    pub events_proof: Proof<Digest>,
    pub events_proof_ops: Vec<(u64, Keyless<Output>)>,
}

impl FilteredEvents {
    pub fn verify(&self, identity: &Identity) -> bool {
        // Verify the signature
        if !self.certificate.verify(NAMESPACE, identity) {
            return false;
        }
        if self.progress.digest() != self.certificate.item.digest {
            return false;
        }

        // Ensure all operations are within the range of the events proof
        for (loc, _) in &self.events_proof_ops {
            if *loc < self.progress.events_start_op || *loc > self.progress.events_end_op {
                return false;
            }
        }

        // Verify the multi-proof for the filtered operations
        let mut hasher = Standard::<Sha256>::new();
        verify_multi_proof(
            &mut hasher,
            &self.events_proof,
            &self.events_proof_ops,
            &self.progress.events_root,
        )
    }
}

impl Write for FilteredEvents {
    fn write(&self, writer: &mut impl BufMut) {
        self.progress.write(writer);
        self.certificate.write(writer);
        self.events_proof.write(writer);
        self.events_proof_ops.write(writer);
    }
}

impl Read for FilteredEvents {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let progress = Progress::read(reader)?;
        let certificate = Certificate::read(reader)?;
        let events_proof = Proof::read_cfg(reader, &500)?;
        let events_proof_ops = Vec::read_range(reader, 0..=500)?;
        Ok(Self {
            progress,
            certificate,
            events_proof,
            events_proof_ops,
        })
    }
}

impl EncodeSize for FilteredEvents {
    fn encode_size(&self) -> usize {
        self.progress.encode_size()
            + self.certificate.encode_size()
            + self.events_proof.encode_size()
            + self.events_proof_ops.encode_size()
    }
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Update {
    Seed(Seed),
    Events(Events),
    FilteredEvents(FilteredEvents),
}

impl Write for Update {
    fn write(&self, writer: &mut impl BufMut) {
        match self {
            Update::Seed(seed) => {
                0u8.write(writer);
                seed.write(writer);
            }
            Update::Events(events) => {
                1u8.write(writer);
                events.write(writer);
            }
            Update::FilteredEvents(events) => {
                2u8.write(writer);
                events.write(writer);
            }
        }
    }
}

impl Read for Update {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let kind = u8::read(reader)?;
        match kind {
            0 => Ok(Update::Seed(Seed::read(reader)?)),
            1 => Ok(Update::Events(Events::read(reader)?)),
            2 => Ok(Update::FilteredEvents(FilteredEvents::read(reader)?)),
            _ => Err(Error::InvalidEnum(kind)),
        }
    }
}

impl EncodeSize for Update {
    fn encode_size(&self) -> usize {
        1 + match self {
            Update::Seed(seed) => seed.encode_size(),
            Update::Events(events) => events.encode_size(),
            Update::FilteredEvents(events) => events.encode_size(),
        }
    }
}

#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Submission {
    Seed(Seed),
    Transactions(Vec<Transaction>),
    Summary(Summary),
}

impl Write for Submission {
    fn write(&self, writer: &mut impl BufMut) {
        match self {
            Submission::Seed(seed) => {
                0u8.write(writer);
                seed.write(writer);
            }
            Submission::Transactions(txs) => {
                1u8.write(writer);
                txs.write(writer);
            }
            Submission::Summary(summary) => {
                2u8.write(writer);
                summary.write(writer);
            }
        }
    }
}

impl Read for Submission {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let kind = u8::read(reader)?;
        match kind {
            0 => Ok(Submission::Seed(Seed::read(reader)?)),
            1 => Ok(Submission::Transactions(Vec::read_range(
                reader,
                1..=MAX_SUBMISSION_TRANSACTIONS,
            )?)),
            2 => Ok(Submission::Summary(Summary::read(reader)?)),
            _ => Err(Error::InvalidEnum(kind)),
        }
    }
}

impl EncodeSize for Submission {
    fn encode_size(&self) -> usize {
        1 + match self {
            Submission::Seed(seed) => seed.encode_size(),
            Submission::Transactions(txs) => txs.encode_size(),
            Submission::Summary(summary) => summary.encode_size(),
        }
    }
}

/// Subscription filter for updates stream
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum UpdatesFilter {
    /// Subscribe to all events
    All,
    /// Subscribe to events for a specific account
    Account(PublicKey),
}

impl Write for UpdatesFilter {
    fn write(&self, writer: &mut impl BufMut) {
        match self {
            UpdatesFilter::All => 0u8.write(writer),
            UpdatesFilter::Account(key) => {
                1u8.write(writer);
                key.write(writer);
            }
        }
    }
}

impl Read for UpdatesFilter {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let kind = u8::read(reader)?;
        match kind {
            0 => Ok(UpdatesFilter::All),
            1 => Ok(UpdatesFilter::Account(PublicKey::read(reader)?)),
            _ => Err(Error::InvalidEnum(kind)),
        }
    }
}

impl EncodeSize for UpdatesFilter {
    fn encode_size(&self) -> usize {
        1 + match self {
            UpdatesFilter::All => 0,
            UpdatesFilter::Account(key) => key.encode_size(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Pending {
    pub transactions: Vec<Transaction>,
}

impl Write for Pending {
    fn write(&self, writer: &mut impl BufMut) {
        self.transactions.write(writer);
    }
}

impl Read for Pending {
    type Cfg = ();

    fn read_cfg(reader: &mut impl Buf, _: &Self::Cfg) -> Result<Self, Error> {
        let transactions = Vec::<Transaction>::read_range(reader, 1..=MAX_SUBMISSION_TRANSACTIONS)?;
        Ok(Self { transactions })
    }
}

impl EncodeSize for Pending {
    fn encode_size(&self) -> usize {
        self.transactions.encode_size()
    }
}
