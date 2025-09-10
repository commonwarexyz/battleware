import { WasmWrapper } from './wasm.js';
import { NonceManager } from './nonceManager.js';

// Delay between fetch retries
const FETCH_RETRY_DELAY_MS = 1000;

/**
 * Client for communicating with the Battleware simulator.
 * Handles WebSocket connections, transaction submission, and state queries.
 */
export class BattlewareClient {
  constructor(baseUrl = '/api', wasm) {
    if (!wasm) {
      throw new Error('WasmWrapper is required for BattlewareClient');
    }
    this.baseUrl = baseUrl;
    this.wasm = wasm;
    this.updatesWs = null;
    this.eventHandlers = new Map();
    this.nonceManager = new NonceManager(this, wasm);
    this.masterPublic = wasm.identityBytes;
    this.latestSeed = null;

    // Reconnection configuration
    this.reconnectConfig = {
      attempts: 0,
      baseDelay: 1000,
      maxDelay: 30000, // Cap at 30 seconds
      reconnecting: false,
      timer: null
    };
  }

  async init() {
    await this.wasm.init();
    // Set master public key after wasm is initialized
    this.masterPublic = this.wasm.identityBytes;
    return this;
  }

  /**
   * Initialize the nonce manager with a keypair.
   * @param {string} publicKeyHex - Hex-encoded public key
   * @param {Uint8Array} publicKeyBytes - Raw public key bytes
   * @param {Object|null} account - Account data (null if account doesn't exist)
   */
  async initNonceManager(publicKeyHex, publicKeyBytes, account) {
    await this.nonceManager.init(publicKeyHex, publicKeyBytes, account);
  }

  /**
   * Clean up the nonce manager and WebSocket connections.
   */
  destroy() {
    this.nonceManager.destroy();

    // Stop any pending reconnection attempts
    this.reconnectConfig.reconnecting = false;
    this.reconnectConfig.attempts = 0;

    // Clear any pending reconnection timers
    if (this.reconnectConfig.timer) {
      clearTimeout(this.reconnectConfig.timer);
      this.reconnectConfig.timer = null;
    }

    // Close WebSocket connections without triggering reconnect
    if (this.updatesWs) {
      // Remove the close handler to prevent reconnection
      this.updatesWs.onclose = null;
      this.updatesWs.close();
      this.updatesWs = null;
    }

    // Clear event handlers to prevent memory leaks
    this.eventHandlers.clear();
  }

  /**
   * Connect to a different updates stream.
   * @param {Uint8Array|null} publicKey - Public key bytes for account filter, or null for all events
   * @returns {Promise<void>}
   */
  async switchUpdates(publicKey = null) {
    // Stop any pending reconnection attempts
    this.reconnectConfig.reconnecting = false;
    this.reconnectConfig.attempts = 0;
    if (this.reconnectConfig.timer) {
      clearTimeout(this.reconnectConfig.timer);
      this.reconnectConfig.timer = null;
    }

    // Close existing connection if any
    if (this.updatesWs) {
      // Remove the close handler to prevent reconnection
      this.updatesWs.onclose = null;
      this.updatesWs.close();
      this.updatesWs = null;
    }

    // Connect with new filter
    await this.connectUpdates(publicKey);
  }


  /**
   * Submit a transaction to the simulator.
   * @param {Uint8Array} transaction - Raw transaction bytes
   * @returns {Promise<{status: string}>} Transaction result
   * @throws {Error} If submission fails
   */
  async submitTransaction(transaction) {
    // Wrap transaction in Submission enum
    const submission = this.wasm.wrapTransactionSubmission(transaction);

    const response = await fetch(`${this.baseUrl}/submit`, {
      method: 'POST',
      headers: {
        'Content-Type': 'application/octet-stream'
      },
      body: submission
    });

    if (!response.ok) {
      const errorText = await response.text();
      console.error('Server error response:', errorText);
      throw new Error(`Server error: ${response.status} ${response.statusText}`);
    }

    // The simulator returns 200 OK with no body for successful submissions
    // Transaction results come through the events WebSocket
    return { status: 'accepted' };
  }

  /**
   * Get the current view number from the latest seed we've seen.
   * @returns {number|null} Current view number or null if no seed seen yet
   */
  getCurrentView() {
    return this.latestSeed ? this.latestSeed.view : null;
  }

  /**
   * Wait for the first seed to arrive.
   * @returns {Promise<void>} Resolves when first seed is received
   */
  waitForFirstSeed() {
    return new Promise((resolve) => {
      if (this.latestSeed) {
        resolve();
        return;
      }

      // Set up a one-time handler for the first seed
      const unsubscribe = this.onEvent('Seed', () => {
        unsubscribe();
        resolve();
      });
    });
  }


