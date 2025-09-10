// Wrapper for WASM functionality
let wasmModule = null;

export async function initWasm() {
  if (!wasmModule) {
    wasmModule = await import('../../wasm/pkg/battleware_wasm.js');
    await wasmModule.default();
  }
  return wasmModule;
}

export class WasmWrapper {
  constructor(identityHex) {
    this.wasm = null;
    this.keypair = null;
    this.identityHex = identityHex;
    this.identityBytes = null;
  }

  async init() {
    this.wasm = await initWasm();
    // Convert identity hex to bytes if provided
    if (this.identityHex) {
      this.identityBytes = this.hexToBytes(this.identityHex);
    }
    return this;
  }

  // Create a new keypair
  createKeypair(privateKeyBytes) {
    if (privateKeyBytes !== undefined) {
      // Only support 32-byte private keys
      if (!(privateKeyBytes instanceof Uint8Array) || privateKeyBytes.length !== 32) {
        throw new Error('Private key must be a Uint8Array of exactly 32 bytes');
      }
      this.keypair = this.wasm.Signer.from_bytes(privateKeyBytes);
    } else {
      // Let WASM generate a new key using the browser's crypto API
      this.keypair = new this.wasm.Signer();
    }
    return this.keypair;
  }

  // Get public key as hex string
  getPublicKeyHex() {
    if (!this.keypair) {
      throw new Error('Keypair not initialized');
    }
    return this.keypair.public_key_hex;
  }

  // Get public key as bytes
  getPublicKeyBytes() {
    if (!this.keypair) {
      throw new Error('Keypair not initialized');
    }
    return this.keypair.public_key;
  }

  // Get private key as hex string
  getPrivateKeyHex() {
    if (!this.keypair) {
      throw new Error('Keypair not initialized');
    }
    return this.keypair.private_key_hex;
  }

  // Create a generate transaction
  createGenerateTransaction(nonce) {
    if (!this.keypair) {
      throw new Error('Keypair not initialized');
    }
    // Convert nonce to BigInt for WASM
    const tx = this.wasm.Transaction.generate(this.keypair, BigInt(nonce));
    return tx.encode();
  }

  // Create a match transaction
  createMatchTransaction(nonce) {
    if (!this.keypair) {
      throw new Error('Keypair not initialized');
    }
    // Convert nonce to BigInt for WASM
    const tx = this.wasm.Transaction.match_tx(this.keypair, BigInt(nonce));
    return tx.encode();
  }

  // Create a move transaction
  createMoveTransaction(nonce, masterPublicBytes, expiry, moveIndex) {
    if (!this.keypair) {
      throw new Error('Keypair not initialized');
    }
    // Convert nonce to BigInt for WASM
    const tx = this.wasm.Transaction.move_tx(
      this.keypair,
      BigInt(nonce),
      masterPublicBytes,
      BigInt(expiry),
      moveIndex
    );
    return tx.encode();
  }

  // Create a settle transaction
  createSettleTransaction(nonce, seedBytes) {
    if (!this.keypair) {
      throw new Error('Keypair not initialized');
    }
    // Convert nonce to BigInt for WASM
    const tx = this.wasm.Transaction.settle_tx(
      this.keypair,
      BigInt(nonce),
      seedBytes
    );
    return tx.encode();
  }

  // Encode keys
  encodeAccountKey(publicKeyBytes) {
    return this.wasm.encode_account_key(publicKeyBytes);
  }

  encodeBattleKey(digestBytes) {
    return this.wasm.encode_battle_key(digestBytes);
  }

  encodeLeaderboardKey() {
    return this.wasm.encode_leaderboard_key();
  }

  // Encode UpdatesFilter for all events
  encodeUpdatesFilterAll() {
    return this.wasm.encode_updates_filter_all();
  }

  // Encode UpdatesFilter for a specific account
  encodeUpdatesFilterAccount(publicKeyBytes) {
    return this.wasm.encode_updates_filter_account(publicKeyBytes);
  }

  // Hash a key for state queries
  hashKey(keyBytes) {
    return this.wasm.hash_key(keyBytes);
  }

  // Decode a lookup
  decodeLookup(bytes) {
    // Require identity for events verification
    if (!this.identityBytes) {
      throw new Error('No identity configured for events verification');
    }

    try {
      // Decode the Events struct - this will verify the certificate and proof
      const events = this.wasm.decode_lookup(bytes, this.identityBytes);
      return events;
    } catch (error) {
      // Log the actual error for debugging
      console.warn('Failed to decode as Lookup:', error.toString());
      throw error; // Re-throw to let caller handle it
    }
  }

  // Decode and verify seed in one operation
  decodeSeed(bytes) {
    // Require identity for seed verification
    if (!this.identityBytes) {
      throw new Error('No identity configured for seed verification');
    }

    // Decode the seed - this will throw if verification fails
    try {
      const seed = this.wasm.decode_seed(bytes, this.identityBytes);
      return seed;
    } catch (error) {
      // Re-throw with the original error message
      throw new Error(error.toString());
    }
  }

  // Generate creature from traits
  generateCreatureFromTraits(traits) {
    return this.wasm.generate_creature_from_traits(traits);
  }

  // Convert hex to bytes
  hexToBytes(hex) {
    const bytes = new Uint8Array(hex.length / 2);
    for (let i = 0; i < hex.length; i += 2) {
      bytes[i / 2] = parseInt(hex.substr(i, 2), 16);
    }
    return bytes;
  }

  // Convert bytes to hex
  bytesToHex(bytes) {
    return Array.from(bytes)
      .map(b => b.toString(16).padStart(2, '0'))
      .join('');
  }

  // Encode query
  encodeQuery(type, index) {
    if (type === 'latest') {
      return this.wasm.encode_query_latest();
    } else if (type === 'index' && index !== undefined) {
      // Convert to BigInt for WASM
      return this.wasm.encode_query_index(BigInt(index));
    }
    throw new Error('Invalid query type');
  }

  // Decode update (can be either Seed or Events)
  decodeUpdate(bytes) {
    // Require identity for update verification
    if (!this.identityBytes) {
      throw new Error('No identity configured for update verification');
    }

    const update = this.wasm.decode_update(bytes, this.identityBytes);
    return update;
  }

  // Wrap a transaction in a Submission enum
  wrapTransactionSubmission(transactionBytes) {
    return this.wasm.wrap_transaction_submission(transactionBytes);
  }

  // Wrap a summary in a Submission enum
  wrapSummarySubmission(summaryBytes) {
    return this.wasm.wrap_summary_submission(summaryBytes);
  }

  // Wrap a seed in a Submission enum
  wrapSeedSubmission(seedBytes) {
    return this.wasm.wrap_seed_submission(seedBytes);
  }

  // Get identity for a given seed
  getIdentity(seed) {
    return this.wasm.get_identity(seed);
  }

  // Encode a seed
  encodeSeed(seed, view) {
    return this.wasm.encode_seed(seed, view);
  }

  // Execute a block with transactions
  executeBlock(networkSecret, view, txBytes) {
    return this.wasm.execute_block(networkSecret, view, txBytes);
  }

  // Encode query latest
  encodeQueryLatest() {
    return this.wasm.encode_query_latest();
  }

  // Encode query index
  encodeQueryIndex(index) {
    return this.wasm.encode_query_index(index);
  }

  // Get access to Signer class
  get Signer() {
    return this.wasm.Signer;
  }

  // Get access to Transaction class
  get Transaction() {
    return this.wasm.Transaction;
  }
}