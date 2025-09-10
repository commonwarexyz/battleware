import { test, describe, beforeEach } from 'node:test';
import assert from 'node:assert';
import { 
  BattleStateMachine, 
  BattleStates, 
  BattleEvents,
  persistStateMachine,
  restoreStateMachine,
  clearPersistedState
} from '../src/utils/battleStateMachine.js';

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

describe('BattleStateMachine Tests', () => {
  let stateMachine;
  
  beforeEach(() => {
    localStorage.clear();
    stateMachine = new BattleStateMachine();
  });
  
  test('Initial state should be INITIALIZING', () => {
    assert.equal(stateMachine.currentState, BattleStates.INITIALIZING);
    assert.equal(stateMachine.context.battleId, null);
    assert.equal(stateMachine.context.currentRound, 1);
    assert.equal(stateMachine.context.settlementInProgress, false);
  });
  
  test('Initialization transition', () => {
    const success = stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'battle123',
      myHealth: 100,
      oppHealth: 100,
      battleExpiry: 1000,
      currentView: 900,
      myMoveCounts: [0, 0, 0, 0, 0]
    });
    
    assert(success, 'Initialization should succeed');
    assert.equal(stateMachine.currentState, BattleStates.SELECTING_MOVE);
    assert.equal(stateMachine.context.battleId, 'battle123');
    assert.equal(stateMachine.context.myHealth, 100);
    assert.equal(stateMachine.context.oppHealth, 100);
    assert.equal(stateMachine.context.battleExpiry, 1000);
  });
  
  test('Move selection and submission flow', () => {
    // Initialize first
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'battle123',
      myHealth: 100,
      oppHealth: 100,
      battleExpiry: 1000,
      currentView: 900
    });
    
    // Select a move
    stateMachine.transition(BattleEvents.MOVE_SELECTED, { move: 2 });
    assert.equal(stateMachine.context.selectedMove, 2);
    assert.equal(stateMachine.currentState, BattleStates.SELECTING_MOVE);
    
    // Submit the move
    const success = stateMachine.transition(BattleEvents.MOVE_SUBMITTED, { move: 2 });
    assert(success, 'Move submission should succeed');
    assert.equal(stateMachine.currentState, BattleStates.MOVE_LOCKED);
    assert.equal(stateMachine.context.lockedMove, 2);
    assert.equal(stateMachine.context.selectedMove, null);
  });
  
  test('Opponent locking while selecting', () => {
    // Initialize and stay in selecting
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'battle123',
      myHealth: 100,
      oppHealth: 100
    });
    
    // Opponent locks their move
    const success = stateMachine.transition(BattleEvents.OPPONENT_LOCKED);
    assert(success, 'Opponent lock should succeed');
    assert.equal(stateMachine.currentState, BattleStates.SELECTING_MOVE);
    assert(stateMachine.context.opponentLocked);
  });
  
  test('Both players locked transition', () => {
    // Initialize
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'battle123',
      myHealth: 100,
      oppHealth: 100
    });
    
    // Submit our move
    stateMachine.transition(BattleEvents.MOVE_SUBMITTED, { move: 2 });
    assert.equal(stateMachine.currentState, BattleStates.MOVE_LOCKED);
    
    // Opponent locks
    const success = stateMachine.transition(BattleEvents.OPPONENT_LOCKED);
    assert(success);
    assert.equal(stateMachine.currentState, BattleStates.BOTH_LOCKED);
  });
  
  test('Settlement flow', () => {
    // Get to BOTH_LOCKED state
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'battle123',
      myHealth: 100,
      oppHealth: 100,
      battleExpiry: 1000,
      currentView: 900
    });
    stateMachine.transition(BattleEvents.MOVE_SUBMITTED, { move: 2 });
    stateMachine.transition(BattleEvents.OPPONENT_LOCKED);
    
    assert.equal(stateMachine.currentState, BattleStates.BOTH_LOCKED);
    
    // Update time context to meet settlement conditions
    stateMachine.updateTimeContext(1000, 0);
    
    // Start settlement
    const success = stateMachine.transition(BattleEvents.SETTLE_STARTED);
    assert(success, 'Settlement should start');
    assert.equal(stateMachine.currentState, BattleStates.SETTLING);
    assert(stateMachine.context.settlementInProgress);
  });
  
  test('Settlement guard prevents early settlement', () => {
    // Get to BOTH_LOCKED state
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'battle123',
      myHealth: 100,
      oppHealth: 100,
      battleExpiry: 1000,
      currentView: 900
    });
    stateMachine.transition(BattleEvents.MOVE_SUBMITTED, { move: 2 });
    stateMachine.transition(BattleEvents.OPPONENT_LOCKED);
    
    // Try to settle before expiry
    const success = stateMachine.transition(BattleEvents.SETTLE_STARTED);
    assert(!success, 'Settlement should be blocked by guard');
    assert.equal(stateMachine.currentState, BattleStates.BOTH_LOCKED);
  });
  
  test('Round completion and reset', () => {
    // Get to settling state
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'battle123',
      myHealth: 100,
      oppHealth: 100,
      battleExpiry: 1000,
      currentView: 1000
    });
    stateMachine.context.lockedMove = 2; // Simulate having locked a move
    stateMachine.currentState = BattleStates.SETTLING;
    
    // Complete the round
    const success = stateMachine.transition(BattleEvents.ROUND_SETTLED, {
      myHealth: 90,
      oppHealth: 85,
      myMoveCounts: [0, 0, 1, 0, 0],
      expiry: 1100,
      round: 2
    });
    
    assert(success, 'Round should settle');
    assert.equal(stateMachine.currentState, BattleStates.ROUND_COMPLETE);
    assert.equal(stateMachine.context.myHealth, 90);
    assert.equal(stateMachine.context.oppHealth, 85);
    assert.equal(stateMachine.context.currentRound, 2);
    assert.equal(stateMachine.context.battleExpiry, 1100);
    assert.equal(stateMachine.context.settlementInProgress, false);
    
    // Reset for new round
    const resetSuccess = stateMachine.transition(BattleEvents.RESET_FOR_NEW_ROUND);
    assert(resetSuccess, 'Reset should succeed');
    assert.equal(stateMachine.currentState, BattleStates.SELECTING_MOVE);
    assert.equal(stateMachine.context.selectedMove, null);
    assert.equal(stateMachine.context.lockedMove, null);
    assert.equal(stateMachine.context.opponentLocked, false);
  });
  
  test('Battle end conditions - max rounds', () => {
    stateMachine.context.currentRound = 15; // Max rounds
    stateMachine.currentState = BattleStates.SETTLING;
    
    const success = stateMachine.transition(BattleEvents.BATTLE_SETTLED, {
      outcome: 'PlayerA'
    });
    
    assert(success, 'Battle should end');
    assert.equal(stateMachine.currentState, BattleStates.BATTLE_ENDED);
  });
  
  test('Battle end prevents further transitions', () => {
    stateMachine.currentState = BattleStates.BATTLE_ENDED;
    
    // Try various transitions - all should fail
    assert(!stateMachine.transition(BattleEvents.INITIALIZED));
    assert(!stateMachine.transition(BattleEvents.MOVE_SELECTED));
    assert(!stateMachine.transition(BattleEvents.RESET_FOR_NEW_ROUND));
    
    assert.equal(stateMachine.currentState, BattleStates.BATTLE_ENDED);
  });
  
  test('Error handling and recovery', () => {
    // Initialize
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'battle123',
      myHealth: 100,
      oppHealth: 100
    });
    
    // Submit move
    stateMachine.transition(BattleEvents.MOVE_SUBMITTED, { move: 2 });
    
    // Error occurs
    const success = stateMachine.transition(BattleEvents.ERROR_OCCURRED, {
      error: 'Network error'
    });
    
    assert(success, 'Error transition should succeed');
    assert.equal(stateMachine.currentState, BattleStates.SELECTING_MOVE);
    assert.equal(stateMachine.context.error, 'Network error');
    assert.equal(stateMachine.context.settlementInProgress, false);
  });
  
  test('Helper methods', () => {
    // Initialize
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'battle123',
      myHealth: 100,
      oppHealth: 100
    });
    
    // Test canSelectMove
    assert(stateMachine.canSelectMove());
    assert(!stateMachine.canSubmitMove()); // No move selected yet
    
    // Select a move
    stateMachine.context.selectedMove = 2;
    assert(stateMachine.canSubmitMove());
    
    // Submit move
    stateMachine.transition(BattleEvents.MOVE_SUBMITTED, { move: 2 });
    assert(!stateMachine.canSelectMove());
    assert(stateMachine.isWaitingForOpponent());
    
    // Test battle active
    assert(stateMachine.isBattleActive());
    
    // End battle
    stateMachine.currentState = BattleStates.BATTLE_ENDED;
    assert(!stateMachine.isBattleActive());
  });
  
  test('needsSettlement conditions', () => {
    // Initialize
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'battle123',
      myHealth: 100,
      oppHealth: 100,
      battleExpiry: 1000,
      currentView: 900
    });
    
    // Not in BOTH_LOCKED state
    assert(!stateMachine.needsSettlement());
    
    // Get to BOTH_LOCKED
    stateMachine.transition(BattleEvents.MOVE_SUBMITTED, { move: 2 });
    stateMachine.transition(BattleEvents.OPPONENT_LOCKED);
    
    // Still before expiry
    assert(!stateMachine.needsSettlement());
    
    // Update time to past expiry
    stateMachine.updateTimeContext(1001, 0);
    assert(stateMachine.needsSettlement());
    
    // Start settlement
    stateMachine.transition(BattleEvents.SETTLE_STARTED);
    assert(!stateMachine.needsSettlement()); // Already settling
  });
  
  test('Invalid transitions are rejected', () => {
    // Try to submit move from INITIALIZING
    assert(!stateMachine.transition(BattleEvents.MOVE_SUBMITTED));
    assert.equal(stateMachine.currentState, BattleStates.INITIALIZING);
    
    // Initialize properly
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'battle123',
      myHealth: 100,
      oppHealth: 100
    });
    
    // Try to settle from SELECTING_MOVE
    assert(!stateMachine.transition(BattleEvents.SETTLE_STARTED));
    assert.equal(stateMachine.currentState, BattleStates.SELECTING_MOVE);
    
    // Try to reset from SELECTING_MOVE (not in ROUND_COMPLETE)
    assert(!stateMachine.transition(BattleEvents.RESET_FOR_NEW_ROUND));
  });
  
  test('State persistence and restoration', () => {
    const battleId = 'test-battle-123';
    
    // Create and configure state machine
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: battleId,
      myHealth: 90,
      oppHealth: 85,
      battleExpiry: 1000,
      currentView: 950
    });
    stateMachine.transition(BattleEvents.MOVE_SUBMITTED, { move: 3 });
    
    // Persist the state
    persistStateMachine(battleId, stateMachine);
    
    // Restore the state
    const restored = restoreStateMachine(battleId);
    
    assert(restored, 'Should restore state machine');
    assert.equal(restored.currentState, BattleStates.MOVE_LOCKED);
    assert.equal(restored.context.battleId, battleId);
    assert.equal(restored.context.myHealth, 90);
    assert.equal(restored.context.oppHealth, 85);
    assert.equal(restored.context.lockedMove, 3);
    
    // Clear persisted state
    clearPersistedState(battleId);
    const afterClear = restoreStateMachine(battleId);
    assert.equal(afterClear, null, 'Should return null after clearing');
  });
  
  test('Expired persisted state is not restored', () => {
    const battleId = 'old-battle';
    const key = `battle_state_${battleId}`;
    
    // Store old data (6 minutes ago)
    const oldData = {
      state: BattleStates.SELECTING_MOVE,
      context: { battleId },
      timestamp: Date.now() - (6 * 60 * 1000)
    };
    localStorage.setItem(key, JSON.stringify(oldData));
    
    // Try to restore
    const restored = restoreStateMachine(battleId);
    assert.equal(restored, null, 'Should not restore expired state');
    assert.equal(localStorage.getItem(key), null, 'Should remove expired data');
  });
  
  test('Corrupted persisted state handling', () => {
    const battleId = 'corrupt-battle';
    const key = `battle_state_${battleId}`;
    
    // Store corrupted data
    localStorage.setItem(key, 'not valid json');
    
    // Try to restore
    const restored = restoreStateMachine(battleId);
    assert.equal(restored, null, 'Should return null for corrupted data');
  });
  
  test('Guards prevent invalid transitions', () => {
    // Initialize
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'battle123',
      myHealth: 100,
      oppHealth: 100
    });
    
    // Try to submit move without selecting one
    assert.equal(stateMachine.context.selectedMove, null);
    const success = stateMachine.transition(BattleEvents.MOVE_SUBMITTED, { move: null });
    assert(!success, 'Should not submit without selected move');
    assert.equal(stateMachine.currentState, BattleStates.SELECTING_MOVE);
  });
  
  test('Round reset guard checks max rounds', () => {
    stateMachine.currentState = BattleStates.ROUND_COMPLETE;
    stateMachine.context.currentRound = 16; // Exceeded max rounds
    
    const success = stateMachine.transition(BattleEvents.RESET_FOR_NEW_ROUND);
    assert(!success, 'Should not reset when exceeded max rounds');
    assert.equal(stateMachine.currentState, BattleStates.ROUND_COMPLETE);
  });
  
  test('Complex battle flow simulation', () => {
    // Initialize battle
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'complex-battle',
      myHealth: 100,
      oppHealth: 100,
      battleExpiry: 1000,
      currentView: 900,
      myMoveCounts: [0, 0, 0, 0, 0]
    });
    
    // Round 1: Both players lock moves
    stateMachine.transition(BattleEvents.MOVE_SELECTED, { move: 2 });
    stateMachine.transition(BattleEvents.MOVE_SUBMITTED, { move: 2 });
    stateMachine.transition(BattleEvents.OPPONENT_LOCKED);
    assert.equal(stateMachine.currentState, BattleStates.BOTH_LOCKED);
    
    // Time passes, trigger settlement
    stateMachine.updateTimeContext(1000, 0);
    stateMachine.transition(BattleEvents.SETTLE_STARTED);
    assert.equal(stateMachine.currentState, BattleStates.SETTLING);
    
    // Round settles
    stateMachine.transition(BattleEvents.ROUND_SETTLED, {
      myHealth: 90,
      oppHealth: 85,
      myMoveCounts: [0, 0, 1, 0, 0],
      expiry: 1100,
      round: 2
    });
    assert.equal(stateMachine.currentState, BattleStates.ROUND_COMPLETE);
    
    // Reset for round 2
    stateMachine.transition(BattleEvents.RESET_FOR_NEW_ROUND);
    assert.equal(stateMachine.currentState, BattleStates.SELECTING_MOVE);
    assert.equal(stateMachine.context.currentRound, 2);
    
    // Round 2: Player submits, error occurs
    stateMachine.transition(BattleEvents.MOVE_SUBMITTED, { move: 3 });
    stateMachine.transition(BattleEvents.ERROR_OCCURRED, { error: 'Network issue' });
    assert.equal(stateMachine.currentState, BattleStates.SELECTING_MOVE);
    
    // Retry submission
    stateMachine.transition(BattleEvents.MOVE_SUBMITTED, { move: 3 });
    stateMachine.transition(BattleEvents.OPPONENT_LOCKED);
    
    // Continue to settlement
    stateMachine.updateTimeContext(1100, 0);
    stateMachine.transition(BattleEvents.SETTLE_STARTED);
    
    // Battle ends (someone wins)
    stateMachine.transition(BattleEvents.BATTLE_SETTLED, { outcome: 'PlayerA' });
    assert.equal(stateMachine.currentState, BattleStates.BATTLE_ENDED);
    assert(!stateMachine.isBattleActive());
  });
  
  test('updateTimeContext auto-transitions to BOTH_LOCKED', () => {
    // Initialize
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'auto-transition',
      myHealth: 100,
      oppHealth: 100,
      battleExpiry: 1000,
      currentView: 900
    });
    
    // Submit our move
    stateMachine.transition(BattleEvents.MOVE_SUBMITTED, { move: 2 });
    assert.equal(stateMachine.currentState, BattleStates.MOVE_LOCKED);
    
    // Set opponent locked flag
    stateMachine.context.opponentLocked = true;
    
    // Update time should trigger auto-transition
    stateMachine.updateTimeContext(950, 50);
    assert.equal(stateMachine.currentState, BattleStates.BOTH_LOCKED);
  });
  
  test('getState returns current state and context', () => {
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'state-test',
      myHealth: 100,
      oppHealth: 100
    });
    
    const state = stateMachine.getState();
    assert.equal(state.state, BattleStates.SELECTING_MOVE);
    assert.equal(state.context.battleId, 'state-test');
    assert.equal(state.context.myHealth, 100);
  });
  
  test('isInState helper method', () => {
    assert(stateMachine.isInState(BattleStates.INITIALIZING));
    assert(!stateMachine.isInState(BattleStates.SELECTING_MOVE));
    
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'test',
      myHealth: 100,
      oppHealth: 100
    });
    
    assert(!stateMachine.isInState(BattleStates.INITIALIZING));
    assert(stateMachine.isInState(BattleStates.SELECTING_MOVE));
  });
  
  test('canTransition checks validity', () => {
    // Can initialize from INITIALIZING
    assert(stateMachine.canTransition(BattleEvents.INITIALIZED));
    assert(!stateMachine.canTransition(BattleEvents.MOVE_SUBMITTED));
    
    // After initialization
    stateMachine.transition(BattleEvents.INITIALIZED, {
      battleId: 'test',
      myHealth: 100,
      oppHealth: 100
    });
    
    // Can select and submit moves from SELECTING_MOVE
    assert(stateMachine.canTransition(BattleEvents.MOVE_SELECTED));
    // Can't submit without a move selected or provided
    assert(!stateMachine.canTransition(BattleEvents.MOVE_SUBMITTED));
    // But can submit with move in data
    assert(stateMachine.canTransition(BattleEvents.MOVE_SUBMITTED, { move: 2 }));
    assert(!stateMachine.canTransition(BattleEvents.SETTLE_STARTED));
  });
  
  test('Move counts tracking during round settlement', () => {
    stateMachine.currentState = BattleStates.SETTLING;
    stateMachine.context.lockedMove = 3;
    stateMachine.context.myMoveCounts = [0, 0, 0, 0, 0];
    
    stateMachine.transition(BattleEvents.ROUND_SETTLED, {
      myHealth: 90,
      oppHealth: 85,
      expiry: 1100,
      round: 2
    });
    
    // Move count for move 3 should be incremented
    assert.equal(stateMachine.context.myMoveCounts[3], 1);
  });
  
  test('Settlement in progress flag prevents duplicate settlements', () => {
    stateMachine.currentState = BattleStates.BOTH_LOCKED;
    stateMachine.context.battleExpiry = 1000;
    stateMachine.context.currentView = 1000;
    stateMachine.context.settlementInProgress = true;
    
    // Guard should prevent transition when already settling
    const success = stateMachine.transition(BattleEvents.SETTLE_STARTED);
    assert(!success, 'Should not start settlement when already in progress');
  });
});