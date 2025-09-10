use battleware_types::execution::Transaction;
use commonware_cryptography::{ed25519::PublicKey, sha256::Digest, Digestible};
use commonware_runtime::Metrics;
use prometheus_client::metrics::gauge::Gauge;
use std::collections::{BTreeMap, HashMap, VecDeque};

/// The maximum number of transactions a single account can have in the mempool.
const MAX_BACKLOG: usize = 16;

/// The maximum number of transactions in the mempool.
const MAX_TRANSACTIONS: usize = 32_768;

/// A mempool for transactions.
pub struct Mempool {
    transactions: HashMap<Digest, Transaction>,
    tracked: HashMap<PublicKey, BTreeMap<u64, Digest>>,
    /// We store the public keys of the transactions to be processed next (rather than transactions
    /// received by digest) because we may receive transactions out-of-order (and/or some may have
    /// already been processed) and should just try return the transaction with the lowest nonce we
    /// are currently tracking.
    queue: VecDeque<PublicKey>,

    unique: Gauge,
    accounts: Gauge,
}

impl Mempool {
    /// Create a new mempool.
    pub fn new(context: impl Metrics) -> Self {
        // Initialize metrics
        let unique = Gauge::default();
        let accounts = Gauge::default();
        context.register(
            "transactions",
            "Number of transactions in the mempool",
            unique.clone(),
        );
        context.register(
            "accounts",
            "Number of accounts in the mempool",
            accounts.clone(),
        );

        // Initialize mempool
        Self {
            transactions: HashMap::new(),
            tracked: HashMap::new(),
            queue: VecDeque::new(),

            unique,
            accounts,
        }
    }

    /// Add a transaction to the mempool.
    pub fn add(&mut self, tx: Transaction) {
        // If there are too many transactions, ignore
        if self.transactions.len() >= MAX_TRANSACTIONS {
            return;
        }

        // Determine if duplicate
        let digest = tx.digest();
        if self.transactions.contains_key(&digest) {
            // If we already have a transaction with this digest, we don't need to track it
            return;
        }

        // Track the transaction
        let public = tx.public.clone();
        let entry = self.tracked.entry(public.clone()).or_default();

        // If there already exists a transaction at some nonce, return
        if entry.contains_key(&tx.nonce) {
            return;
        }

        // Insert the transaction into the mempool
        assert!(entry.insert(tx.nonce, digest).is_none());
        self.transactions.insert(digest, tx);

        // If there are too many transactions, remove the furthest in the future
        let entries = entry.len();
        if entries > MAX_BACKLOG {
            let (_, future) = entry.pop_last().unwrap();
            self.transactions.remove(&future);
        }

        // Add to queue if this is the first entry (otherwise the public key will already be
        // in the queue)
        if entries == 1 {
            self.queue.push_back(public);
        }

        // Update metrics
        self.unique.set(self.transactions.len() as i64);
        self.accounts.set(self.tracked.len() as i64);
    }

    /// Retain transactions for a given account with a minimum nonce.
    pub fn retain(&mut self, public: &PublicKey, min: u64) {
        // Remove any items no longer present
        let Some(tracked) = self.tracked.get_mut(public) else {
            return;
        };
        let remove = loop {
            let Some((nonce, digest)) = tracked.first_key_value() else {
                break true;
            };
            if nonce >= &min {
                break false;
            }
            self.transactions.remove(digest);
            tracked.pop_first();
        };

        // If we removed a transaction, remove the address from the tracked map
        if remove {
            self.tracked.remove(public);
        }

        // Update metrics
        self.unique.set(self.transactions.len() as i64);
        self.accounts.set(self.tracked.len() as i64);
    }