  /**
   * Query state by key.
   * @param {Uint8Array} keyBytes - State key bytes
   * @returns {Promise<{found: boolean, value: any}>} Query result
   */
  async queryState(keyBytes) {
    // Hash the key before querying (matching upstream changes)
    const hashedKey = this.wasm.hashKey(keyBytes);
    const hexKey = this.wasm.bytesToHex(hashedKey);

    let response;
    while (true) {
      response = await fetch(`${this.baseUrl}/state/${hexKey}`);

      if (response.status === 404) {
        return { found: false, value: null };
      }

      if (response.status === 200) {
        break;
      }

      // Retry on any other status
      console.log(`State query returned ${response.status}, retrying...`);
      await new Promise(resolve => setTimeout(resolve, FETCH_RETRY_DELAY_MS));
    }

    // Get binary response
    const buffer = await response.arrayBuffer();
    const valueBytes = new Uint8Array(buffer);

    if (valueBytes.length === 0) {
      return { found: false, value: null };
    }

    try {
      // Decode value using WASM - returns plain JSON object
      const value = this.wasm.decodeLookup(valueBytes);
      return { found: true, value };
    } catch (error) {
      console.error('Failed to decode value:', error);
      return { found: false, value: null };
    }
  }

  /**
   * Query seed by view number.
   * @param {number} view - View number to query
   * @returns {Promise<{found: boolean, seed?: any, seedBytes?: Uint8Array}>} Query result
   */
  async querySeed(view) {
    // Encode the query for specific view index
    const queryBytes = this.wasm.encodeQuery('index', view);
    const hexQuery = this.wasm.bytesToHex(queryBytes);

    let response;
    while (true) {
      response = await fetch(`${this.baseUrl}/seed/${hexQuery}`);

      if (response.status === 404) {
        return { found: false };
      }

      if (response.status === 200) {
        break;
      }

      // Retry on any other status
      console.log(`Seed query returned ${response.status}, retrying...`);
      await new Promise(resolve => setTimeout(resolve, FETCH_RETRY_DELAY_MS));
    }

    const seedBytes = await response.arrayBuffer();
    const seedBytesArray = new Uint8Array(seedBytes);
    const seed = this.wasm.decodeSeed(seedBytesArray);
    return { found: true, seed, seedBytes: seedBytesArray };
  }

  /**
   * Connect to the updates WebSocket stream with exponential backoff.
   * @param {Uint8Array|null} publicKey - Public key bytes for account filter, or null for all events
   * @returns {Promise<void>}
   * @private
   */
  connectUpdates(publicKey = null) {
    return new Promise((resolve, reject) => {
      // Store the publicKey for reconnection
      this.currentUpdateFilter = publicKey;

      // Encode the filter based on whether we have a public key
      let filterBytes;
      if (publicKey === null) {
        // Connect to all events (firehose)
        filterBytes = this.wasm.encodeUpdatesFilterAll();
      } else {
        // Connect to account-specific events
        filterBytes = this.wasm.encodeUpdatesFilterAccount(publicKey);
      }
      const filterHex = this.wasm.bytesToHex(filterBytes);

      let wsUrl;
      if (this.baseUrl.startsWith('http://') || this.baseUrl.startsWith('https://')) {
        // Full URL provided, convert to WebSocket URL
        const url = new URL(this.baseUrl);
        wsUrl = url.protocol === 'https:'
          ? `wss://${url.host}/updates/${filterHex}`
          : `ws://${url.host}/updates/${filterHex}`;
      } else {
        // Relative URL, use window.location
        wsUrl = window.location.protocol === 'https:'
          ? `wss://${window.location.host}${this.baseUrl}/updates/${filterHex}`
          : `ws://${window.location.host}${this.baseUrl}/updates/${filterHex}`;
      }

      console.log('Connecting to Updates WebSocket at:', wsUrl, 'with filter:', publicKey ? 'account' : 'all');
      this.updatesWs = new WebSocket(wsUrl);

      this.updatesWs.onopen = () => {
        console.log('Updates WebSocket connected successfully');
        resolve();
      };

      this.updatesWs.onerror = (error) => {
        console.error('Updates WebSocket error:', error);
        console.error('WebSocket URL was:', wsUrl);
        console.error('WebSocket readyState:', this.updatesWs.readyState);
        reject(new Error(`WebSocket connection failed to ${wsUrl}`));
      };

      this.updatesWs.onmessage = async (event) => {
        try {
          let bytes;
          if (event.data instanceof Blob) {
            // Browser environment - convert blob to array buffer
            const arrayBuffer = await event.data.arrayBuffer();
            bytes = new Uint8Array(arrayBuffer);
          } else if (event.data instanceof ArrayBuffer) {
            // ArrayBuffer - convert directly
            bytes = new Uint8Array(event.data);
          } else if (Buffer.isBuffer(event.data)) {
            // Node.js environment - Buffer is already a Uint8Array
            bytes = new Uint8Array(event.data);
          } else {
            console.warn('Unknown WebSocket message type:', typeof event.data);
            return;
          }

          // Now we have binary data in bytes, decode it
          try {
            const decodedUpdate = this.wasm.decodeUpdate(bytes);

            // Check if it's a Seed or Events/FilteredEvents update
            if (decodedUpdate.type === 'Seed') {
              this.latestSeed = decodedUpdate;
              this.handleEvent(decodedUpdate);
            } else if (decodedUpdate.type === 'Events') {
              // Process each event from the array - treat FilteredEvents the same as Events
              for (const eventData of decodedUpdate.events) {
                // Check if this is a transaction from our account
                if (eventData.type === 'Transaction') {
                  if (this.nonceManager.publicKeyHex &&
                    eventData.public.toLowerCase() === this.nonceManager.publicKeyHex.toLowerCase()) {
                    this.nonceManager.updateNonceFromTransaction(eventData.nonce);
                  }
                }
                this.handleEvent(eventData);
              }
            }
          } catch (decodeError) {
            console.error('Failed to decode update:', decodeError);
            console.log('Full raw bytes:', this.wasm.bytesToHex(bytes).match(/.{2}/g).join(' '));
          }
        } catch (e) {
          console.error('Failed to process WebSocket message:', e);
        }
      };

      this.updatesWs.onclose = (event) => {
        console.log('Updates WebSocket disconnected, code:', event.code, 'reason:', event.reason);
        this.handleReconnect('updatesWs', () => this.connectUpdates(this.currentUpdateFilter));
      };
    });
  }


