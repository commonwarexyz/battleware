import { spawn } from 'child_process';
import { test, describe, before, after, beforeEach } from 'node:test';
import assert from 'node:assert';
import path from 'path';
import { fileURLToPath } from 'url';
import { WebSocket } from 'ws';
import fetch from 'node-fetch';
import fs from 'fs/promises';
import { WasmWrapper } from '../src/api/wasm.js';
import { BattlewareClient } from '../src/api/client.js';

// We need to use dynamic import for WASM in Node.js environment
let wasmWrapper;

// Setup globals
global.fetch = fetch;
global.WebSocket = WebSocket;

// Mock localStorage for Node.js environment
global.localStorage = {
  storage: {},
  getItem(key) {
    return this.storage[key] || null;
  },
  setItem(key, value) {
    this.storage[key] = value;
  },
  removeItem(key) {
    delete this.storage[key];
  },
  clear() {
    this.storage = {};
  },
  get length() {
    return Object.keys(this.storage).length;
  },
  key(index) {
    return Object.keys(this.storage)[index];
  }
};

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);

const SIMULATOR_PORT = 8089;
const SIMULATOR_URL = `http://localhost:${SIMULATOR_PORT}`;

let simulatorProcess;

async function waitForPort(port, maxAttempts = 20) {
  for (let i = 0; i < maxAttempts; i++) {
    try {
      // Check if simulator is ready by querying the seed endpoint
      const latestQuery = wasmWrapper.encodeQueryLatest();
      const latestHex = wasmWrapper.bytesToHex(latestQuery);
      const response = await fetch(`http://localhost:${port}/seed/${latestHex}`);
      // 404 is OK - it means the endpoint is responding but no seed exists yet
      if (response.status === 404 || response.ok) return true;
    } catch (e) {
      // Port not ready
    }
    await new Promise(resolve => setTimeout(resolve, 500));
  }
  throw new Error(`Port ${port} did not become ready`);
}

async function startSimulator() {
  const testIdentity = wasmWrapper.getIdentity(0n);
  const testIdentityHex = wasmWrapper.bytesToHex(testIdentity);

  // Always use the pre-built binary for consistent and fast test execution
  const simulatorPath = path.join(__dirname, '../../target/release/battleware-simulator');
  const simulatorArgs = ['-p', SIMULATOR_PORT.toString(), '-i', testIdentityHex];

  simulatorProcess = spawn(simulatorPath, simulatorArgs, {
    cwd: path.join(__dirname, '../../'),
    stdio: 'pipe'
  });

  simulatorProcess.stderr.on('data', (data) => {
    const str = data.toString();
    if (!str.includes('WARN') && !str.includes('INFO') && !str.includes('warning:')) {
      console.error(`Simulator: ${str}`);
    }
  });

  await waitForPort(SIMULATOR_PORT);
}

async function stopSimulator() {
  if (simulatorProcess) {
    simulatorProcess.kill();
    await new Promise(resolve => setTimeout(resolve, 1000));
    simulatorProcess = null;
  }
}


before(async () => {
  // Load WASM wrapper
  try {
    // First, manually load and initialize the WASM module for Node.js environment
    const wasmPath = path.join(__dirname, '../wasm/pkg/battleware_wasm_bg.wasm');
    const wasmBuffer = await fs.readFile(wasmPath);

    // Import the JS bindings
    const wasmJs = await import('../wasm/pkg/battleware_wasm.js');

    // Initialize with the WASM buffer for Node.js
    await wasmJs.default(wasmBuffer);

    // Create a WasmWrapper instance with the initialized WASM module
    wasmWrapper = new WasmWrapper();
    wasmWrapper.wasm = wasmJs; // Directly set the wasm module
    // Set the simulator identity for tests that need decoding
    wasmWrapper.identityBytes = wasmJs.get_identity(0n);

  } catch (e) {
    console.error('Failed to load WASM wrapper:', e);
    throw e;
  }

  // Start simulator
  await startSimulator();
});

after(async () => {
  await stopSimulator();
});

