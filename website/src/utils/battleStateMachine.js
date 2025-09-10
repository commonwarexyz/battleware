// Battle State Machine
// Manages the complex state transitions for battles in a clear, predictable way

// States in the battle lifecycle
export const BattleStates = {
  // Initial state when battle component mounts
  INITIALIZING: 'INITIALIZING',

  // Player is selecting their move
  SELECTING_MOVE: 'SELECTING_MOVE',

  // Player has locked their move, waiting for opponent
  MOVE_LOCKED: 'MOVE_LOCKED',

  // Both players have locked, waiting for settlement
  BOTH_LOCKED: 'BOTH_LOCKED',

  // Settlement is in progress
  SETTLING: 'SETTLING',

  // Round has been settled, showing results
  ROUND_COMPLETE: 'ROUND_COMPLETE',

  // Battle has ended
  BATTLE_ENDED: 'BATTLE_ENDED',

  // Error state
  ERROR: 'ERROR'
};

// Events that can trigger state transitions
export const BattleEvents = {
  INITIALIZED: 'INITIALIZED',
  MOVE_SELECTED: 'MOVE_SELECTED',
  MOVE_SUBMITTED: 'MOVE_SUBMITTED',
  OPPONENT_LOCKED: 'OPPONENT_LOCKED',
  BOTH_MOVES_LOCKED: 'BOTH_MOVES_LOCKED',
  SETTLE_STARTED: 'SETTLE_STARTED',
  ROUND_SETTLED: 'ROUND_SETTLED',
  BATTLE_SETTLED: 'BATTLE_SETTLED',
  ERROR_OCCURRED: 'ERROR_OCCURRED',
  RESET_FOR_NEW_ROUND: 'RESET_FOR_NEW_ROUND'
};