  /**
   * Subscribe to events of a specific type.
   * @param {string} eventType - Event type to subscribe to ('*' for all events)
   * @param {Function} handler - Event handler function
   * @returns {Function} Unsubscribe function
   */
  onEvent(eventType, handler) {
    if (!this.eventHandlers.has(eventType)) {
      this.eventHandlers.set(eventType, []);
    }
    this.eventHandlers.get(eventType).push(handler);

    // Return unsubscribe function to prevent memory leaks
    return () => {
      const handlers = this.eventHandlers.get(eventType);
      if (handlers) {
        const index = handlers.indexOf(handler);
        if (index !== -1) {
          handlers.splice(index, 1);
        }
        // Clean up empty handler arrays
        if (handlers.length === 0) {
          this.eventHandlers.delete(eventType);
        }
      }
    };
  }

  /**
   * Handle incoming events from WebSocket.
   * @param {Object} event - Event data object
   * @private
   */
  handleEvent(event) {
    const handlers = this.eventHandlers.get(event.type) || [];
    handlers.forEach(handler => handler(event));

    // Also call generic handlers
    const allHandlers = this.eventHandlers.get('*') || [];
    allHandlers.forEach(handler => handler(event));
  }

  /**
   * Handle WebSocket reconnection with exponential backoff.
   * @param {string} wsType - Type of WebSocket ('updatesWs')
   * @param {Function} connectFn - Function to call for reconnection
   * @private
   */
  handleReconnect(wsType, connectFn) {
    const config = this.reconnectConfig;

    if (config.reconnecting) {
      return;
    }

    config.reconnecting = true;
    config.attempts++;

    // Calculate delay with exponential backoff and jitter
    const baseDelay = Math.min(config.baseDelay * Math.pow(2, config.attempts - 1), config.maxDelay);
    const jitter = Math.random() * 0.3 * baseDelay; // 30% jitter
    const delay = baseDelay + jitter;

    console.log(`Reconnecting ${wsType} in ${Math.round(delay)}ms (attempt ${config.attempts})`);

    config.timer = setTimeout(async () => {
      // Check if we've been destroyed while waiting
      if (config.attempts === 0) {
        // attempts is reset to 0 in destroy()
        return;
      }

      try {
        await connectFn();
        // Reset on successful connection
        config.attempts = 0;
        console.log(`Successfully reconnected ${wsType}`);
      } catch (error) {
        console.error(`Failed to reconnect ${wsType}:`, error.message);
        config.reconnecting = false;
        // Try again unless destroyed
        if (config.attempts > 0) {
          this.handleReconnect(wsType, connectFn);
        }
      } finally {
        config.reconnecting = false;
        config.timer = null;
      }
    }, delay);
  }

  /**
   * Get account information by public key.
   * @param {Uint8Array} publicKeyBytes - Account public key
   * @returns {Promise<Object|null>} Account data or null if not found
   */
  async getAccount(publicKeyBytes) {
    const keyBytes = this.wasm.encodeAccountKey(publicKeyBytes);
    const result = await this.queryState(keyBytes);

    if (result.found && result.value) {
      // Value is already a plain object from WASM
      if (result.value.type === 'Account') {
        return result.value;
      } else {
        console.log('Value is not an Account type:', result.value.type);
        return null;
      }
    }

    return null;
  }