describe('WASM Tests', () => {
  test('Signer generation and properties', async (t) => {
    // Test random keypair generation
    const keypair1 = new wasmWrapper.Signer();
    const keypair2 = new wasmWrapper.Signer();

    assert(keypair1.public_key instanceof Uint8Array, 'Public key should be Uint8Array');
    assert.equal(keypair1.public_key.length, 32, 'Public key should be 32 bytes');
    assert(keypair1.private_key instanceof Uint8Array, 'Private key should be Uint8Array');
    assert.equal(keypair1.private_key.length, 32, 'Private key should be 32 bytes');

    // Keys should be different
    assert.notDeepEqual(keypair1.public_key, keypair2.public_key, 'Different keypairs should have different public keys');
    assert.notDeepEqual(keypair1.private_key, keypair2.private_key, 'Different keypairs should have different private keys');

    // Test hex encoding
    assert.equal(keypair1.public_key_hex.length, 64, 'Hex public key should be 64 characters');
    assert.equal(keypair1.private_key_hex.length, 64, 'Hex private key should be 64 characters');

    // Test from_bytes constructor
    const keypair3 = wasmWrapper.Signer.from_bytes(keypair1.private_key);
    assert.deepEqual(keypair3.public_key, keypair1.public_key, 'Reconstructed keypair should have same public key');
    assert.deepEqual(keypair3.private_key, keypair1.private_key, 'Reconstructed keypair should have same private key');

    // Test error handling
    assert.throws(() => {
      wasmWrapper.Signer.from_bytes(new Uint8Array(31)); // Wrong size
    }, 'Should throw on invalid private key size');
  });

  test('Transaction creation and encoding', async (t) => {
    const keypair = new wasmWrapper.Signer();

    // Test Generate transaction
    const generateTx = wasmWrapper.Transaction.generate(keypair, 0n);
    const generateBytes = generateTx.encode();
    assert(generateBytes instanceof Uint8Array, 'Transaction should encode to Uint8Array');
    assert(generateBytes.length > 100, 'Transaction should have reasonable size');

    // Test Match transaction
    const matchTx = wasmWrapper.Transaction.match_tx(keypair, 1n);
    const matchBytes = matchTx.encode();
    assert(matchBytes instanceof Uint8Array, 'Match transaction should encode to Uint8Array');

    // Test nonces
    const tx1 = wasmWrapper.Transaction.generate(keypair, 0n);
    const tx2 = wasmWrapper.Transaction.generate(keypair, 1n);
    assert.notDeepEqual(tx1.encode(), tx2.encode(), 'Different nonces should produce different transactions');
  });

  test('Key encoding functions', async (t) => {
    // Test account key encoding
    const keypair = new wasmWrapper.Signer();
    const accountKey = wasmWrapper.encodeAccountKey(keypair.public_key);
    assert(accountKey instanceof Uint8Array, 'Account key should be Uint8Array');
    assert.equal(accountKey[0], 0, 'Account key variant should be 0');
    assert.equal(accountKey.length, 33, 'Account key should be 33 bytes (1 + 32)');

    // Test battle key encoding
    const battleDigest = new Uint8Array(32);
    for (let i = 0; i < 32; i++) battleDigest[i] = i;
    const battleKey = wasmWrapper.encodeBattleKey(battleDigest);
    assert(battleKey instanceof Uint8Array, 'Battle key should be Uint8Array');
    assert.equal(battleKey[0], 2, 'Battle key variant should be 2');
    assert.equal(battleKey.length, 33, 'Battle key should be 33 bytes (1 + 32)');

    // Test error handling
    assert.throws(() => {
      wasmWrapper.encodeAccountKey(new Uint8Array(31)); // Wrong size
    }, 'Should throw on invalid public key size');

    assert.throws(() => {
      wasmWrapper.encodeBattleKey(new Uint8Array(31)); // Wrong size
    }, 'Should throw on invalid digest size');
  });

  test('Query encoding functions', async (t) => {
    // Test Query::Latest
    const latestQuery = wasmWrapper.encodeQueryLatest();
    assert(latestQuery instanceof Uint8Array, 'Latest query should be Uint8Array');
    assert.equal(latestQuery[0], 0, 'Latest query variant should be 0');
    assert.equal(latestQuery.length, 1, 'Latest query should be 1 byte');

    // Test Query::Index
    const indexQuery = wasmWrapper.encodeQueryIndex(42n);
    assert(indexQuery instanceof Uint8Array, 'Index query should be Uint8Array');
    assert.equal(indexQuery[0], 1, 'Index query variant should be 1');
    assert(indexQuery.length > 1, 'Index query should include the index value');

    // Test different indices produce different encodings
    const index1 = wasmWrapper.encodeQueryIndex(1n);
    const index2 = wasmWrapper.encodeQueryIndex(2n);
    assert.notDeepEqual(index1, index2, 'Different indices should produce different queries');
  });

  test('Creature generation from traits', async (t) => {
    const traits = new Uint8Array(32);
    for (let i = 0; i < 32; i++) {
      traits[i] = i;
    }
    const creature = wasmWrapper.generateCreatureFromTraits(traits);

    assert(creature.traits, 'Creature should have traits');
    assert.equal(creature.traits.length, 32, 'Traits should be 32 bytes');
    assert(typeof creature.health === 'number', 'Creature should have health');
    assert(creature.health >= 0, 'Health should be non-negative');

    assert(Array.isArray(creature.moves), 'Creature should have moves array');
    assert.equal(creature.moves.length, 5, 'Should have 5 moves');

    creature.moves.forEach((move, index) => {
      assert(typeof move.index === 'number', 'Move should have index');
      assert(typeof move.name === 'string', 'Move should have name');
      assert(typeof move.strength === 'number', 'Move should have strength');
      assert(typeof move.usage_limit === 'number', 'Move should have usage limit');
      assert(typeof move.is_defense === 'boolean', 'Move should have is_defense flag');

      if (index === 0) assert.equal(move.name, 'No-op');
      if (index === 1) {
        assert.equal(move.name, 'Defense');
        assert(move.is_defense, 'Defense move should have is_defense=true');
      }
    });

    // Test error handling
    assert.throws(() => {
      wasmWrapper.generateCreatureFromTraits(new Uint8Array(31)); // Wrong size
    }, 'Should throw on invalid traits size');
  });

  test('Verify events from WebSocket with summaries', async (t) => {
    // Connect to events WebSocket first (using 'all' filter)
    const allFilter = wasmWrapper.encodeUpdatesFilterAll();
    const filterHex = wasmWrapper.bytesToHex(allFilter);
    const updatesWs = new WebSocket(`ws://localhost:${SIMULATOR_PORT}/updates/${filterHex}`);
    const receivedEvents = [];

    await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        updatesWs.close();
        resolve();
      }, 5000); // 5 second timeout

      updatesWs.on('open', async () => {
        // Create and submit a summary to generate events
        const keypair1 = new wasmWrapper.Signer();
        const tx1 = wasmWrapper.Transaction.generate(keypair1, 0n);

        const summaryBytes1 = wasmWrapper.executeBlock(
          0n,  // network_secret
          1n,  // view
          tx1.encode()
        );

        // Wrap summary in Submission enum
        const submission = wasmWrapper.wrapSummarySubmission(summaryBytes1);

        const summaryResponse1 = await fetch(`${SIMULATOR_URL}/submit`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/octet-stream' },
          body: submission
        });
        assert.equal(summaryResponse1.status, 200, 'Summary submission should succeed');
      });

      updatesWs.on('message', async (data) => {
        // Decode and verify the update
        let update = wasmWrapper.decodeUpdate(new Uint8Array(data));

        // Check if it's an Events update
        if (update.type === 'Events') {
          let events = update.events;
          assert.equal(events.length, 2, 'Should have 2 outputs');
          assert.equal(events[0].type, 'Generated', 'First event should be Generated');
          assert.equal(events[1].type, 'Transaction', 'Second event should be Transaction');
          receivedEvents.push(events);

          clearTimeout(timeout);
          updatesWs.close();
          resolve();
        }
        // Ignore Seed updates for this test
      });

      updatesWs.on('error', (err) => {
        clearTimeout(timeout);
        reject(err);
      });
    });

    // Verify we received events
    assert(receivedEvents.length > 0, 'Should have received at least one events message');

    // Restart simulator to clear state for next test
    await stopSimulator();
    await startSimulator();
  });

  test('Query account state', async (t) => {
    const keypair = new wasmWrapper.Signer();

    // Query account state (should not exist initially)
    const accountKey = wasmWrapper.encodeAccountKey(keypair.public_key);
    // Hash the key to match the new behavior
    const hashedKey = wasmWrapper.hashKey(accountKey);
    const accountHex = wasmWrapper.bytesToHex(hashedKey);

    const stateResponse = await fetch(`${SIMULATOR_URL}/state/${accountHex}`);
    assert.equal(stateResponse.status, 404, 'Non-existent account should return 404');

    // Create a generate transaction for the account
    const tx = wasmWrapper.Transaction.generate(keypair, 0n);

    // Create a test summary with the transaction
    // Using the same identity seed (0) as the simulator was started with
    const summaryBytes = wasmWrapper.executeBlock(
      0n,  // network_secret (matches simulator)
      1n,  // view
      tx.encode()  // transaction bytes
    );

    // Submit the summary to the simulator
    // Wrap summary in Submission enum
    const submission = wasmWrapper.wrapSummarySubmission(summaryBytes);
    const summaryResponse = await fetch(`${SIMULATOR_URL}/submit`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/octet-stream' },
      body: submission
    });
    assert.equal(summaryResponse.status, 200, 'Summary submission should succeed');

    // Now query the account state again - it should exist
    const stateResponse2 = await fetch(`${SIMULATOR_URL}/state/${accountHex}`);
    assert.equal(stateResponse2.status, 200, 'Account should exist after summary');

    // Decode and verify the state (simulator returns a Lookup object)
    const stateBytes = await stateResponse2.arrayBuffer();
    // Get the identity of the simulator (using same seed as when we started it)
    const stateValue = await wasmWrapper.decodeLookup(new Uint8Array(stateBytes));

    assert.equal(stateValue.type, 'Account', 'Should decode as Account value');
    assert.equal(stateValue.nonce, 1, 'Account should have nonce 1 after generate tx');
    assert(stateValue.creature, 'Account should have a creature');
    assert.equal(stateValue.creature.traits.length, 32, 'Creature should have 32 trait bytes');
    assert.equal(stateValue.elo, 1000, 'Account should have default elo (1000)');
    assert.equal(stateValue.wins, 0, 'Account should have 0 wins');
    assert.equal(stateValue.losses, 0, 'Account should have 0 losses');
    assert.equal(stateValue.draws, 0, 'Account should have 0 draws');

    // Restart simulator to clear state for next test
    await stopSimulator();
    await startSimulator();
  });


  test('UpdatesFilter for account-specific events', async (t) => {
    // Create two accounts
    const keypair1 = new wasmWrapper.Signer();
    const keypair2 = new wasmWrapper.Signer();

    // Connect to updates stream filtered for account1
    const accountFilter = wasmWrapper.encodeUpdatesFilterAccount(keypair1.public_key);
    const filterHex = wasmWrapper.bytesToHex(accountFilter);
    const updatesWs = new WebSocket(`ws://localhost:${SIMULATOR_PORT}/updates/${filterHex}`);

    const receivedEvents = [];

    await new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        updatesWs.close();
        resolve();
      }, 5000);

      updatesWs.on('open', async () => {
        // Create transactions for both accounts
        const tx1 = wasmWrapper.Transaction.generate(keypair1, 0n);
        const tx2 = wasmWrapper.Transaction.generate(keypair2, 0n);

        // Concatenate both transaction bytes into a single buffer
        const tx1Bytes = tx1.encode();
        const tx2Bytes = tx2.encode();
        const combinedTxs = new Uint8Array(tx1Bytes.length + tx2Bytes.length);
        combinedTxs.set(tx1Bytes, 0);
        combinedTxs.set(tx2Bytes, tx1Bytes.length);

        // Submit both in a single block
        const summaryBytes = wasmWrapper.executeBlock(0n, 1n, combinedTxs);
        const submission = wasmWrapper.wrapSummarySubmission(summaryBytes);

        const response = await fetch(`${SIMULATOR_URL}/submit`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/octet-stream' },
          body: submission
        });
        assert.equal(response.status, 200, 'Summary submission should succeed');
      });

      updatesWs.on('message', async (data) => {
        const update = wasmWrapper.decodeUpdate(new Uint8Array(data));

        // Check if it's an Events update
        if (update.type === 'Events') {
          let events = update.events;
          assert.equal(events.length, 2, 'Should have 2 outputs');
          assert.equal(events[0].type, 'Generated', 'First event should be Generated');
          assert.equal(events[1].type, 'Transaction', 'Second event should be Transaction');
          receivedEvents.push(events);

          clearTimeout(timeout);
          updatesWs.close();
          resolve();
        } else {
          console.log('Received message:', update.type);
        }
        // Ignore Seed updates for this test
      });

      updatesWs.on('error', reject);
    });

    // Should have received events only for account1
    assert(receivedEvents.length > 0, 'Should have received events for account1');

    // Verify no events for account2
    const account2Events = receivedEvents.filter(e =>
      (e.account && e.account === keypair2.public_key_hex) ||
      (e.public && e.public === keypair2.public_key_hex)
    );
    assert.equal(account2Events.length, 0, 'Should not receive events for account2');

    // Restart simulator to clear state
    await stopSimulator();
    await startSimulator();
  });

  test('Client switchUpdates functionality', async (t) => {
    // Import the client class

    // Initialize WASM wrapper
    const testIdentityHex = wasmWrapper.bytesToHex(wasmWrapper.getIdentity(0n));
    const wasm = new WasmWrapper(testIdentityHex);
    wasm.wasm = wasmWrapper.wasm;
    wasm.identityBytes = wasmWrapper.getIdentity(0n);

    // Create client and keypair
    const client = new BattlewareClient(SIMULATOR_URL, wasm);
    const keypair = client.wasm.createKeypair();
    const publicKey = client.wasm.getPublicKeyBytes();
    await client.initNonceManager(
      client.wasm.getPublicKeyHex(),
      publicKey,
      null
    );

    // Connect with account filter using public key
    await client.connectUpdates(publicKey);
    assert(client.updatesWs, 'WebSocket should be connected');
    const ws1 = client.updatesWs;

    // Switch to 'all' filter (null for all events)
    await client.switchUpdates(null);
    assert(client.updatesWs, 'WebSocket should be connected after switch');
    assert.notEqual(client.updatesWs, ws1, 'Should have new WebSocket connection');

    // Switch back to 'account' filter using public key
    await client.switchUpdates(publicKey);
    assert(client.updatesWs, 'WebSocket should be connected after second switch');

    // Clean up
    client.destroy();
  });

  test('Event handler memory leak prevention', async (t) => {
    // Import the client class

    // Initialize WASM wrapper
    const testIdentityHex = wasmWrapper.bytesToHex(wasmWrapper.getIdentity(0n));
    const wasm = new WasmWrapper(testIdentityHex);
    wasm.wasm = wasmWrapper.wasm;
    wasm.identityBytes = wasmWrapper.getIdentity(0n);

    // Create client
    const client = new BattlewareClient(SIMULATOR_URL, wasm);

    // Test 1: Event handler returns unsubscribe function
    const handler1 = () => { };
    const unsubscribe1 = client.onEvent('Generated', handler1);
    assert(typeof unsubscribe1 === 'function', 'onEvent should return unsubscribe function');

    // Verify handler is registered
    assert(client.eventHandlers.has('Generated'), 'Handler should be registered');
    assert.equal(client.eventHandlers.get('Generated').length, 1, 'Should have 1 handler');

    // Test 2: Unsubscribe removes handler
    unsubscribe1();
    assert(!client.eventHandlers.has('Generated'), 'Handler should be removed after unsubscribe');

    // Test 3: Multiple handlers and selective unsubscribe
    const handler2 = () => { };
    const handler3 = () => { };
    const unsubscribe2 = client.onEvent('Matched', handler2);
    const unsubscribe3 = client.onEvent('Matched', handler3);

    assert.equal(client.eventHandlers.get('Matched').length, 2, 'Should have 2 handlers');

    // Unsubscribe one handler
    unsubscribe2();
    assert.equal(client.eventHandlers.get('Matched').length, 1, 'Should have 1 handler after unsubscribe');
    assert(client.eventHandlers.get('Matched').includes(handler3), 'Handler 3 should still be registered');

    // Test 4: Destroy clears all handlers
    client.onEvent('*', () => { });
    client.onEvent('Moved', () => { });
    client.onEvent('Settled', () => { });

    assert(client.eventHandlers.size > 0, 'Should have multiple event types registered');

    client.destroy();
    assert.equal(client.eventHandlers.size, 0, 'All handlers should be cleared after destroy');
  });

  test('WebSocket reconnection with exponential backoff', async (t) => {
    // Import the client class

    // Initialize WASM wrapper
    const testIdentityHex = wasmWrapper.bytesToHex(wasmWrapper.getIdentity(0n));
    const wasm = new WasmWrapper(testIdentityHex);
    wasm.wasm = wasmWrapper.wasm;
    wasm.identityBytes = wasmWrapper.getIdentity(0n);

    // Create client with invalid URL to force reconnection
    const client = new BattlewareClient('http://localhost:9999', wasm);

    // Test reconnection config initialization
    assert.equal(client.reconnectConfig.baseDelay, 1000, 'Should have base delay set');
    assert.equal(client.reconnectConfig.maxDelay, 30000, 'Should have max delay capped at 30s');

    // Test handleReconnect method with limited attempts
    let reconnectAttempts = 0;
    let reconnectDelays = [];

    // Mock the reconnection to succeed after 3 attempts
    const testReconnectFn = async () => {
      reconnectAttempts++;

      // Capture the delay for this attempt
      const config = client.reconnectConfig;
      const baseDelay = Math.min(config.baseDelay * Math.pow(2, config.attempts - 1), config.maxDelay);
      reconnectDelays.push(baseDelay);

      if (reconnectAttempts < 3) {
        throw new Error('Test reconnection failure');
      }
      // Success on 3rd attempt
      return true;
    };

    // Trigger reconnection
    client.handleReconnect('updatesWs', testReconnectFn);

    // Wait for all reconnection attempts (need more time for 3 attempts with exponential backoff)
    await new Promise(resolve => setTimeout(resolve, 10000));

    assert.equal(reconnectAttempts, 3, 'Should have attempted 3 reconnections');
    assert(client.reconnectConfig.attempts === 0, 'Attempts should be reset after success');

    // Verify exponential backoff
    assert(reconnectDelays[0] <= 1000, 'First delay should be ~1000ms');
    assert(reconnectDelays[1] <= 2000, 'Second delay should be ~2000ms');
    assert(reconnectDelays[2] <= 4000, 'Third delay should be ~4000ms');

    // Clean up - set reconnecting to false to stop any pending timeouts
    client.reconnectConfig.reconnecting = false;
    client.destroy();
  });

  test('Error handling for invalid inputs', async (t) => {
    // Test invalid key sizes
    assert.throws(() => {
      wasmWrapper.Signer.from_bytes(new Uint8Array(10));
    }, 'Should throw on short private key');

    // Ed25519 private keys are 32 bytes, providing 64 bytes will just read the first 32
    // So we need to test with actual invalid data that would fail to parse
    assert.throws(() => {
      wasmWrapper.Signer.from_bytes(new Uint8Array(0)); // Empty array
    }, 'Should throw on empty private key');

    // Test invalid public key for account encoding
    await assert.rejects(async () => {
      wasmWrapper.encodeAccountKey(new Uint8Array(10));
    }, 'Should reject on invalid public key size');

    // Test invalid digest for battle key
    await assert.rejects(async () => {
      wasmWrapper.encodeBattleKey(new Uint8Array(10));
    }, 'Should reject on invalid digest size');

    // Test invalid traits for creature
    await assert.rejects(async () => {
      wasmWrapper.generateCreatureFromTraits(new Uint8Array(10));
    }, 'Should reject on invalid traits size');

    // Test error handling - decode_seed requires identity parameter
    assert.throws(() => {
      wasmWrapper.decodeSeed(new Uint8Array([255, 255, 255]));
    }, 'Should throw on invalid seed encoding');
  });

  test('Seed upload and query with verification', async (t) => {
    // Upload a seed
    const seedBytes = wasmWrapper.encodeSeed(0n, 1n);

    // Wrap seed in Submission enum
    const submission = wasmWrapper.wrapSeedSubmission(seedBytes);

    const seedUploadResponse = await fetch(`${SIMULATOR_URL}/submit`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/octet-stream'
      },
      body: submission
    });

    assert.equal(seedUploadResponse.status, 200, 'Seed upload should succeed');

    // Query the seed we just uploaded
    const latestQuery = wasmWrapper.encodeQueryLatest();
    const latestHex = wasmWrapper.bytesToHex(latestQuery);

    const seedResponse = await fetch(`${SIMULATOR_URL}/seed/${latestHex}`);
    assert.equal(seedResponse.status, 200, 'Seed query should return 200');

    // Decode the seed
    const seedResponseBytes = await seedResponse.arrayBuffer();
    const seed = await wasmWrapper.decodeSeed(new Uint8Array(seedResponseBytes));

    // The seed is returned as a plain object
    assert.equal(seed.type, 'Seed', 'Should decode as Seed');
    assert.equal(seed.view, 1, 'Seed should have view 1');
    assert(seed.bytes, 'Seed should have bytes');

    // Test with incorrect identity (different seed) - should throw
    const originalIdentity = wasmWrapper.identityBytes;
    wasmWrapper.identityBytes = wasmWrapper.getIdentity(123n); // Different seed
    assert.throws(() => {
      wasmWrapper.decodeSeed(new Uint8Array(seedResponseBytes));
    }, 'Should throw when seed verification fails with incorrect identity');
    // Restore original identity
    wasmWrapper.identityBytes = originalIdentity;

    // Also test Query::Index
    const indexQuery = wasmWrapper.encodeQueryIndex(1n);
    const indexHex = wasmWrapper.bytesToHex(indexQuery);

    const indexResponse = await fetch(`${SIMULATOR_URL}/seed/${indexHex}`);
    assert.equal(indexResponse.status, 200, 'Index query should also return 200');
  });

});