// State machine configuration
export class BattleStateMachine {
  constructor(initialState = BattleStates.INITIALIZING) {
    this.currentState = initialState;
    this.context = {
      battleId: null,
      currentRound: 1,
      maxRounds: 15,
      myHealth: null,
      oppHealth: null,
      myMoveCounts: [0, 0, 0, 0, 0],
      selectedMove: null,
      lockedMove: null,
      opponentLocked: false,
      battleExpiry: null,
      currentView: null,
      timeLeft: null,
      settlementInProgress: false,
      error: null
    };

    // Define valid state transitions
    this.transitions = {
      [BattleStates.INITIALIZING]: {
        [BattleEvents.INITIALIZED]: BattleStates.SELECTING_MOVE,
        [BattleEvents.ERROR_OCCURRED]: BattleStates.ERROR
      },

      [BattleStates.SELECTING_MOVE]: {
        [BattleEvents.MOVE_SELECTED]: BattleStates.SELECTING_MOVE, // Can reselect
        [BattleEvents.MOVE_SUBMITTED]: BattleStates.MOVE_LOCKED,
        [BattleEvents.OPPONENT_LOCKED]: BattleStates.SELECTING_MOVE, // Still selecting
        [BattleEvents.ERROR_OCCURRED]: BattleStates.ERROR
      },

      [BattleStates.MOVE_LOCKED]: {
        [BattleEvents.OPPONENT_LOCKED]: BattleStates.BOTH_LOCKED,
        [BattleEvents.BOTH_MOVES_LOCKED]: BattleStates.BOTH_LOCKED,
        [BattleEvents.MOVE_SELECTED]: BattleStates.MOVE_LOCKED, // Can't reselect after locking
        [BattleEvents.ERROR_OCCURRED]: BattleStates.SELECTING_MOVE // Retry on error
      },

      [BattleStates.BOTH_LOCKED]: {
        [BattleEvents.SETTLE_STARTED]: BattleStates.SETTLING,
        [BattleEvents.ERROR_OCCURRED]: BattleStates.ERROR
      },

      [BattleStates.SETTLING]: {
        [BattleEvents.ROUND_SETTLED]: BattleStates.ROUND_COMPLETE,
        [BattleEvents.BATTLE_SETTLED]: BattleStates.BATTLE_ENDED,
        [BattleEvents.ERROR_OCCURRED]: BattleStates.BOTH_LOCKED // Retry settlement
      },

      [BattleStates.ROUND_COMPLETE]: {
        [BattleEvents.RESET_FOR_NEW_ROUND]: BattleStates.SELECTING_MOVE,
        [BattleEvents.SETTLE_STARTED]: BattleStates.SETTLING, // Allow settling from ROUND_COMPLETE to end battle
        [BattleEvents.BATTLE_SETTLED]: BattleStates.BATTLE_ENDED,
        [BattleEvents.ERROR_OCCURRED]: BattleStates.ERROR
      },

      [BattleStates.BATTLE_ENDED]: {
        // Terminal state - no transitions
      },

      [BattleStates.ERROR]: {
        [BattleEvents.INITIALIZED]: BattleStates.SELECTING_MOVE // Can recover
      }
    };

    // Guards that must pass for transitions to occur
    this.guards = {
      [BattleEvents.MOVE_SUBMITTED]: (context, data) => {
        // Allow submission when we're not already settling and we have a valid move
        // Move can be provided in data or come from selectedMove
        const move = data.move !== undefined ? data.move : context.selectedMove;
        return !context.settlementInProgress && move !== null && move !== undefined;
      },
      [BattleEvents.BOTH_MOVES_LOCKED]: (context) => {
        // Always allow this transition - it's used to force settlement when needed
        return true;
      },
      [BattleEvents.SETTLE_STARTED]: (context, data, currentState) => {
        // Allow settling when past expiry and not already settling
        // OR when we've completed max rounds and are in ROUND_COMPLETE state (for final battle settlement)
        const pastExpiry = context.battleExpiry !== null &&
          context.currentView >= context.battleExpiry;
        const completedMaxRounds = context.currentRound > context.maxRounds &&
          currentState === BattleStates.ROUND_COMPLETE;

        return !context.settlementInProgress && (pastExpiry || completedMaxRounds);
      },
      [BattleEvents.RESET_FOR_NEW_ROUND]: (context) => {
        // Allow new round if we haven't exceeded max rounds
        return context.currentRound <= context.maxRounds;
      }
    };

    // Actions to perform on state transitions
    this.actions = {
      [BattleEvents.INITIALIZED]: (context, data) => {
        return {
          ...context,
          battleId: data.battleId,
          myHealth: data.myHealth,
          oppHealth: data.oppHealth,
          battleExpiry: data.battleExpiry,
          currentView: data.currentView,
          myMoveCounts: data.myMoveCounts || [0, 0, 0, 0, 0],
          currentRound: data.currentRound || 1
        };
      },

      [BattleEvents.MOVE_SELECTED]: (context, data) => {
        return {
          ...context,
          selectedMove: data.move
        };
      },

      [BattleEvents.MOVE_SUBMITTED]: (context, data) => {
        return {
          ...context,
          lockedMove: data.move !== undefined ? data.move : context.selectedMove,
          selectedMove: null
        };
      },

      [BattleEvents.OPPONENT_LOCKED]: (context) => {
        return {
          ...context,
          opponentLocked: true
        };
      },

      [BattleEvents.SETTLE_STARTED]: (context) => {
        return {
          ...context,
          settlementInProgress: true
        };
      },

      [BattleEvents.ROUND_SETTLED]: (context, data) => {
        const newMoveCounts = [...context.myMoveCounts];
        if (context.lockedMove !== null && context.lockedMove > 0) {
          newMoveCounts[context.lockedMove] = (newMoveCounts[context.lockedMove] || 0) + 1;
        }

        return {
          ...context,
          myHealth: data.myHealth,
          oppHealth: data.oppHealth,
          myMoveCounts: data.myMoveCounts || newMoveCounts,
          currentRound: data.round || context.currentRound + 1,
          battleExpiry: data.expiry,
          settlementInProgress: false,
          lockedMove: null,
          selectedMove: null,
          opponentLocked: false
        };
      },

      [BattleEvents.RESET_FOR_NEW_ROUND]: (context) => {
        return {
          ...context,
          selectedMove: null,
          lockedMove: null,
          opponentLocked: false,
          settlementInProgress: false
        };
      },

      [BattleEvents.ERROR_OCCURRED]: (context, data) => {
        return {
          ...context,
          error: data?.error || 'Unknown error',
          settlementInProgress: false,
          selectedMove: null,
          lockedMove: null
        };
      }
    };
  }

  // Check if a transition is valid
  canTransition(event, data = {}) {
    const validTransitions = this.transitions[this.currentState];
    if (!validTransitions) return false;

    const nextState = validTransitions[event];
    if (!nextState) return false;

    // Check guard if exists
    const guard = this.guards[event];
    if (guard && !guard(this.context, data, this.currentState)) return false;

    return true;
  }