  async getBattle(battleDigest) {
    const keyBytes = this.wasm.encodeBattleKey(battleDigest);
    const result = await this.queryState(keyBytes);

    if (result.found && result.value.type === 'Battle') {
      return result.value;
    }

    return null;
  }

  /**
   * Submit a creature generation transaction.
   * @returns {Promise<{status: string}>} Transaction result
   */
  async submitGenerate() {
    return this.nonceManager.submitGenerate();
  }

  /**
   * Submit a matchmaking transaction.
   * @returns {Promise<{status: string}>} Transaction result
   */
  async submitMatch() {
    return this.nonceManager.submitMatch();
  }

  /**
   * Submit a battle move transaction.
   * @param {Uint8Array} battleId - Battle identifier
   * @param {number} moveIndex - Move index to execute
   * @param {number} expiry - Move expiration time
   * @returns {Promise<{status: string}>} Transaction result
   */
  async submitMove(battleId, moveIndex, expiry) {
    return this.nonceManager.submitMove(battleId, moveIndex, expiry, this.masterPublic);
  }

  /**
   * Submit a battle settlement transaction.
   * @param {Uint8Array} seed - Settlement seed
   * @returns {Promise<{status: string}>} Transaction result
   */
  async submitSettle(seed) {
    return this.nonceManager.submitSettle(seed);
  }

  /**
   * Get existing keypair from localStorage or create a new one.
   * @returns {{publicKey: Uint8Array, publicKeyHex: string}} Keypair information
   * @warning Private keys are stored in localStorage which is not secure.
   *          In production, consider using more secure storage methods.
   */
  getOrCreateKeypair() {
    // Security warning for development
    if (typeof window !== 'undefined' && window.location.hostname === 'localhost') {
      console.warn('WARNING: Private keys are stored in localStorage. This is not secure for production use.');
    }

    // Check if we have a stored private key in localStorage
    const storedPrivateKeyHex = localStorage.getItem('battleware_private_key');

    if (storedPrivateKeyHex) {
      // Convert hex string back to bytes
      const privateKeyBytes = new Uint8Array(storedPrivateKeyHex.match(/.{1,2}/g).map(byte => parseInt(byte, 16)));
      this.wasm.createKeypair(privateKeyBytes);
      console.log('Loaded keypair from storage');
    } else {
      // Let WASM generate a new keypair using the browser's crypto API
      this.wasm.createKeypair();

      // Store the private key for persistence (Note: In production, consider more secure storage)
      const privateKeyHex = this.wasm.getPrivateKeyHex();
      localStorage.setItem('battleware_private_key', privateKeyHex);
      console.log('Generated new keypair using browser crypto API and saved to localStorage');
    }

    const keypair = {
      publicKey: this.wasm.getPublicKeyBytes(),
      publicKeyHex: this.wasm.getPublicKeyHex()
    };

    console.log('Using keypair with public key:', keypair.publicKeyHex);

    return keypair;
  }

  /**
   * Fetch and format the leaderboard data.
   * @returns {Promise<Array>} Formatted leaderboard array
   */
  async fetchLeaderboard() {
    try {
      const keyBytes = this.wasm.encodeLeaderboardKey();
      const hashedKey = this.wasm.hashKey(keyBytes);
      const hexKey = this.wasm.bytesToHex(hashedKey);

      let response;
      while (true) {
        response = await fetch(`${this.baseUrl}/state/${hexKey}`);

        if (response.status === 404) {
          return [];
        }

        if (response.status === 200) {
          break;
        }

        // Retry on any other status
        console.log(`Leaderboard query returned ${response.status}, retrying...`);
        await new Promise(resolve => setTimeout(resolve, FETCH_RETRY_DELAY_MS));
      }

      // Decode value using WASM - returns a plain object
      const buffer = await response.arrayBuffer();
      const valueBytes = new Uint8Array(buffer);
      const leaderboard = this.wasm.decodeLookup(valueBytes);
      const players = leaderboard.players;

      // Format players - each player is [publicKeyBytes, stats]
      const formattedPlayers = players
        .map(player => {
          const [publicKeyBytes, stats] = player;
          const publicKeyHex = this.wasm.bytesToHex(new Uint8Array(publicKeyBytes));
          return {
            publicKey: publicKeyHex,
            elo: stats.elo,
            wins: stats.wins,
            losses: stats.losses,
            draws: stats.draws
          };
        })
        .sort((a, b) => b.elo - a.elo); // Sort by ELO descending

      return formattedPlayers;
    } catch (error) {
      console.error('Failed to fetch leaderboard:', error);
      return [];
    }
  }

}