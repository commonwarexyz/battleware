import React, { useState, useEffect, useCallback, useRef, useMemo } from 'react';
import RetroBox from './RetroBox';
import RetroText from './RetroText';
import { parseCreature as parseCreatureUtil } from '../utils/creatureUtils';
import { generateTrainerName } from '../utils/trainerUtils';
import {
  BattleStateMachine,
  BattleStates,
  BattleEvents,
  persistStateMachine,
  restoreStateMachine,
  clearPersistedState
} from '../utils/battleStateMachine';

const TURN_TIME = 50; // Default turn time based on MOVE_EXPIRY in lib.rs
const MAX_BATTLE_ROUNDS = 15; // Maximum number of rounds before timeout/draw

const BattleScreen = ({ client, player, account, battle }) => {
  const [battleMessage, setBattleMessage] = useState('What will you do?');
  const [opponentCreature, setOpponentCreature] = useState(null);
  const [opponentAccount, setOpponentAccount] = useState(null);
  const [isLoading, setIsLoading] = useState(true);

  // Store seeds as they come in - must be at component level
  const seedsRef = useRef(new Map());

  const isPlayerA = battle?.player_a === player.publicKeyHex;

  // Initialize or restore the state machine
  const stateMachine = useMemo(() => {
    if (!battle?.battleId) return new BattleStateMachine();

    // Try to restore from localStorage
    const restored = restoreStateMachine(battle.battleId);
    if (restored && restored.context.battleId === battle.battleId) {
      console.log('Restored battle state machine from localStorage');
      return restored;
    }

    // Create new state machine
    return new BattleStateMachine();
  }, [battle?.battleId]);

  // State derived from state machine
  const [machineState, setMachineState] = useState(() => stateMachine.getState());

  // Helper to update state and persist
  const updateStateMachine = useCallback((event, data) => {
    const success = stateMachine.transition(event, data);
    if (success) {
      const newState = stateMachine.getState();
      setMachineState(newState);
      if (battle?.battleId) {
        persistStateMachine(battle.battleId, stateMachine);
      }
    }
    return success;
  }, [stateMachine, battle?.battleId]);

  // Extract commonly used values from state machine context
  const {
    selectedMove,
    lockedMove,
    myHealth,
    oppHealth,
    myMoveCounts,
    battleExpiry,
    currentView,
    timeLeft,
    currentRound,
    opponentLocked
  } = machineState.context;

  // Helper to select a move
  const selectMove = useCallback((moveIndex) => {
    if (stateMachine.canSelectMove()) {
      stateMachine.context.selectedMove = moveIndex;
      setMachineState(stateMachine.getState());
    }
  }, [stateMachine]);

  // Initialize battle data and handle settlement
  useEffect(() => {
    const initData = async () => {
      // Get current view from client (guaranteed non-null after App.jsx waits for first seed)
      const view = client.getCurrentView();

      // Initialize state machine if we have battle data
      if (battle && stateMachine.isInState(BattleStates.INITIALIZING)) {
        const initialHealth = isPlayerA ?
          { my: battle.player_a_health, opp: battle.player_b_health } :
          { my: battle.player_b_health, opp: battle.player_a_health };

        const initialMoveCounts = isPlayerA ?
          battle.player_a_move_counts : battle.player_b_move_counts;

        updateStateMachine(BattleEvents.INITIALIZED, {
          battleId: battle.battleId,
          myHealth: initialHealth.my,
          oppHealth: initialHealth.opp,
          battleExpiry: battle.expiry || 0,
          currentView: view,
          myMoveCounts: initialMoveCounts || [0, 0, 0, 0, 0],
          currentRound: battle.round !== undefined ? battle.round + 1 : 1
        });
      } else if (battle && !stateMachine.isInState(BattleStates.INITIALIZING)) {
        // State machine was restored from localStorage
        // Update the current view, round, and expiry from fresh battle data
        console.log('State machine restored, current state:', stateMachine.currentState);

        const oldRound = stateMachine.context.currentRound;
        const newRound = battle.round !== undefined ? battle.round + 1 : oldRound;
        const oldExpiry = stateMachine.context.battleExpiry;
        const newExpiry = battle.expiry || oldExpiry;

        stateMachine.context.currentView = view;
        stateMachine.context.timeLeft = battle.expiry ? Math.max(0, battle.expiry - view) : null;
        stateMachine.context.currentRound = newRound;
        stateMachine.context.battleExpiry = newExpiry;

        // If we're in a new round or expiry changed, we need to reset state
        if (newRound > oldRound || newExpiry !== oldExpiry) {
          if (battle.expiry && view <= battle.expiry) {
            // New round hasn't expired yet - allow move selection
            console.log(`Advanced to round ${newRound}, resetting to SELECTING_MOVE`);
            stateMachine.currentState = BattleStates.SELECTING_MOVE;
            stateMachine.context.selectedMove = null;
            stateMachine.context.lockedMove = null;
            stateMachine.context.opponentLocked = false;
            stateMachine.context.settlementInProgress = false;
          } else if (battle.expiry && view > battle.expiry) {
            // New round has expired - need to be in BOTH_LOCKED to settle
            console.log(`Advanced to round ${newRound} but already expired, setting to BOTH_LOCKED`);
            stateMachine.currentState = BattleStates.BOTH_LOCKED;
            stateMachine.context.settlementInProgress = false;
          }
        } else if (stateMachine.isInState(BattleStates.SETTLING) && battle.expiry && view > battle.expiry) {
          // Still in same round but if we're stuck in SETTLING, reset to allow retry
          console.log('Still in SETTLING state from previous session, will allow settle retry');
          stateMachine.context.settlementInProgress = false;
        }

        setMachineState(stateMachine.getState());
      }

      // When past expiry, we must settle - no new moves can be submitted
      if (battle && battle.expiry && view > battle.expiry) {
        console.log(`Past expiry: view=${view}, expiry=${battle.expiry}, state=${stateMachine.currentState}, round=${currentRound}`);

        // Check if we need to settle (and not already settling)
        if (stateMachine.needsSettlement() && !stateMachine.isInState(BattleStates.SETTLING)) {
          console.log('Past expiry - attempting to settle...');

          // Force transition to BOTH_LOCKED state if we're in selecting/locked state
          if (stateMachine.isInState(BattleStates.SELECTING_MOVE) ||
            stateMachine.isInState(BattleStates.MOVE_LOCKED)) {
            // Jump directly to BOTH_LOCKED - we can't accept new moves anyway
            stateMachine.currentState = BattleStates.BOTH_LOCKED;
            setMachineState(stateMachine.getState());
          }

          // Now settle
          try {
            console.log(`Querying seed for expiry view: ${battle.expiry}`);
            const seedResult = await client.querySeed(battle.expiry);
            console.log('Seed query result:', seedResult);

            if (seedResult.found) {
              // Mark as settling
              if (updateStateMachine(BattleEvents.SETTLE_STARTED)) {
                console.log('Submitting settle transaction');
                await client.submitSettle(seedResult.seed.bytes);
                console.log('Settle submitted successfully');
              }
            } else {
              console.log('Seed not found for expiry view:', battle.expiry);
            }
          } catch (err) {
            console.error('Failed to settle:', err);
          }
        }
      }

      setIsLoading(false);
    };
    initData();
  }, [client, battle, isPlayerA, stateMachine, updateStateMachine]);

  // Update time tracking and check for settlement needs
  useEffect(() => {
    if (currentView !== null && battleExpiry !== null && battleExpiry !== 0) {
      const newTimeLeft = Math.max(0, battleExpiry - currentView);
      stateMachine.updateTimeContext(currentView, newTimeLeft);
      setMachineState(stateMachine.getState());

      // Check if we need to settle
      if (stateMachine.needsSettlement()) {
        // Try to settle with historical seed if we're past expiry
        client.querySeed(battleExpiry).then(seedResult => {
          if (seedResult.found && stateMachine.needsSettlement()) {
            updateStateMachine(BattleEvents.SETTLE_STARTED);

            client.submitSettle(seedResult.seed.bytes).then(() => {
              console.log('Settle transaction submitted successfully');
              // Don't update state here - wait for the Settled event
            }).catch(err => {
              console.error('Failed to settle with historical seed:', err);
              updateStateMachine(BattleEvents.ERROR_OCCURRED, { error: err.message });
            });
          } else if (!seedResult.found) {
            console.log('Seed not found for view:', battleExpiry, 'will retry...');
          }
        }).catch(err => {
          console.error('Error fetching historical seed:', err);
        });
      }
    }
  }, [currentView, battleExpiry, client, stateMachine, updateStateMachine]);

  // Helper functions

  const getOpponentMoveName = (moveIndex) => {
    // Move 0 = no move, 1 = defend, 2-4 = attacks
    const moveNames = ['NO MOVE', 'DEFEND', 'ATTACK A', 'ATTACK B', 'ATTACK C'];
    return moveNames[moveIndex] || 'UNKNOWN';
  };

  // Parse creature data
  const parseCreature = useCallback((creatureData) => {
    if (!creatureData) return null;
    return parseCreatureUtil(creatureData, client.wasm);
  }, [client]);

  const creature = account?.creature ? parseCreature(account.creature) : null;

  // Submit move function
  const submitMove = useCallback(async (moveIndex = null) => {
    const move = moveIndex !== null ? moveIndex : selectedMove;
    const myCreature = creature;

    // Check if we can submit
    if (!battle?.battleId) {
      console.error('Cannot submit move: battleId is missing');
      setBattleMessage('Error: Battle not properly initialized');
      return;
    }

    if (!stateMachine.canSubmitMove() || !myCreature || !battleExpiry) {
      console.warn('Cannot submit move in current state');
      return;
    }

    setBattleMessage(`Submitting ${move > 0 ? myCreature.moves.find(m => m.index === move)?.name : 'NO MOVE'}...`);

    try {
      // Convert battleId from hex to bytes
      const battleIdBytes = client.wasm.hexToBytes(battle.battleId);
      const result = await client.submitMove(battleIdBytes, move, battleExpiry);

      if (result.status === 'accepted') {
        updateStateMachine(BattleEvents.MOVE_SUBMITTED, { move });
        setBattleMessage('Move locked! Waiting for decryption...');

        // Check if opponent already locked
        const oppPending = isPlayerA ? battle.player_b_pending : battle.player_a_pending;
        if (oppPending) {
          updateStateMachine(BattleEvents.BOTH_MOVES_LOCKED);
        }
      } else {
        throw new Error(result.reason || 'Move rejected');
      }
    } catch (error) {
      console.error('Failed to submit move:', error);
      setBattleMessage('Failed to submit move!');
      updateStateMachine(BattleEvents.ERROR_OCCURRED, { error: error.message });
    }
  }, [selectedMove, creature, battleExpiry, client, battle, stateMachine, updateStateMachine, isPlayerA]);

  // Use opponent data from the battle object (populated from Matched event)
  // or fetch it if not available (happens on reload)
  useEffect(() => {
    if (!battle) return;

    const loadOpponentData = async () => {
      // First try to use data from battle object (from Matched event)
      if (isPlayerA && battle.player_b_creature) {
        const parsedCreature = parseCreature(battle.player_b_creature);
        if (parsedCreature) {
          setOpponentCreature(parsedCreature);
        }
        if (battle.player_b_stats) {
          setOpponentAccount({ elo: battle.player_b_stats.elo });
        }
      } else if (!isPlayerA && battle.player_a_creature) {
        const parsedCreature = parseCreature(battle.player_a_creature);
        if (parsedCreature) {
          setOpponentCreature(parsedCreature);
        }
        if (battle.player_a_stats) {
          setOpponentAccount({ elo: battle.player_a_stats.elo });
        }
      } else {
        // Data not in battle object (happens on reload), fetch from API
        try {
          const opponentPubKeyHex = isPlayerA ? battle.player_b : battle.player_a;
          const opponentPubKeyBytes = client.wasm.hexToBytes(opponentPubKeyHex);
          const opponentAccountData = await client.getAccount(opponentPubKeyBytes);

          if (opponentAccountData) {
            setOpponentAccount({ elo: opponentAccountData.elo });
            if (opponentAccountData.creature) {
              const parsedCreature = parseCreature(opponentAccountData.creature);
              if (parsedCreature) {
                setOpponentCreature(parsedCreature);
              }
            }
          }
        } catch (error) {
          console.error('Failed to fetch opponent data:', error);
        }
      }
    };

    loadOpponentData();
  }, [battle, isPlayerA, parseCreature, client]);

  // Check if we have a pending move on mount
  useEffect(() => {
    if (!battle) return;

    // Only check pending moves if we're in a state that can handle them
    if (!stateMachine.isInState(BattleStates.SELECTING_MOVE) &&
      !stateMachine.isInState(BattleStates.MOVE_LOCKED)) return;

    const myPending = isPlayerA ? battle.player_a_pending : battle.player_b_pending;
    const oppPending = isPlayerA ? battle.player_b_pending : battle.player_a_pending;

    if (myPending && stateMachine.isInState(BattleStates.SELECTING_MOVE)) {
      // We already have a locked move
      updateStateMachine(BattleEvents.MOVE_SUBMITTED, { move: null });
      setBattleMessage('Move locked! Waiting for decryption...');

      if (oppPending) {
        updateStateMachine(BattleEvents.BOTH_MOVES_LOCKED);
      }
    } else if (myPending && oppPending && stateMachine.isInState(BattleStates.MOVE_LOCKED)) {
      // Both have locked while we were in MOVE_LOCKED state
      updateStateMachine(BattleEvents.BOTH_MOVES_LOCKED);
    }

    if (oppPending && !myPending && stateMachine.isInState(BattleStates.SELECTING_MOVE)) {
      updateStateMachine(BattleEvents.OPPONENT_LOCKED);
    }
  }, [battle, isPlayerA, stateMachine, updateStateMachine]);

  // Subscribe to Seed events for time tracking and settling
  useEffect(() => {
    if (!battle?.battleId) return;

    let cleanup = null;

    // Check if WebSocket connection is ready
    if (!client.updatesWs || client.updatesWs.readyState !== WebSocket.OPEN) {
      const checkConnection = setInterval(() => {
        if (client.updatesWs && client.updatesWs.readyState === WebSocket.OPEN) {
          clearInterval(checkConnection);
          cleanup = setupEventHandler();
        }
      }, 100);

      return () => {
        clearInterval(checkConnection);
        if (cleanup) cleanup();
      };
    } else {
      cleanup = setupEventHandler();
    }

    function setupEventHandler() {
      const handleSeedEvent = async (event) => {
        // Update current view in state machine
        const newTimeLeft = battleExpiry ? Math.max(0, battleExpiry - event.view) : null;
        stateMachine.updateTimeContext(event.view, newTimeLeft);
        setMachineState(stateMachine.getState());

        // Store the seed bytes for this view
        if (event.bytes) {
          seedsRef.current.set(event.view, event.bytes);
        }

        const currentBattleExpiry = stateMachine.context.battleExpiry;
        if (!currentBattleExpiry || event.view < currentBattleExpiry) {
          return;
        }

        // When past expiry, ensure we're in the right state and settle
        if (stateMachine.isInState(BattleStates.SELECTING_MOVE) ||
          stateMachine.isInState(BattleStates.MOVE_LOCKED)) {
          // We're past expiry but not in BOTH_LOCKED - force transition
          console.log('Seed event: Past expiry, forcing BOTH_LOCKED state');
          stateMachine.currentState = BattleStates.BOTH_LOCKED;
          setMachineState(stateMachine.getState());
        }

        // Check if we should settle (needsSettlement now handles max rounds check)
        if (!stateMachine.needsSettlement() && !stateMachine.isInState(BattleStates.SETTLING)) {
          return;
        }

        // If already settling, don't try again
        if (stateMachine.isInState(BattleStates.SETTLING)) {
          return;
        }

        // Check if we have the seed bytes for the exact expiry view
        const expirySeedBytes = seedsRef.current.get(currentBattleExpiry);

        if (expirySeedBytes) {
          // We have the seed bytes for the expiry view
          if (updateStateMachine(BattleEvents.SETTLE_STARTED)) {
            try {
              console.log('Submitting settle with cached seed for view:', currentBattleExpiry);
              await client.submitSettle(expirySeedBytes);
              // Don't update state here - wait for the Settled/Moved event
            } catch (err) {
              console.error('Failed to settle:', err);
              updateStateMachine(BattleEvents.ERROR_OCCURRED, { error: err.message });
            }
          }
        } else {
          // Need to fetch the seed
          try {
            console.log('Fetching seed for view:', currentBattleExpiry);
            const seedResult = await client.querySeed(currentBattleExpiry);
            if (seedResult.found && updateStateMachine(BattleEvents.SETTLE_STARTED)) {
              console.log('Submitting settle with fetched seed for view:', currentBattleExpiry);
              await client.submitSettle(seedResult.seed.bytes);
              // Don't update state here - wait for the Settled/Moved event
            } else if (!seedResult.found) {
              console.log('Seed not yet available for view:', currentBattleExpiry);
            }
          } catch (err) {
            console.error('Error fetching/submitting seed:', err);
            updateStateMachine(BattleEvents.ERROR_OCCURRED, { error: err.message });
          }
        }
      };

      client.onEvent('Seed', handleSeedEvent);

      return () => {
        if (client.eventHandlers && client.eventHandlers.has('Seed')) {
          const handlers = client.eventHandlers.get('Seed');
          const index = handlers.indexOf(handleSeedEvent);
          if (index > -1) {
            handlers.splice(index, 1);
          }
        }
      };
    }

    if (client.updatesWs && client.updatesWs.readyState === WebSocket.OPEN) {
      return setupEventHandler();
    }
  }, [client, battle, isPlayerA, battleExpiry, stateMachine, updateStateMachine]);

  // Subscribe to Moved events
  useEffect(() => {
    if (!client || !battle || !account?.creature) return;

    let cleanup = null;
    let checkConnection = null;

    // Wait for WebSocket connection if needed
    if (!client.updatesWs || client.updatesWs.readyState !== WebSocket.OPEN) {
      checkConnection = setInterval(() => {
        if (client.updatesWs && client.updatesWs.readyState === WebSocket.OPEN) {
          clearInterval(checkConnection);
          checkConnection = null;
          cleanup = setupHandler();
        }
      }, 100);
      return () => {
        if (checkConnection) clearInterval(checkConnection);
        if (cleanup) cleanup();
      };
    }

    cleanup = setupHandler();

    function setupHandler() {
      const handleMovedEvent = (event) => {
        if (event.battle !== battle.battleId) return;

        const myCreature = parseCreature(account.creature);
        if (!myCreature) return;

        // Parse move data
        const myMove = isPlayerA ? event.player_a_move : event.player_b_move;
        const myPower = isPlayerA ? event.player_a_power : event.player_b_power;
        const oppMove = isPlayerA ? event.player_b_move : event.player_a_move;
        const oppPower = isPlayerA ? event.player_b_power : event.player_a_power;

        // Use health data directly from the event
        const newMyHealth = isPlayerA ? event.player_a_health : event.player_b_health;
        const newOppHealth = isPlayerA ? event.player_b_health : event.player_a_health;
        const newMoveCounts = isPlayerA ? event.player_a_move_counts : event.player_b_move_counts;

        // Calculate the new round number
        const newRound = event.round !== undefined ? event.round + 1 : currentRound;

        // Update state machine with round results
        updateStateMachine(BattleEvents.ROUND_SETTLED, {
          myHealth: newMyHealth,
          oppHealth: newOppHealth,
          myMoveCounts: newMoveCounts,
          expiry: event.expiry,
          round: newRound
        });

        // Generate move feedback
        const myMoveName = myMove > 0 ? myCreature.moves.find(m => m.index === myMove)?.name : 'NO MOVE';
        const oppMoveName = oppMove > 0 ? (opponentCreature?.moves?.find(m => m.index === oppMove)?.name || getOpponentMoveName(oppMove)) : 'NO MOVE';

        let resultMessage = '';
        if (myMove === 0) {
          resultMessage = `You did not submit a move!\n`;
        } else if (myMove === 1) {
          resultMessage = `You used ${myMoveName} and recovered ${myPower} HP!\n`;
        } else {
          resultMessage = `You used ${myMoveName} for ${myPower} damage!\n`;
        }

        if (oppMove === 0) {
          resultMessage += `Opponent did not submit a move!`;
        } else if (oppMove === 1) {
          resultMessage += `Opponent used ${oppMoveName} and recovered ${oppPower} HP!`;
        } else {
          resultMessage += `Opponent used ${oppMoveName} for ${oppPower} damage!`;
        }

        setBattleMessage(resultMessage);

        // Clear pending states in battle object
        if (battle) {
          battle.player_a_pending = null;
          battle.player_b_pending = null;
        }

        // Check if we've reached max rounds or if someone has 0 health
        // Use the NEW round number for this check
        // Battle ends AFTER round 15 is complete (when trying to go to round 16)
        const battleShouldEnd = newRound > MAX_BATTLE_ROUNDS || newMyHealth <= 0 || newOppHealth <= 0;

        if (battleShouldEnd) {
          console.log(`Battle should end - round ${newRound}/${MAX_BATTLE_ROUNDS}, myHealth: ${newMyHealth}, oppHealth: ${newOppHealth}`);
          // Stay in ROUND_COMPLETE and wait for Settled event
          // Do NOT reset to SELECTING_MOVE
          // The battle needs to be settled to determine the winner
        } else {
          // Reset for next round immediately
          if (stateMachine.isInState(BattleStates.ROUND_COMPLETE)) {
            updateStateMachine(BattleEvents.RESET_FOR_NEW_ROUND);
          }
        }
      };

      client.onEvent('Moved', handleMovedEvent);

      return () => {
        // Remove the event handler on cleanup
        if (client.eventHandlers && client.eventHandlers.has('Moved')) {
          const handlers = client.eventHandlers.get('Moved');
          const index = handlers.indexOf(handleMovedEvent);
          if (index > -1) {
            handlers.splice(index, 1);
          }
        }
      };
    }

    return () => {
      if (checkConnection) clearInterval(checkConnection);
      if (cleanup) cleanup();
    };
  }, [client, battle?.battleId, account?.creature, isPlayerA, stateMachine, updateStateMachine, opponentCreature]);

  // Subscribe to Locked events
  useEffect(() => {
    if (!client || !battle) return;

    let cleanup = null;
    let checkConnection = null;

    // Wait for WebSocket connection if needed
    if (!client.updatesWs || client.updatesWs.readyState !== WebSocket.OPEN) {
      checkConnection = setInterval(() => {
        if (client.updatesWs && client.updatesWs.readyState === WebSocket.OPEN) {
          clearInterval(checkConnection);
          checkConnection = null;
          cleanup = setupHandler();
        }
      }, 100);
      return () => {
        if (checkConnection) clearInterval(checkConnection);
        if (cleanup) cleanup();
      };
    }

    cleanup = setupHandler();

    function setupHandler() {
      const handleLockedEvent = (event) => {
        if (event.battle !== battle.battleId) return;

        const isMe = event.locker === player.publicKeyHex;

        // Update the battle state based on who locked
        if (isMe) {
          // Update my pending state
          if (isPlayerA) {
            battle.player_a_pending = event.ciphertext;
          } else {
            battle.player_b_pending = event.ciphertext;
          }

          // If we're in SELECTING_MOVE, transition to MOVE_LOCKED
          if (stateMachine.isInState(BattleStates.SELECTING_MOVE)) {
            updateStateMachine(BattleEvents.MOVE_SUBMITTED, { move: null });
          }
        } else {
          // Opponent locked their move
          updateStateMachine(BattleEvents.OPPONENT_LOCKED);

          // Update opponent's pending state
          if (isPlayerA) {
            battle.player_b_pending = event.ciphertext;
          } else {
            battle.player_a_pending = event.ciphertext;
          }

          // Check if both have locked
          if (stateMachine.isInState(BattleStates.MOVE_LOCKED)) {
            updateStateMachine(BattleEvents.BOTH_MOVES_LOCKED);
          } else if (stateMachine.canSelectMove()) {
            setBattleMessage('Opponent has locked their move! Choose wisely...');

            // Reset message after a delay
            setTimeout(() => {
              if (stateMachine.canSelectMove()) {
                setBattleMessage('');
              }
            }, 3000);
          }
        }
      };

      client.onEvent('Locked', handleLockedEvent);

      return () => {
        // Remove the event handler on cleanup
        if (client.eventHandlers && client.eventHandlers.has('Locked')) {
          const handlers = client.eventHandlers.get('Locked');
          const index = handlers.indexOf(handleLockedEvent);
          if (index > -1) {
            handlers.splice(index, 1);
          }
        }
      };
    }

    return () => {
      if (checkConnection) clearInterval(checkConnection);
      if (cleanup) cleanup();
    };
  }, [client, battle?.battleId, player.publicKeyHex, isPlayerA, stateMachine, updateStateMachine]);

  // Subscribe to Settled events
  useEffect(() => {
    if (!client || !battle) return;

    let cleanup = null;
    let checkConnection = null;

    // Wait for WebSocket connection if needed
    if (!client.updatesWs || client.updatesWs.readyState !== WebSocket.OPEN) {
      checkConnection = setInterval(() => {
        if (client.updatesWs && client.updatesWs.readyState === WebSocket.OPEN) {
          clearInterval(checkConnection);
          checkConnection = null;
          cleanup = setupHandler();
        }
      }, 100);
      return () => {
        if (checkConnection) clearInterval(checkConnection);
        if (cleanup) cleanup();
      };
    }

    cleanup = setupHandler();

    function setupHandler() {
      const handleSettledEvent = (event) => {
        if (event.battle !== battle.battleId) return;

        let message = '';
        if (event.outcome === 'PlayerA') {
          message = isPlayerA ? 'YOU WIN!' : 'YOU LOSE!';
        } else if (event.outcome === 'PlayerB') {
          message = isPlayerA ? 'YOU LOSE!' : 'YOU WIN!';
        } else {
          message = 'DRAW!';
        }

        setBattleMessage(message);
        updateStateMachine(BattleEvents.BATTLE_SETTLED, { outcome: event.outcome });

        // Clear localStorage for this battle
        if (battle?.battleId) {
          clearPersistedState(battle.battleId);
        }

        // Clear stored seeds to free memory
        seedsRef.current.clear();
      };

      client.onEvent('Settled', handleSettledEvent);

      return () => {
        // Remove the event handler on cleanup
        if (client.eventHandlers && client.eventHandlers.has('Settled')) {
          const handlers = client.eventHandlers.get('Settled');
          const index = handlers.indexOf(handleSettledEvent);
          if (index > -1) {
            handlers.splice(index, 1);
          }
        }
      };
    }

    return () => {
      if (checkConnection) clearInterval(checkConnection);
      if (cleanup) cleanup();
    };
  }, [client, battle?.battleId, isPlayerA, updateStateMachine]);

  const handleSelectMove = (moveIndex) => {
    if (stateMachine.canSelectMove()) {
      selectMove(moveIndex);
      // Submit immediately
      submitMove(moveIndex);
    }
  };

  if (isLoading || !creature || !battle) {
    return null;
  }

  // Battle View
  return (
    <div className="min-h-screen bg-retro-blue flex items-center justify-center p-4 sm:p-8">
      <div className="max-w-[800px] w-full">
        {/* Battle Screen */}
        <div className="bg-retro-blue p-2 sm:p-4 mx-auto max-w-[800px]">
          <div className="bg-retro-white border-4 border-retro-white">
            {/* Battle Arena */}
            <div className="h-[260px] sm:h-[380px] lg:h-[450px] relative bg-retro-blue">
              {/* Opponent (Top Right) */}
              <div className="absolute top-1 sm:top-4 lg:top-6 right-2 sm:right-4 lg:right-6 flex flex-col items-end">
                <div className="bg-retro-blue border-4 border-retro-white p-1 sm:p-2 lg:p-3 mb-2 sm:mb-4 min-w-[160px] sm:min-w-[220px]">
                  <div>
                    <div className="flex items-center justify-between gap-2">
                      <RetroText className="text-xs sm:text-sm lg:text-base font-bold text-retro-white">{opponentCreature?.name || 'OPPONENT'}</RetroText>
                      {opponentLocked && !stateMachine.isInState(BattleStates.BATTLE_ENDED) && (
                        <div className="bg-retro-white text-retro-blue px-1 py-0 border-2 border-retro-white flex-shrink-0">
                          <RetroText className="text-[8px] sm:text-[10px] lg:text-xs font-bold text-retro-blue">LOCKED</RetroText>
                        </div>
                      )}
                    </div>
                    <div className="flex items-center gap-1 sm:gap-2 mt-1">
                      <span className="text-xs sm:text-sm font-retro text-retro-white">HP</span>
                      <div className="bg-retro-blue h-2 sm:h-3 border-2 sm:border-4 border-retro-white w-[120px] sm:w-[200px] relative overflow-hidden">
                        <div
                          className="h-full bg-retro-white transition-all duration-300"
                          style={{ width: `${((oppHealth ?? opponentCreature?.health ?? 0) / (opponentCreature?.health || 1)) * 100}%` }}
                        />
                      </div>
                      <span className="text-xs sm:text-sm font-retro ml-1 sm:ml-2 text-retro-white">{oppHealth ?? opponentCreature?.health ?? 0}/{opponentCreature?.health || 0}</span>
                    </div>
                    {opponentAccount && (
                      <>
                        <RetroText className="text-xs mt-1 text-retro-white">TRAINER: {generateTrainerName(isPlayerA ? battle.player_b : battle.player_a)}</RetroText>
                        <RetroText className="text-xs text-retro-white">ELO: {opponentAccount.elo}</RetroText>
                      </>
                    )}
                  </div>
                </div>
                <pre className="text-xs sm:text-sm lg:text-base leading-tight font-retro text-retro-white text-center">
                  {opponentCreature ? opponentCreature.ascii.join('\n') : '  ????  \n ????? \n???????\n ????? \n  ? ?  '}
                </pre>
              </div>

              {/* Player (Bottom Left) */}
              <div className="absolute bottom-1 sm:bottom-4 lg:bottom-6 left-2 sm:left-4 lg:left-6 flex flex-col items-start">
                <pre className="text-xs sm:text-sm lg:text-base leading-tight font-retro text-retro-white text-center mb-2 sm:mb-4">
                  {creature.ascii.join('\n')}
                </pre>
                <div className="bg-retro-blue border-4 border-retro-white p-1 sm:p-2 lg:p-3 min-w-[160px] sm:min-w-[220px]">
                  <div>
                    <div className="flex items-center justify-between gap-2">
                      <RetroText className="text-xs sm:text-sm lg:text-base font-bold text-retro-white">{creature.name}</RetroText>
                      {(stateMachine.isInState(BattleStates.MOVE_LOCKED) ||
                        stateMachine.isInState(BattleStates.BOTH_LOCKED) ||
                        stateMachine.isInState(BattleStates.SETTLING)) && (
                          <div className="bg-retro-white text-retro-blue px-1 py-0 border-2 border-retro-white flex-shrink-0">
                            <RetroText className="text-[8px] sm:text-[10px] lg:text-xs font-bold text-retro-blue">LOCKED</RetroText>
                          </div>
                        )}
                    </div>
                    <div className="flex items-center gap-1 sm:gap-2 mt-1">
                      <span className="text-xs sm:text-sm font-retro text-retro-white">HP</span>
                      <div className="bg-retro-blue h-2 sm:h-3 border-2 sm:border-4 border-retro-white w-[120px] sm:w-[200px] relative overflow-hidden">
                        <div
                          className="h-full bg-retro-white transition-all duration-300"
                          style={{ width: `${((myHealth ?? creature.health) / creature.health) * 100}%` }}
                        />
                      </div>
                      <span className="text-xs sm:text-sm font-retro ml-1 sm:ml-2 text-retro-white">{myHealth ?? creature.health}/{creature.health}</span>
                    </div>
                    {account && (
                      <>
                        <RetroText className="text-xs mt-1 text-retro-white">TRAINER: {generateTrainerName(player.publicKeyHex)}</RetroText>
                        <RetroText className="text-xs text-retro-white">ELO: {account.elo}</RetroText>
                      </>
                    )}
                  </div>
                </div>
              </div>
            </div>

            {/* Text Box */}
            <div className="border-t-4 border-retro-white bg-retro-blue p-1 sm:p-4">
              <div className="border-4 border-retro-white bg-retro-blue p-1 sm:p-4 h-[100px] sm:h-[140px] lg:h-[160px] flex items-center">
                {battleMessage ? (
                  <RetroText className="text-xs sm:text-base lg:text-xl whitespace-pre-line text-retro-white">{battleMessage}</RetroText>
                ) : (() => {
                  // Show message based on current state
                  if (stateMachine.isInState(BattleStates.BATTLE_ENDED)) {
                    return <RetroText className="text-xs sm:text-base lg:text-xl text-retro-white">Battle Over!</RetroText>;
                  } else if (stateMachine.isInState(BattleStates.ROUND_COMPLETE)) {
                    return <RetroText className="text-xs sm:text-base lg:text-xl text-retro-white">{battleMessage || 'Preparing next round...'}</RetroText>;
                  } else if (stateMachine.isInState(BattleStates.SETTLING)) {
                    return <RetroText className="text-xs sm:text-base lg:text-xl text-retro-white">Settling round...</RetroText>;
                  } else if (stateMachine.isInState(BattleStates.BOTH_LOCKED)) {
                    return <RetroText className="text-xs sm:text-base lg:text-xl text-retro-white">Move locked! Waiting for decryption...</RetroText>;
                  } else if (stateMachine.isInState(BattleStates.MOVE_LOCKED)) {
                    return <RetroText className="text-xs sm:text-base lg:text-xl text-retro-white">Move locked! Waiting for opponent...</RetroText>;
                  } else if (stateMachine.isInState(BattleStates.SELECTING_MOVE) && opponentLocked) {
                    return <RetroText className="text-xs sm:text-base lg:text-xl text-retro-white">Opponent has locked their move! Choose wisely...</RetroText>;
                  } else {
                    return <RetroText className="text-xs sm:text-base lg:text-xl text-retro-white">What will {creature.name} do?</RetroText>;
                  }
                })()}
              </div>
            </div>
          </div>
        </div>

        {/* Move Menu */}
        {(stateMachine.isInState(BattleStates.MOVE_LOCKED) ||
          stateMachine.isInState(BattleStates.BOTH_LOCKED) ||
          stateMachine.isInState(BattleStates.SETTLING) ||
          (stateMachine.isInState(BattleStates.ROUND_COMPLETE) && currentRound > MAX_BATTLE_ROUNDS)) ? (
          <div className="bg-retro-blue p-2 sm:p-4 mx-auto max-w-[800px] mt-1">
            <div className="bg-retro-white border-4 border-retro-white flex flex-col-reverse min-h-[300px] sm:min-h-[350px] w-full">
              <div className="flex-1 p-4 sm:p-6 flex flex-col justify-center items-center">
                <div className="flex items-center justify-center gap-2 sm:gap-4 mb-3 sm:mb-4">
                  <pre className="text-sm sm:text-base inline-block leading-tight font-retro text-retro-blue">
                    {`[▓▓]
 ▒▒`}</pre>
                  <RetroText className="text-lg sm:text-xl lg:text-2xl text-retro-blue">Move locked!</RetroText>
                </div>
                <RetroText className="text-base sm:text-lg lg:text-xl text-center mt-2 sm:mt-3 text-retro-blue">
                  {lockedMove !== null && lockedMove > 0 ? creature.moves.find(m => m.index === lockedMove)?.name : 'NO MOVE'}
                </RetroText>
                <div className="mt-4 sm:mt-6 text-center">
                  <div className="inline-block bg-retro-blue text-retro-white px-4 sm:px-6 py-1 sm:py-2 border-4 border-retro-white">
                    <RetroText className="text-sm sm:text-base text-retro-white">DECRYPTING...</RetroText>
                  </div>
                </div>
              </div>
              <div className="w-full border-b-4 border-retro-blue p-3 sm:p-4 bg-retro-white">
                {/* Unified compact view for all screen sizes */}
                <div>
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-3">
                      <div>
                        <RetroText className="text-xs font-bold text-retro-blue">TIME</RetroText>
                        <div className="text-xl sm:text-2xl font-retro tabular-nums text-retro-blue">{timeLeft === null || isNaN(timeLeft) ? '...' : timeLeft}</div>
                      </div>
                      <div className="h-8 w-px bg-retro-blue"></div>
                      <div>
                        <RetroText className="text-xs font-bold text-retro-blue">VIEW</RetroText>
                        <div className="text-xs sm:text-sm font-retro tabular-nums text-retro-blue">{currentView || '...'}</div>
                      </div>
                    </div>
                    <div className="text-right">
                      <RetroText className="text-xs font-bold text-retro-blue">ROUND</RetroText>
                      <RetroText className="text-xs font-bold text-retro-blue">{currentRound}/{MAX_BATTLE_ROUNDS}</RetroText>
                    </div>
                  </div>
                  <div className="bg-retro-white h-2 w-full border-2 border-retro-blue mt-2">
                    <div
                      className="bg-retro-blue h-full transition-all duration-300"
                      style={{ width: `${timeLeft === null || isNaN(timeLeft) ? 0 : Math.min(100, (timeLeft / TURN_TIME) * 100)}%` }}
                    />
                  </div>
                </div>
              </div>
            </div>
          </div>
        ) : (stateMachine.isInState(BattleStates.SELECTING_MOVE) ||
          stateMachine.isInState(BattleStates.INITIALIZING)) ? (
          <div className="bg-retro-blue p-2 sm:p-4 mx-auto max-w-[800px] mt-1">
            <div className="bg-retro-white border-4 border-retro-white flex flex-col-reverse min-h-[300px] sm:min-h-[350px] w-full">
              <div className="flex-1 p-2 sm:p-4 grid grid-cols-1 gap-2 sm:gap-3">
                {creature.moves.map((move, idx) => {
                  // move.index is 1-4, myMoveCounts array is indexed 0-4
                  const usageCount = myMoveCounts ? myMoveCounts[move.index] : 0;
                  const canUse = usageCount < move.usageLimit;
                  const moveIndex = move.index; // Use the move's actual index (1-4)

                  return (
                    <button
                      key={idx}
                      onClick={() => handleSelectMove(moveIndex)}
                      disabled={!canUse}
                      className={`text-left transition-all ${selectedMove === moveIndex
                        ? 'border-4 border-retro-white bg-retro-white text-retro-blue p-2 sm:p-3'
                        : !canUse
                          ? 'border-0 bg-textured-blue cursor-not-allowed p-3 sm:p-4'
                          : 'border-4 border-retro-blue bg-retro-blue text-retro-white hover:bg-retro-white hover:text-retro-blue hover:border-retro-blue group p-2 sm:p-3'
                        }`}
                    >
                      <div className="flex justify-between items-center gap-2">
                        <RetroText className={`text-xs sm:text-sm font-bold flex-1 ${selectedMove === moveIndex ? 'text-retro-blue' : canUse ? 'text-retro-white group-hover:text-retro-blue' : 'text-retro-white'}`}>{move.name}</RetroText>
                        <div className="text-right">
                          <RetroText className={`text-[10px] sm:text-xs block ${selectedMove === moveIndex ? 'text-retro-blue' : canUse ? 'text-retro-white group-hover:text-retro-blue' : 'text-retro-white'}`}>
                            {move.isDefense ? 'REC' : 'PWR'}: {move.strength}
                          </RetroText>
                          <RetroText className={`text-[10px] sm:text-xs font-bold block ${selectedMove === moveIndex ? 'text-retro-blue' : canUse ? 'text-retro-white group-hover:text-retro-blue' : 'text-retro-white'}`}>
                            PP: {move.usageLimit - usageCount}/{move.usageLimit}
                          </RetroText>
                        </div>
                      </div>
                    </button>
                  );
                })}
              </div>
              <div className="w-full border-b-4 border-retro-blue p-3 sm:p-4 bg-retro-white">
                {/* Unified compact view for all screen sizes */}
                <div>
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-3">
                      <div>
                        <RetroText className="text-xs font-bold text-retro-blue">TIME</RetroText>
                        <div className="text-xl sm:text-2xl font-retro tabular-nums text-retro-blue">{timeLeft === null || isNaN(timeLeft) ? '...' : timeLeft}</div>
                      </div>
                      <div className="h-8 w-px bg-retro-blue"></div>
                      <div>
                        <RetroText className="text-xs font-bold text-retro-blue">VIEW</RetroText>
                        <div className="text-xs sm:text-sm font-retro tabular-nums text-retro-blue">{currentView || '...'}</div>
                      </div>
                    </div>
                    <div className="text-right">
                      <RetroText className="text-xs font-bold text-retro-blue">ROUND</RetroText>
                      <RetroText className="text-xs font-bold text-retro-blue">{currentRound}/{MAX_BATTLE_ROUNDS}</RetroText>
                    </div>
                  </div>
                  <div className="bg-retro-white h-2 w-full border-2 border-retro-blue mt-2">
                    <div
                      className="bg-retro-blue h-full transition-all duration-300"
                      style={{ width: `${timeLeft === null || isNaN(timeLeft) ? 0 : Math.min(100, (timeLeft / TURN_TIME) * 100)}%` }}
                    />
                  </div>
                </div>
              </div>
            </div>
          </div>
        ) : null}
      </div>
    </div>
  );
};

export default BattleScreen;