    /// Get the next transaction to process from the mempool.
    pub fn next(&mut self) -> Option<Transaction> {
        let tx = loop {
            // Get the transaction with the lowest nonce
            let address = self.queue.pop_front()?;
            let Some(tracked) = self.tracked.get_mut(&address) else {
                // We don't prune the queue when we drop a transaction, so we may need to
                // read through some untracked addresses.
                continue;
            };
            let Some((_, digest)) = tracked.pop_first() else {
                continue;
            };

            // If the address still has transactions, add it to the end of the queue (to
            // ensure everyone gets a chance to process their transactions)
            if !tracked.is_empty() {
                self.queue.push_back(address);
            } else {
                // If the address has no transactions, remove it from the tracked map
                self.tracked.remove(&address);
            }

            // Remove the transaction from the mempool
            let tx = self.transactions.remove(&digest).unwrap();
            break Some(tx);
        };

        // Update metrics
        self.unique.set(self.transactions.len() as i64);
        self.accounts.set(self.tracked.len() as i64);

        tx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use battleware_types::execution::Instruction;
    use commonware_cryptography::{ed25519::PrivateKey, PrivateKeyExt, Signer};
    use commonware_runtime::{deterministic, Runner};

    #[test]
    fn test_add_single_transaction() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            let private = PrivateKey::from_seed(1);
            let tx = Transaction::sign(&private, 0, Instruction::Generate);
            let digest = tx.digest();
            let public = tx.public.clone();

            mempool.add(tx);

            assert_eq!(mempool.transactions.len(), 1);
            assert!(mempool.transactions.contains_key(&digest));
            assert_eq!(mempool.tracked.len(), 1);
            assert!(mempool.tracked.contains_key(&public));
            assert_eq!(mempool.queue.len(), 1);
        });
    }

    #[test]
    fn test_add_duplicate_transaction() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            let private = PrivateKey::from_seed(1);
            let tx = Transaction::sign(&private, 0, Instruction::Generate);

            mempool.add(tx.clone());
            mempool.add(tx);

            assert_eq!(mempool.transactions.len(), 1);
            assert_eq!(mempool.tracked.len(), 1);
            assert_eq!(mempool.queue.len(), 1);
        });
    }

    #[test]
    fn test_add_transaction_with_same_nonce_dropped() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            let private = PrivateKey::from_seed(1);
            let tx1 = Transaction::sign(&private, 0, Instruction::Generate);
            let tx2 = Transaction::sign(&private, 0, Instruction::Match);
            let digest1 = tx1.digest();
            let digest2 = tx2.digest();

            mempool.add(tx1);
            assert!(mempool.transactions.contains_key(&digest1));

            mempool.add(tx2);
            assert!(mempool.transactions.contains_key(&digest1));
            assert!(!mempool.transactions.contains_key(&digest2));
            assert_eq!(mempool.transactions.len(), 1);
        });
    }

    #[test]
    fn test_add_multiple_transactions_same_account() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            let private = PrivateKey::from_seed(1);

            for nonce in 0..5 {
                let tx = Transaction::sign(&private, nonce, Instruction::Generate);
                mempool.add(tx);
            }

            assert_eq!(mempool.transactions.len(), 5);
            assert_eq!(mempool.tracked.len(), 1);
            assert_eq!(mempool.queue.len(), 1);
        });
    }

    #[test]
    fn test_add_exceeds_max_backlog() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            let private = PrivateKey::from_seed(1);

            for nonce in 0..=MAX_BACKLOG {
                let tx = Transaction::sign(&private, nonce as u64, Instruction::Generate);
                mempool.add(tx);
            }

            assert_eq!(mempool.transactions.len(), MAX_BACKLOG);
            assert_eq!(mempool.tracked.len(), 1);

            let tracked = mempool.tracked.get(&private.public_key()).unwrap();
            assert_eq!(tracked.len(), MAX_BACKLOG);
            assert!(tracked.contains_key(&0));
            assert!(!tracked.contains_key(&(MAX_BACKLOG as u64))); // remove oldest when full
        });
    }

    #[test]
    fn test_add_multiple_accounts() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            for seed in 0..5 {
                let private = PrivateKey::from_seed(seed);
                let tx = Transaction::sign(&private, 0, Instruction::Generate);
                mempool.add(tx);
            }

            assert_eq!(mempool.transactions.len(), 5);
            assert_eq!(mempool.tracked.len(), 5);
            assert_eq!(mempool.queue.len(), 5);
        });
    }

    #[test]
    fn test_retain_removes_old_transactions() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            let private = PrivateKey::from_seed(1);
            let public = private.public_key();

            for nonce in 0..5 {
                let tx = Transaction::sign(&private, nonce, Instruction::Generate);
                mempool.add(tx);
            }

            mempool.retain(&public, 3);

            assert_eq!(mempool.transactions.len(), 2);
            let tracked = mempool.tracked.get(&public).unwrap();
            assert!(!tracked.contains_key(&0));
            assert!(!tracked.contains_key(&1));
            assert!(!tracked.contains_key(&2));
            assert!(tracked.contains_key(&3));
            assert!(tracked.contains_key(&4));
        });
    }

    #[test]
    fn test_retain_removes_all_transactions() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            let private = PrivateKey::from_seed(1);
            let public = private.public_key();

            for nonce in 0..3 {
                let tx = Transaction::sign(&private, nonce, Instruction::Generate);
                mempool.add(tx);
            }

            mempool.retain(&public, 5);

            assert_eq!(mempool.transactions.len(), 0);
            assert!(!mempool.tracked.contains_key(&public));
        });
    }

    #[test]
    fn test_retain_nonexistent_account() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            let private = PrivateKey::from_seed(1);
            let public = private.public_key();

            mempool.retain(&public, 0);

            assert_eq!(mempool.transactions.len(), 0);
            assert_eq!(mempool.tracked.len(), 0);
        });
    }

    #[test]
    fn test_next_single_transaction() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            let private = PrivateKey::from_seed(1);
            let tx = Transaction::sign(&private, 0, Instruction::Generate);
            let expected_nonce = tx.nonce;

            mempool.add(tx);

            let next = mempool.next();
            assert!(next.is_some());
            assert_eq!(next.unwrap().nonce, expected_nonce);

            assert_eq!(mempool.transactions.len(), 0);
            assert_eq!(mempool.tracked.len(), 0);
            assert_eq!(mempool.queue.len(), 0);
        });
    }

    #[test]
    fn test_next_multiple_transactions_same_account() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            let private = PrivateKey::from_seed(1);

            for nonce in 0..3 {
                let tx = Transaction::sign(&private, nonce, Instruction::Generate);
                mempool.add(tx);
            }

            for expected_nonce in 0..3 {
                let next = mempool.next();
                assert!(next.is_some());
                assert_eq!(next.unwrap().nonce, expected_nonce);
            }

            assert_eq!(mempool.transactions.len(), 0);
            assert_eq!(mempool.tracked.len(), 0);
            assert_eq!(mempool.queue.len(), 0);
        });
    }

    #[test]
    fn test_next_round_robin_between_accounts() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            let mut privates = Vec::new();
            for seed in 0..3 {
                let private = PrivateKey::from_seed(seed);
                privates.push(private.clone());

                for nonce in 0..2 {
                    let tx = Transaction::sign(&private, nonce, Instruction::Generate);
                    mempool.add(tx);
                }
            }

            let mut account_counts = std::collections::HashMap::new();
            for _ in 0..6 {
                let next = mempool.next().unwrap();
                *account_counts.entry(next.public.clone()).or_insert(0) += 1;
            }

            for private in privates {
                assert_eq!(*account_counts.get(&private.public_key()).unwrap(), 2);
            }
        });
    }

    #[test]
    fn test_next_empty_mempool() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            let next = mempool.next();
            assert!(next.is_none());
        });
    }

    #[test]
    fn test_next_skips_removed_addresses() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            let private1 = PrivateKey::from_seed(1);
            let public1 = private1.public_key();

            let private2 = PrivateKey::from_seed(2);

            let tx1 = Transaction::sign(&private1, 0, Instruction::Generate);
            let tx2 = Transaction::sign(&private2, 0, Instruction::Generate);

            mempool.add(tx1);
            mempool.add(tx2);

            mempool.retain(&public1, 1);

            let next = mempool.next();
            assert!(next.is_some());
            assert_eq!(next.unwrap().public, private2.public_key());
        });
    }

    #[test]
    fn test_max_transactions_limit() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            for seed in 0..=MAX_TRANSACTIONS {
                let private = PrivateKey::from_seed(seed as u64);
                let tx = Transaction::sign(&private, 0, Instruction::Generate);
                mempool.add(tx);
            }

            assert_eq!(mempool.transactions.len(), MAX_TRANSACTIONS);
        });
    }

    #[test]
    fn test_metrics_updates() {
        let runner = deterministic::Runner::default();
        runner.start(|ctx| async move {
            let mut mempool = Mempool::new(ctx);

            assert_eq!(mempool.unique.get(), 0);
            assert_eq!(mempool.accounts.get(), 0);

            let private = PrivateKey::from_seed(1);
            let tx = Transaction::sign(&private, 0, Instruction::Generate);
            mempool.add(tx);

            assert_eq!(mempool.unique.get(), 1);
            assert_eq!(mempool.accounts.get(), 1);

            mempool.next();

            assert_eq!(mempool.unique.get(), 0);
            assert_eq!(mempool.accounts.get(), 0);
        });
    }
}
