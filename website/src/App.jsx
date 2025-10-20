import React, { useState, useEffect } from 'react';
import { BattlewareClient } from './api/client';
import { WasmWrapper } from './api/wasm';
import TitleScreen from './components/TitleScreen';
import CharacterGeneration from './components/CharacterGeneration';
import MatchmakingScreen from './components/MatchmakingScreen';
import BattleScreen from './components/BattleScreen';
import GameOverScreen from './components/GameOverScreen';
import LoadingScreen from './components/LoadingScreen';
import EventExplorer from './components/EventExplorer';
import MaintenancePage from './components/MaintenancePage';

function App() {
  const [gameState, setGameState] = useState('loading');
  const [client, setClient] = useState(null);
  const [player, setPlayer] = useState(null);
  const [account, setAccount] = useState(null);
  const [battle, setBattle] = useState(null);
  const [settlementEvent, setSettlementEvent] = useState(null);

  // Initialize client on mount with high priority
  useEffect(() => {
    // Mark this as a high-priority task for the browser
    if ('scheduler' in window && 'postTask' in window.scheduler) {
      // Use the Prioritized Task Scheduling API if available
      window.scheduler.postTask(initClient, { priority: 'user-blocking' });
    } else {
      // Fall back to immediate execution
      initClient();
    }
    
    async function initClient() {
      try {
        // Get identity from environment variable (required)
        const identity = import.meta.env.VITE_IDENTITY;
        if (!identity) {
          throw new Error('VITE_IDENTITY environment variable is required. Please set it when building or running the app.');
        }
        console.log('Using identity for seed verification:', identity);

        // Get backend URL from environment variable (required)
        const backendUrl = import.meta.env.VITE_URL;
        if (!backendUrl) {
          throw new Error('VITE_URL environment variable is required. Please set it when building or running the app.');
        }
        console.log('Using backend URL:', backendUrl);

        // Create WasmWrapper instance
        const wasmWrapper = new WasmWrapper(identity);

        // Create client with WasmWrapper
        const battlewareClient = new BattlewareClient(backendUrl, wasmWrapper);
        await battlewareClient.init();

        // Get or create keypair
        const keypair = battlewareClient.getOrCreateKeypair();

        // Set player state immediately
        const playerData = {
          publicKey: keypair.publicKey,
          publicKeyHex: keypair.publicKeyHex
        };
        setPlayer(playerData);

        // Now do parallel operations for seed and account
        const [, existingAccount] = await Promise.all([
          // Wait for first seed
          (async () => {
            // Connect WebSocket with account filter using the public key
            console.log('Attempting to connect to WebSocket updates...');
            await battlewareClient.connectUpdates(keypair.publicKey);
            console.log('WebSocket updates connected successfully');

            // Wait for first seed
            console.log('Waiting for first seed...');
            await battlewareClient.waitForFirstSeed();
            console.log('First seed received, view:', battlewareClient.getCurrentView());
          })(),

          // Account read
          (async () => {
            console.log('Fetching account data...');
            const account = await battlewareClient.getAccount(keypair.publicKey);
            console.log('Account fetch complete:', account ? 'found' : 'not found');
            return account;
          })()
        ]);

        // Initialize nonce manager with the account data
        await battlewareClient.initNonceManager(keypair.publicKeyHex, keypair.publicKey, existingAccount);
        if (existingAccount) {
          // Set account state
          setAccount(existingAccount);

          // Check if in battle
          if (existingAccount.battle) {
            const battleHex = existingAccount.battle;
            const battleBytes = battlewareClient.wasm.hexToBytes(battleHex);
            const battleData = await battlewareClient.getBattle(battleBytes);
            if (battleData) {
              setBattle({
                battleId: battleHex,
                ...battleData
              });
              setGameState('battle');
            } else {
              setGameState('title');
            }
          } else {
            setGameState('title');
          }
        } else {
          setGameState('title');
        }

        // Set client state
        setClient(battlewareClient);

        // Don't set up event handlers here - we'll do it in a separate useEffect
      } catch (err) {
        console.error('Failed to initialize client:', err);
        console.error('Error details:', err?.message, err?.stack);
        setGameState('maintenance');
      }
    }

    // Cleanup function
    return () => {
      if (client) {
        client.destroy();
      }
    };
  }, []);

  // Set up event handler after client and player are initialized
  useEffect(() => {
    if (!client || !player) return;


    // Set up event handler with current player value in closure
    const handler = (event) => {
      switch (event.type) {
        case 'Generated':

          if (event.account === player.publicKeyHex) {
            // Update account with new creature from the event
            setAccount(prevAccount => ({
              ...prevAccount,
              creature: event.creature
            }));
          }
          break;

        case 'Matched':
          // Check if we're in this match
          if (event.player_a === player.publicKeyHex || event.player_b === player.publicKeyHex) {
            // Update our account stats from the Matched event
            const isPlayerA = event.player_a === player.publicKeyHex;
            const ourStats = isPlayerA ? event.player_a_stats : event.player_b_stats;

            // Update account with latest stats from the Matched event
            setAccount(prevAccount => ({
              ...prevAccount,
              elo: ourStats.elo,
              wins: ourStats.wins,
              losses: ourStats.losses,
              draws: ourStats.draws,
              battle: event.battle
            }));

            // Parse creatures to get health values
            const playerACreature = client.wasm.generateCreatureFromTraits(new Uint8Array(event.player_a_creature.traits));
            const playerBCreature = client.wasm.generateCreatureFromTraits(new Uint8Array(event.player_b_creature.traits));

            // Use data directly from the event - no need to fetch battle data
            setBattle({
              battleId: event.battle,
              player_a: event.player_a,
              player_b: event.player_b,
              expiry: event.expiry,
              // Include creature and stats data from the event
              player_a_creature: event.player_a_creature,
              player_a_stats: event.player_a_stats,
              player_b_creature: event.player_b_creature,
              player_b_stats: event.player_b_stats,
              // Initialize battle state fields with actual creature health
              player_a_health: playerACreature.health,
              player_b_health: playerBCreature.health,
              player_a_move_counts: [0, 0, 0, 0, 0],
              player_b_move_counts: [0, 0, 0, 0, 0],
              player_a_pending: null,
              player_b_pending: null,
              round: 0
            });
            setGameState('battle');
          }
          break;

        case 'Moved':
          // BattleScreen handles Moved events internally now
          break;

        case 'Settled':
          // Game over
          if (battle && event.battle === battle.battleId) {
            // Store the outcome with the battle
            setBattle({
              ...battle,
              outcome: event.outcome
            });
            // Store the settlement event for ELO change display
            setSettlementEvent(event);
            setGameState('gameover');

            // Update account stats from the event
            if (event.player_a === player.publicKeyHex) {
              setAccount(prevAccount => ({
                ...prevAccount,
                elo: event.player_a_new.elo,
                wins: event.player_a_new.wins,
                losses: event.player_a_new.losses,
                draws: event.player_a_new.draws,
                battle: null  // Battle is over
              }));
            } else if (event.player_b === player.publicKeyHex) {
              setAccount(prevAccount => ({
                ...prevAccount,
                elo: event.player_b_new.elo,
                wins: event.player_b_new.wins,
                losses: event.player_b_new.losses,
                draws: event.player_b_new.draws,
                battle: null  // Battle is over
              }));
            }
          }
          break;
      }
    };

    client.onEvent('*', handler);

    // Return cleanup function to remove handler
    return () => {
      // Remove the handler on cleanup
      if (client.eventHandlers && client.eventHandlers.has('*')) {
        const handlers = client.eventHandlers.get('*');
        const index = handlers.indexOf(handler);
        if (index > -1) {
          handlers.splice(index, 1);
        }
      }
    };
  }, [client, player, battle]);


  const startNewGame = () => {
    if (!account || !account.creature) {
      setGameState('chargen');
    } else {
      setGameState('matchmaking');
    }
  };

  const generateCharacter = async () => {
    if (!client) {
      console.error('Client not initialized');
      return;
    }

    try {
      const result = await client.submitGenerate();

      if (result.status !== 'accepted') {
        throw new Error(result.error || 'Transaction rejected');
      }

      // The event handler in handleEvent will update the UI when the Generated event arrives
    } catch (err) {
      console.error('Failed to generate character:', err);
    }
  };

  const enterMatchmaking = async () => {
    if (!client || !account) return;

    try {
      const result = await client.submitMatch();

      if (result.status !== 'accepted') {
        throw new Error(result.error || 'Transaction rejected');
      }

      // Wait for match event
    } catch (err) {
      console.error('Failed to enter matchmaking:', err);
    }
  };

  if (gameState === 'loading') {
    return <LoadingScreen />;
  }

  if (gameState === 'maintenance') {
    return <MaintenancePage />;
  }

  return (
    <div className="min-h-screen bg-retro-blue flex items-center justify-center p-2 sm:p-4 lg:p-8">
      <div className="w-full max-w-[800px]">
        {gameState === 'title' && (
          <TitleScreen
            account={account}
            publicKeyHex={player?.publicKeyHex}
            onStart={startNewGame}
            onRegenerate={() => setGameState('viewcreature')}
            onExplore={() => setGameState('explore')}
            client={client}
          />
        )}

        {gameState === 'chargen' && (
          <CharacterGeneration
            onGenerate={generateCharacter}
            account={account}
            onContinue={() => setGameState('matchmaking')}
            showExisting={false}
            onBack={() => setGameState('title')}
          />
        )}

        {gameState === 'viewcreature' && (
          <CharacterGeneration
            onGenerate={generateCharacter}
            account={account}
            onContinue={() => setGameState('matchmaking')}
            showExisting={true}
            onBack={() => setGameState('title')}
          />
        )}

        {gameState === 'matchmaking' && (
          <MatchmakingScreen
            onMatch={enterMatchmaking}
            account={account}
          />
        )}

        {gameState === 'battle' && (
          <BattleScreen
            client={client}
            player={player}
            account={account}
            battle={battle}
          />
        )}

        {gameState === 'gameover' && (
          <GameOverScreen
            account={account}
            player={player}
            battle={battle}
            onPlayAgain={() => {
              setBattle(null);
              setSettlementEvent(null);
              setGameState('matchmaking');
            }}
            onMainMenu={() => {
              setBattle(null);
              setSettlementEvent(null);
              setGameState('title');
            }}
            client={client}
            settlementEvent={settlementEvent}
          />
        )}

        {gameState === 'explore' && (
          <EventExplorer
            client={client}
            onBack={() => setGameState('title')}
          />
        )}
      </div>
    </div>
  );
}

export default App;
