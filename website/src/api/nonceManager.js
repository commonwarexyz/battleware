/**
 * Manages transaction nonces and pending transactions for a Battleware account.
 * Handles automatic nonce synchronization, transaction resubmission, and cleanup.
 */
export class NonceManager {
  constructor(client, wasm) {
    this.client = client;
    this.wasm = wasm;
    this.publicKeyHex = null;
    this.publicKeyBytes = null;
    this.resubmitInterval = null;
    this.resubmitInProgress = false; // Prevent concurrent resubmissions
    this.transactionQueue = Promise.resolve(); // Queue for sequential transaction submission

    // Configuration constants
    this.TX_STORAGE_PREFIX = 'battleware_tx_';
    this.RESUBMIT_INTERVAL_MS = 10000; // Try to resubmit transactions every 10 seconds
  }

  /**
   * Initialize the nonce manager for a specific account.
   * @param {string} publicKeyHex - Hex-encoded public key
   * @param {Uint8Array} publicKeyBytes - Raw public key bytes
   * @param {Object|null} account - Account data (null if account doesn't exist yet)
   */
  async init(publicKeyHex, publicKeyBytes, account) {
    if (!publicKeyHex || !publicKeyBytes) {
      throw new Error('Public key is required for initialization');
    }

    this.publicKeyHex = publicKeyHex;
    this.publicKeyBytes = publicKeyBytes;

    // Check if network identity has changed (indicates network reset)
    const currentIdentity = this.wasm.identityHex;
    const identityKey = 'battleware_identity';
    const storedIdentity = localStorage.getItem(identityKey);

    if (storedIdentity && storedIdentity !== currentIdentity) {
      console.log('Network identity changed - resetting nonce and clearing pending transactions');
      console.log('Previous identity:', storedIdentity);
      console.log('Current identity:', currentIdentity);

      // Reset nonce and clear pending transactions
      this.resetNonce();
      this.cleanupAllTransactions();
    }

    // Store the current identity
    localStorage.setItem(identityKey, currentIdentity);

    // Log initial state
    const pendingTxs = this.getPendingTransactions();
    if (pendingTxs.length > 0) {
      console.log(`Found ${pendingTxs.length} pending transactions`);
    }

    // Do initial sync with provided account
    this.syncWithAccountState(account);

    // Start periodic resubmission only (no more polling for nonce)
    this.startPeriodicResubmission();
  }

  /**
   * Clean up intervals and resources.
   */
  destroy() {
    if (this.resubmitInterval) {
      clearInterval(this.resubmitInterval);
      this.resubmitInterval = null;
    }
    this.resubmitInProgress = false;
  }

  /**
   * Start periodic resubmission of pending transactions.
   * @private
   */
  startPeriodicResubmission() {
    // Periodic transaction resubmission only
    this.resubmitInterval = setInterval(async () => {
      if (this.resubmitInProgress) {
        return;
      }
      try {
        await this.resubmitPendingTransactions();
      } catch (error) {
        console.error('Periodic resubmit failed:', error.message);
      }
    }, this.RESUBMIT_INTERVAL_MS);
  }

  /**
   * Synchronize local nonce with server account state.
   * @param {Object|null} account - Account data (null if account doesn't exist)
   * @private
   */
  syncWithAccountState(account) {
    if (!this.publicKeyBytes) {
      console.warn('Cannot sync - no public key set');
      return;
    }

    if (!account) {
      return;
    }

    const serverNonce = account.nonce;
    const localNonce = this.getCurrentNonce();
    const pendingTxs = this.getPendingTransactions();

    // Check for gap between server nonce and first pending transaction
    if (pendingTxs.length > 0) {
      const firstPendingNonce = pendingTxs[0].nonce;

      if (firstPendingNonce > serverNonce) {
        console.log(`Gap detected during account load: server nonce ${serverNonce}, first pending nonce ${firstPendingNonce}`);
        console.log('Resetting nonce and clearing pending transactions');

        // Reset local nonce to server nonce
        this.setNonce(serverNonce);

        // Clear all pending transactions
        this.cleanupAllTransactions();

        return; // Exit early since we've reset everything
      }
    }

    // Always clean up confirmed transactions
    // serverNonce is the next expected nonce, so anything < serverNonce is confirmed
    if (serverNonce > 0) {
      this.cleanupConfirmedTransactions(serverNonce - 1);
    }

    // Compare and sync nonces
    if (serverNonce > localNonce) {
      this.setNonce(serverNonce);
    }
  }