  // Perform a state transition
  transition(event, data = {}) {
    const validTransitions = this.transitions[this.currentState];

    if (!validTransitions) {
      console.error(`No transitions defined for state: ${this.currentState}`);
      return false;
    }

    const nextState = validTransitions[event];

    if (!nextState) {
      console.warn(`Invalid transition: ${event} from state ${this.currentState}`);
      return false;
    }

    // Check guard
    const guard = this.guards[event];
    if (guard && !guard(this.context, data, this.currentState)) {
      console.warn(`Guard failed for transition: ${event}`);
      return false;
    }

    // Perform action if defined
    const action = this.actions[event];
    if (action) {
      this.context = action(this.context, data);
    }

    // Update state
    const previousState = this.currentState;
    this.currentState = nextState;

    console.log(`Battle state transition: ${previousState} -> ${nextState} (${event})`);

    return true;
  }

  // Get current state info
  getState() {
    return {
      state: this.currentState,
      context: this.context
    };
  }

  // Check if in a specific state
  isInState(state) {
    return this.currentState === state;
  }

  // Helper methods for common checks
  canSelectMove() {
    return this.isInState(BattleStates.SELECTING_MOVE);
  }

  canSubmitMove() {
    return this.canSelectMove() && this.context.selectedMove !== null;
  }

  isWaitingForOpponent() {
    return this.isInState(BattleStates.MOVE_LOCKED);
  }

  isSettling() {
    return this.isInState(BattleStates.SETTLING) || this.context.settlementInProgress;
  }

  isBattleActive() {
    return !this.isInState(BattleStates.BATTLE_ENDED) &&
      !this.isInState(BattleStates.ERROR);
  }

  needsSettlement() {
    // Need settlement if:
    // 1. In BOTH_LOCKED state and past expiry
    // 2. In ROUND_COMPLETE state after completing max rounds (to trigger final settlement)
    const inBothLockedPastExpiry = this.isInState(BattleStates.BOTH_LOCKED) &&
      this.context.currentView >= this.context.battleExpiry;

    const inRoundCompleteAfterMaxRounds = this.isInState(BattleStates.ROUND_COMPLETE) &&
      this.context.currentRound > this.context.maxRounds;

    return !this.context.settlementInProgress &&
      (inBothLockedPastExpiry || inRoundCompleteAfterMaxRounds);
  }

  // Update time-based context
  updateTimeContext(currentView, timeLeft) {
    this.context.currentView = currentView;
    this.context.timeLeft = timeLeft;

    // Auto-transition to BOTH_LOCKED if conditions are met
    if (this.isInState(BattleStates.MOVE_LOCKED) &&
      this.context.opponentLocked &&
      !this.isInState(BattleStates.BOTH_LOCKED)) {
      this.transition(BattleEvents.BOTH_MOVES_LOCKED);
    }
  }
}

// Helper to persist and restore state machine from localStorage
export const persistStateMachine = (battleId, stateMachine) => {
  if (!battleId) return;

  const key = `battle_state_${battleId}`;
  const data = {
    state: stateMachine.currentState,
    context: stateMachine.context,
    timestamp: Date.now()
  };

  try {
    localStorage.setItem(key, JSON.stringify(data));
  } catch (e) {
    console.error('Failed to persist battle state:', e);
  }
};

export const restoreStateMachine = (battleId) => {
  if (!battleId) return null;

  const key = `battle_state_${battleId}`;

  try {
    const stored = localStorage.getItem(key);
    if (!stored) return null;

    const data = JSON.parse(stored);

    // Check if data is recent (within 5 minutes)
    if (Date.now() - data.timestamp > 5 * 60 * 1000) {
      localStorage.removeItem(key);
      return null;
    }

    const stateMachine = new BattleStateMachine(data.state);
    stateMachine.context = data.context;

    // If we're in SETTLING state, reset the settlement flag
    // This allows retry on page reload if settlement failed
    if (stateMachine.currentState === BattleStates.SETTLING) {
      console.log('Found SETTLING state on restore, resetting to BOTH_LOCKED to allow retry');
      stateMachine.currentState = BattleStates.BOTH_LOCKED;
      stateMachine.context.settlementInProgress = false;
    }

    return stateMachine;
  } catch (e) {
    console.error('Failed to restore battle state:', e);
    return null;
  }
};

export const clearPersistedState = (battleId) => {
  if (!battleId) return;

  const key = `battle_state_${battleId}`;
  try {
    localStorage.removeItem(key);
  } catch (e) {
    console.error('Failed to clear battle state:', e);
  }
};