describe('API Tests', () => {
  // Clear localStorage before each test
  beforeEach(() => {
    localStorage.clear();
  });

  // Helper to create test client and wasm wrapper
  async function createTestClient() {

    const testIdentityHex = wasmWrapper.bytesToHex(wasmWrapper.getIdentity(0n));
    const wasm = new WasmWrapper(testIdentityHex);
    wasm.wasm = wasmWrapper.wasm;
    wasm.identityBytes = wasmWrapper.getIdentity(0n);

    return { BattlewareClient, WasmWrapper, wasm };
  }

  test('Client destroy method cleanup', async (t) => {
    const { BattlewareClient, wasm } = await createTestClient();
    wasm.createKeypair();

    // Create client and initialize
    const client = new BattlewareClient(SIMULATOR_URL, wasm);
    const keypair = {
      publicKey: wasm.getPublicKeyBytes(),
      publicKeyHex: wasm.getPublicKeyHex()
    };
    await client.initNonceManager(keypair.publicKeyHex, keypair.publicKey);

    // Add some event handlers
    client.onEvent('test1', () => { });
    client.onEvent('test2', () => { });

    // Verify state before destroy
    assert(client.nonceManager, 'Should have nonce manager');
    assert(client.eventHandlers.size > 0, 'Should have event handlers');

    // Call destroy
    client.destroy();

    // Verify cleanup
    assert.equal(client.eventHandlers.size, 0, 'Event handlers should be cleared');
    assert.equal(client.eventWs, null, 'Event WebSocket should be null');
    assert.equal(client.seedWs, null, 'Seed WebSocket should be null');

    // Restart simulator to clear state for next test
    await stopSimulator();
    await startSimulator();
  });

  test('Client can submit transactions and receive correct response', async (t) => {
    const { BattlewareClient, wasm } = await createTestClient();
    wasm.createKeypair();

    // Create client
    const client = new BattlewareClient(SIMULATOR_URL, wasm);

    // Test submitTransaction returns correct response format
    const tx = wasm.createGenerateTransaction(0);
    const result = await client.submitTransaction(tx);

    assert.deepEqual(result, { status: 'accepted' }, 'Should return accepted status');

    // Clean up
    client.destroy();

    // Restart simulator to clear state for next test
    await stopSimulator();
    await startSimulator();
  });

  test('Client can get current view from seed', async (t) => {
    const { BattlewareClient, wasm } = await createTestClient();

    // Create client
    const client = new BattlewareClient(SIMULATOR_URL, wasm);

    // Initialize nonce manager with a test keypair
    const keypair = client.wasm.createKeypair();
    const publicKey = client.wasm.getPublicKeyBytes();
    await client.initNonceManager(
      client.wasm.getPublicKeyHex(),
      publicKey,
      null
    );

    // Connect to updates stream to receive seeds (using account filter)
    await client.connectUpdates(publicKey);

    // First upload a seed so we have something to receive
    const seedBytes = wasmWrapper.encodeSeed(0n, 1n);
    const submission = wasmWrapper.wrapSeedSubmission(seedBytes);
    const seedUploadResponse = await fetch(`${SIMULATOR_URL}/submit`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/octet-stream' },
      body: submission
    });
    assert.equal(seedUploadResponse.status, 200, 'Seed upload should succeed');

    // Wait for the seed to arrive through WebSocket
    await client.waitForFirstSeed();

    // Get current view - now it should have a value
    const view = client.getCurrentView();
    assert(typeof view === 'number', 'View should be a number');
    assert(view >= 1, 'View should be at least 1');

    // Clean up
    client.destroy();
  });

  test('Client can query seeds', async (t) => {
    const { BattlewareClient, wasm } = await createTestClient();

    // Create client
    const client = new BattlewareClient(SIMULATOR_URL, wasm);

    // Query seed by view 1 (should exist from previous test)
    const result = await client.querySeed(1);
    assert(result.found === true, 'Should find seed for view 1');
    assert(result.seed, 'Should return seed data');

    // Query non-existent seed
    const notFound = await client.querySeed(99999);
    assert(notFound.found === false, 'Should not find seed for view 99999');

    // Clean up
    client.destroy();
  });

  test('Security warning for localStorage keypair', async (t) => {
    const { BattlewareClient, wasm } = await createTestClient();
    wasm.createKeypair();

    // Create client
    const client = new BattlewareClient(SIMULATOR_URL, wasm);

    // Mock console.warn to capture warning
    const originalWarn = console.warn;
    let warningCalled = false;
    let warningMessage = '';
    console.warn = (msg) => {
      if (msg.includes('Private keys are stored in localStorage')) {
        warningCalled = true;
        warningMessage = msg;
      }
    };

    // Mock window.location for localhost
    if (typeof window === 'undefined') {
      global.window = {
        location: {
          hostname: 'localhost'
        }
      };
    }

    // Call getOrCreateKeypair
    client.getOrCreateKeypair();

    // Verify warning was shown
    assert(warningCalled, 'Security warning should be displayed');
    assert(warningMessage.includes('not secure for production'), 'Warning should mention production security');

    // Restore console.warn
    console.warn = originalWarn;

    // Clean up
    if (global.window) {
      delete global.window;
    }
    client.destroy();
  });

  test('Client enforces seed verification', async (t) => {
    // Import the client class

    // Test 1: WasmWrapper without identity should throw on decodeSeed
    const wasmNoIdentity = new WasmWrapper(); // No identity
    wasmNoIdentity.wasm = wasmWrapper.wasm;

    // Upload a test seed first
    const seedBytes = wasmWrapper.encodeSeed(0n, 1n);
    await fetch(`${SIMULATOR_URL}/seed`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/octet-stream' },
      body: seedBytes
    });

    assert.throws(() => {
      wasmNoIdentity.decodeSeed(seedBytes);
    }, /No identity configured/, 'Should throw when no identity is configured');

    // Test 2: WasmWrapper with wrong identity should throw on invalid signature
    const wrongIdentityHex = wasmWrapper.bytesToHex(wasmWrapper.getIdentity(999n));
    const wasmWrongIdentity = new WasmWrapper(wrongIdentityHex);
    wasmWrongIdentity.wasm = wasmWrapper.wasm;
    wasmWrongIdentity.identityBytes = wasmWrapper.getIdentity(999n);

    assert.throws(() => {
      wasmWrongIdentity.decodeSeed(seedBytes);
    }, /invalid seed/, 'Should throw when seed verification fails');

    // Test 3: WasmWrapper with correct identity should succeed
    const correctIdentityHex = wasmWrapper.bytesToHex(wasmWrapper.getIdentity(0n));
    const wasmCorrectIdentity = new WasmWrapper(correctIdentityHex);
    wasmCorrectIdentity.wasm = wasmWrapper.wasm;
    wasmCorrectIdentity.identityBytes = wasmWrapper.getIdentity(0n);

    const decodedSeed = wasmCorrectIdentity.decodeSeed(seedBytes);
    assert(decodedSeed, 'Should successfully decode and verify seed with correct identity');
    assert.equal(decodedSeed.type, 'Seed', 'Should decode as Seed');
  });

  test('Client can query state', async (t) => {
    const { BattlewareClient, wasm } = await createTestClient();
    wasm.createKeypair();

    // Create client
    const client = new BattlewareClient(SIMULATOR_URL, wasm);

    // Query account state (should not exist)
    const publicKeyBytes = wasm.getPublicKeyBytes();
    const accountKey = wasm.encodeAccountKey(publicKeyBytes);
    const result = await client.queryState(accountKey);

    assert(result.found === false, 'Should not find account that does not exist');
    assert(result.value === null, 'Value should be null for non-existent state');

    // Clean up
    client.destroy();

    // Shutdown simulator
    await stopSimulator();
  });

  test('Client retries fetch on non-200/404 responses', async (t) => {
    const { BattlewareClient, wasm } = await createTestClient();
    wasm.createKeypair();

    // Create a mock fetch that fails initially, then succeeds
    const originalFetch = global.fetch;
    let fetchCallCount = 0;
    const targetCallsBeforeSuccess = 3;
    
    global.fetch = async (url, options) => {
      fetchCallCount++;
      
      // For state queries
      if (url.includes('/state/')) {
        if (fetchCallCount < targetCallsBeforeSuccess) {
          // Return 500 error for first attempts
          return { 
            status: 500,
            ok: false,
            statusText: 'Internal Server Error'
          };
        } else {
          // Return 404 (not found) on success attempt
          return { 
            status: 404,
            ok: false,
            statusText: 'Not Found'
          };
        }
      }
      
      // For seed queries
      if (url.includes('/seed/')) {
        if (fetchCallCount < targetCallsBeforeSuccess) {
          // Return 503 error for first attempts
          return { 
            status: 503,
            ok: false,
            statusText: 'Service Unavailable'
          };
        } else {
          // Return 404 (not found) on success attempt
          return { 
            status: 404,
            ok: false,
            statusText: 'Not Found'
          };
        }
      }
      
      // Fall back to original fetch for other URLs
      return originalFetch(url, options);
    };

    // Mock console.log to verify retry messages
    const originalLog = console.log;
    let retryLogs = [];
    console.log = (msg) => {
      if (msg.includes('retrying...')) {
        retryLogs.push(msg);
      }
    };

    try {
      // Create client
      const client = new BattlewareClient(SIMULATOR_URL, wasm);

      // Test queryState retry
      fetchCallCount = 0;
      const publicKeyBytes = wasm.getPublicKeyBytes();
      const accountKey = wasm.encodeAccountKey(publicKeyBytes);
      const stateResult = await client.queryState(accountKey);
      
      assert(stateResult.found === false, 'Should return not found after retries');
      assert(fetchCallCount === targetCallsBeforeSuccess, `Should have made ${targetCallsBeforeSuccess} fetch calls for state query`);
      assert(retryLogs.some(log => log.includes('State query returned 500')), 'Should log state query retry for 500');

      // Test querySeed retry
      fetchCallCount = 0;
      retryLogs = [];
      const seedResult = await client.querySeed(1);
      
      assert(seedResult.found === false, 'Should return not found after retries');
      assert(fetchCallCount === targetCallsBeforeSuccess, `Should have made ${targetCallsBeforeSuccess} fetch calls for seed query`);
      assert(retryLogs.some(log => log.includes('Seed query returned 503')), 'Should log seed query retry for 503');

      // Test fetchLeaderboard retry
      fetchCallCount = 0;
      retryLogs = [];
      const leaderboard = await client.fetchLeaderboard();
      
      assert(Array.isArray(leaderboard), 'Should return empty array after retries');
      assert(leaderboard.length === 0, 'Should return empty leaderboard');
      assert(fetchCallCount === targetCallsBeforeSuccess, `Should have made ${targetCallsBeforeSuccess} fetch calls for leaderboard`);
      assert(retryLogs.some(log => log.includes('Leaderboard query returned 500')), 'Should log leaderboard query retry');

      // Clean up
      client.destroy();
    } finally {
      // Restore original functions
      global.fetch = originalFetch;
      console.log = originalLog;
    }
  });
});