  /**
   * Get the current nonce from local storage.
   * @returns {number} The current nonce value
   */
  getCurrentNonce() {
    const key = 'battleware_nonce';
    const stored = localStorage.getItem(key);
    return stored ? parseInt(stored) : 0;
  }

  /**
   * Set the nonce in local storage.
   * @param {number} nonce - The nonce value to set
   * @private
   */
  setNonce(nonce) {
    const key = 'battleware_nonce';
    localStorage.setItem(key, nonce.toString());
  }

  /**
   * Reset the nonce to 0 (used when network identity changes).
   * @private
   */
  resetNonce() {
    const key = 'battleware_nonce';
    localStorage.setItem(key, '0');
  }

  /**
   * Get the next nonce to use for a transaction.
   * @returns {number} The next nonce value
   */
  getNextNonce() {
    return this.getCurrentNonce();
  }

  /**
   * Increment the nonce after successfully submitting a transaction.
   * @returns {number} The new nonce value
   * @private
   */
  incrementNonce() {
    const current = this.getCurrentNonce();
    this.setNonce(current + 1);
    return current + 1;
  }

  /**
   * Store a submitted transaction for tracking and potential resubmission.
   * @param {number} nonce - The transaction nonce
   * @param {Uint8Array} txData - The raw transaction data
   * @private
   */
  storeTransaction(nonce, txData) {
    const key = `${this.TX_STORAGE_PREFIX}${nonce}`;
    const txRecord = {
      nonce,
      txData: Array.from(txData), // Store as array for JSON serialization
      timestamp: Date.now(),
      retryCount: 0
    };
    localStorage.setItem(key, JSON.stringify(txRecord));
  }


  /**
   * Get all pending transactions sorted by nonce.
   * @returns {Array<{nonce: number, txData: Array<number>, timestamp: number, retryCount: number}>}
   */
  getPendingTransactions() {
    const prefix = this.TX_STORAGE_PREFIX;
    const transactions = [];
    const keysToCheck = [];

    // Collect all keys first to avoid iteration issues
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key && key.startsWith(prefix)) {
        keysToCheck.push(key);
      }
    }

    // Now parse the transactions
    for (const key of keysToCheck) {
      try {
        const txRecord = JSON.parse(localStorage.getItem(key));
        if (txRecord) {
          transactions.push(txRecord);
        }
      } catch (error) {
        console.error(`Error parsing transaction record ${key}:`, error);
        // Remove corrupted record
        localStorage.removeItem(key);
      }
    }

    return transactions.sort((a, b) => a.nonce - b.nonce);
  }

  /**
   * Clean up all pending transactions from localStorage.
   * @private
   */
  cleanupAllTransactions() {
    if (!this.publicKeyHex) return;

    const prefix = this.TX_STORAGE_PREFIX;
    const keysToRemove = [];

    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key && key.startsWith(prefix)) {
        keysToRemove.push(key);
      }
    }

    keysToRemove.forEach(key => localStorage.removeItem(key));
  }

  /**
   * Remove transactions that have been confirmed onchain.
   * @param {number} confirmedNonce - The highest confirmed nonce
   * @private
   */
  cleanupConfirmedTransactions(confirmedNonce) {
    const prefix = this.TX_STORAGE_PREFIX;
    const toRemove = [];


    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key && key.startsWith(prefix)) {
        try {
          const txRecord = JSON.parse(localStorage.getItem(key));
          if (txRecord.nonce <= confirmedNonce) {
            toRemove.push({ key, nonce: txRecord.nonce });
          }
        } catch (error) {
          console.error('Error parsing transaction record:', error);
        }
      }
    }

    toRemove.forEach(({ key, nonce }) => {
      localStorage.removeItem(key);
    });
  }

  /**
   * Remove all pending transactions for this account.
   * @private
   */
  cleanupAllTransactions() {
    const prefix = this.TX_STORAGE_PREFIX;
    const keysToRemove = [];

    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key && key.startsWith(prefix)) {
        keysToRemove.push(key);
      }
    }

    keysToRemove.forEach(key => {
      localStorage.removeItem(key);
    });

    if (keysToRemove.length > 0) {
      console.log(`Cleaned up ${keysToRemove.length} pending transactions`);
    }
  }

  /**
   * Attempt to resubmit all pending transactions.
   * @returns {Promise<void>}
   * @private
   */
  async resubmitPendingTransactions() {
    if (this.resubmitInProgress) {
      return;
    }

    this.resubmitInProgress = true;
    try {
      const pendingTxs = this.getPendingTransactions();

      if (pendingTxs.length === 0) {
        return;
      }


      // Try to resubmit all pending transactions
      for (const txRecord of pendingTxs) {
        // Convert array back to Uint8Array
        const txData = new Uint8Array(txRecord.txData);

        // Resubmit the transaction
        const result = await this.client.submitTransaction(txData);

        if (result.status === 'accepted') {
          // Update retry count
          txRecord.retryCount++;
          const key = `${this.TX_STORAGE_PREFIX}${txRecord.nonce}`;
          localStorage.setItem(key, JSON.stringify(txRecord));

        }
      }
    } catch (error) {
      console.error('Error in resubmitPendingTransactions:', error);
    } finally {
      this.resubmitInProgress = false;
    }
  }

  /**
   * Submit a transaction with automatic nonce management.
   * @param {Function} createTxFn - Function that creates transaction data given a nonce
   * @param {string} txType - Type of transaction for logging
   * @returns {Promise<{status: string}>} Transaction result
   * @throws {Error} If transaction submission fails
   * @private
   */
  async submitTransaction(createTxFn, txType) {
    // Queue transactions to ensure nonces are allocated sequentially
    return this.transactionQueue = this.transactionQueue.then(async () => {
      const nonce = this.getNextNonce();

      try {
        // Create the transaction with the nonce
        const txData = createTxFn(nonce);

        // Store the transaction before submitting
        this.storeTransaction(nonce, txData);

        // Submit the transaction
        const result = await this.client.submitTransaction(txData);

        if (result.status === 'accepted') {
          // Increment nonce for next transaction
          this.incrementNonce();
        } else {
          // Remove the stored transaction if it was rejected
          const key = `${this.TX_STORAGE_PREFIX}${nonce}`;
          localStorage.removeItem(key);
        }

        return result;
      } catch (error) {
        // Continue trying to submit transactions until confirmed
        console.error(`Error submitting ${txType} transaction with nonce ${nonce}:`, error.message);
        throw error;
      }
    }).catch(error => {
      // Reset queue on error to prevent blocking
      this.transactionQueue = Promise.resolve();
      throw error;
    });
  }

  /**
   * Submit a creature generation transaction.
   * @returns {Promise<{status: string}>} Transaction result
   */
  async submitGenerate() {
    return this.submitTransaction(
      (nonce) => this.wasm.createGenerateTransaction(nonce),
      'generate'
    );
  }

  /**
   * Submit a matchmaking transaction.
   * @returns {Promise<{status: string}>} Transaction result
   */
  async submitMatch() {
    return this.submitTransaction(
      (nonce) => this.wasm.createMatchTransaction(nonce),
      'match'
    );
  }

  /**
   * Submit a battle move transaction.
   * @param {Uint8Array} battleId - The battle identifier
   * @param {number} moveIndex - The move index to execute
   * @param {number} expiry - Move expiration time
   * @param {Uint8Array} masterPublic - Master public key for verification
   * @returns {Promise<{status: string}>} Transaction result
   */
  async submitMove(battleId, moveIndex, expiry, masterPublic) {
    return this.submitTransaction(
      (nonce) => this.wasm.createMoveTransaction(nonce, masterPublic, expiry, moveIndex),
      'move'
    );
  }

  /**
   * Submit a battle settlement transaction.
   * @param {Uint8Array} seed - The seed for settlement
   * @returns {Promise<{status: string}>} Transaction result
   */
  async submitSettle(seed) {
    return this.submitTransaction(
      (nonce) => this.wasm.createSettleTransaction(nonce, seed),
      'settle'
    );
  }

  /**
   * Update nonce based on executed transaction from event stream.
   * This replaces polling - we now track executed transactions directly.
   * @param {number} executedNonce - The nonce of the executed transaction
   */
  updateNonceFromTransaction(executedNonce) {
    const currentNonce = this.getCurrentNonce();
    const nextExpectedNonce = executedNonce + 1;

    // Clean up the confirmed transaction
    this.cleanupConfirmedTransactions(executedNonce);

    // Update our nonce if the executed transaction advances it
    if (nextExpectedNonce > currentNonce) {
      this.setNonce(nextExpectedNonce);
    } else {
    }
  }
}