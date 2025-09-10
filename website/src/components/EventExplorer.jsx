import React, { useState, useEffect, useRef } from 'react';
import RetroBox from './RetroBox';
import RetroText from './RetroText';
import { generateTrainerName } from '../utils/trainerUtils';

const EventExplorer = ({ client, onBack }) => {
  const [events, setEvents] = useState([]);
  const [isPaused, setIsPaused] = useState(false);
  const unsubscribeRef = useRef(null);
  const [newEventIds, setNewEventIds] = useState(new Set());
  const MAX_EVENTS = 100; // Keep only 100 most recent events

  useEffect(() => {
    if (!client) return;

    // Switch to 'all' filter when entering explorer
    const switchToAllEvents = async () => {
      try {
        console.log('Switching to all events stream for EventExplorer');
        await client.switchUpdates(null); // null = all events
      } catch (error) {
        console.error('Failed to switch to all events:', error);
      }
    };

    switchToAllEvents();

    // Switch back to account filter when leaving
    return () => {
      const switchToAccountEvents = async () => {
        try {
          console.log('Switching back to account-specific events');
          // Get the current public key from the nonce manager
          const publicKey = client.nonceManager.publicKeyBytes;
          if (!publicKey) {
            throw new Error('No public key available to switch back to account events');
          }
          await client.switchUpdates(publicKey);
        } catch (error) {
          console.error('Failed to switch back to account events:', error);
        }
      };
      switchToAccountEvents();
    };
  }, [client]);

  useEffect(() => {
    if (!client || isPaused) return;

    // Subscribe to all events
    unsubscribeRef.current = client.onEvent('*', (event) => {
      // Filter out Seed events
      if (event.type === 'Seed') return;
      
      const eventId = `${event.type}-${Date.now()}-${Math.random()}`;
      
      setEvents(prev => {
        // Add new event at the beginning (newest first)
        const newEvents = [{ ...event, timestamp: Date.now(), id: eventId }, ...prev];
        // Keep only MAX_EVENTS to prevent memory issues
        if (newEvents.length > MAX_EVENTS) {
          return newEvents.slice(0, MAX_EVENTS);
        }
        return newEvents;
      });
      
      // Mark this event as new for animation
      setNewEventIds(prev => new Set([...prev, eventId]));
      
      // Remove from new events after animation completes
      setTimeout(() => {
        setNewEventIds(prev => {
          const next = new Set(prev);
          next.delete(eventId);
          return next;
        });
      }, 300);
    });

    return () => {
      if (unsubscribeRef.current) {
        unsubscribeRef.current();
      }
    };
  }, [client, isPaused]);

  const renderEventCard = (event) => {
    const time = new Date(event.timestamp).toLocaleTimeString();
    
    switch (event.type) {
      case 'Generated':
        return (
          <div className="border-4 border-retro-blue bg-retro-white p-4">
            <div className="flex justify-between items-start mb-2">
              <RetroText className="text-sm font-bold text-retro-blue">CHARACTER GENERATED</RetroText>
              <RetroText className="text-xs text-retro-blue">{time}</RetroText>
            </div>
            <div className="space-y-1">
              <RetroText className="text-xs text-retro-blue">
                Player: {generateTrainerName(event.account)}
              </RetroText>
              <RetroText className="text-xs text-retro-blue break-all">
                Account: {event.account}
              </RetroText>
              {event.creature_name && (
                <RetroText className="text-xs text-retro-blue">
                  Creature: {event.creature_name}
                </RetroText>
              )}
              {event.creature && (
                <>
                  {!event.creature_name && event.creature.name && (
                    <RetroText className="text-xs text-retro-blue">
                      Creature: {event.creature.name}
                    </RetroText>
                  )}
                  {event.creature.health && (
                    <RetroText className="text-xs text-retro-blue">
                      Health: {event.creature.health} HP
                    </RetroText>
                  )}
                  {event.creature.type && (
                    <RetroText className="text-xs text-retro-blue">
                      Type: {event.creature.type}
                    </RetroText>
                  )}
                  {event.creature.moves && event.creature.moves.length > 0 && (
                    <RetroText className="text-xs text-retro-blue">
                      Moves: {event.creature.moves.length}
                    </RetroText>
                  )}
                </>
              )}
              {event.nonce !== undefined && (
                <RetroText className="text-xs text-retro-blue">
                  Nonce: {event.nonce}
                </RetroText>
              )}
            </div>
          </div>
        );
      
      case 'Matched':
        return (
          <div className="border-4 border-retro-blue bg-retro-white p-4">
            <div className="flex justify-between items-start mb-2">
              <RetroText className="text-sm font-bold text-retro-blue">BATTLE MATCHED</RetroText>
              <RetroText className="text-xs text-retro-blue">{time}</RetroText>
            </div>
            <div className="space-y-1">
              <RetroText className="text-xs text-retro-blue">
                Player A: {generateTrainerName(event.player_a)}
              </RetroText>
              <RetroText className="text-xs text-retro-blue">
                Player B: {generateTrainerName(event.player_b)}
              </RetroText>
              <RetroText className="text-xs text-retro-blue break-all">
                Battle ID: {event.battle}
              </RetroText>
              {event.turn !== undefined && (
                <RetroText className="text-xs text-retro-blue">
                  Starting Turn: {event.turn}
                </RetroText>
              )}
              {event.expiry !== undefined && (
                <RetroText className="text-xs text-retro-blue">
                  First Move Expiry: {event.expiry}
                </RetroText>
              )}
            </div>
          </div>
        );
      
      case 'Moved':
        // Determine move types
        const getMoveType = (moveIndex) => {
          if (moveIndex === 0) return 'DEFEND';
          return `ATTACK ${moveIndex}`;
        };
        
        // Check for KO
        const playerAKO = event.player_a_health === 0;
        const playerBKO = event.player_b_health === 0;
        
        return (
          <div className="border-4 border-retro-blue bg-retro-white p-4">
            <div className="flex justify-between items-start mb-2">
              <RetroText className="text-sm font-bold text-retro-blue">MOVE REVEALED</RetroText>
              <RetroText className="text-xs text-retro-blue">{time}</RetroText>
            </div>
            <div className="space-y-1">
              <RetroText className="text-xs text-retro-blue break-all">
                Battle: {event.battle}
              </RetroText>
              <RetroText className="text-xs text-retro-blue font-bold">
                Round {event.round}
              </RetroText>
              {event.expiry !== undefined && (
                <RetroText className="text-xs text-retro-blue">
                  Next Move Deadline: {event.expiry}
                </RetroText>
              )}
              
              <div className="mt-2 pt-2 border-t-2 border-retro-blue">
                {/* Player A Status */}
                <div className="mb-2">
                  <RetroText className="text-xs text-retro-blue font-bold">
                    PLAYER A {playerAKO && '- KO!'}
                  </RetroText>
                  {event.player_a_health !== undefined && (
                    <RetroText className="text-xs text-retro-blue">
                      Health: {event.player_a_health} HP
                    </RetroText>
                  )}
                  {event.player_a_move !== undefined && (
                    <RetroText className="text-xs text-retro-blue">
                      Move: {getMoveType(event.player_a_move)} {event.player_a_power !== undefined && event.player_a_power > 0 && 
                        `(Power: ${event.player_a_power})`}
                    </RetroText>
                  )}
                </div>
                
                {/* Player B Status */}
                <div>
                  <RetroText className="text-xs text-retro-blue font-bold">
                    PLAYER B {playerBKO && '- KO!'}
                  </RetroText>
                  {event.player_b_health !== undefined && (
                    <RetroText className="text-xs text-retro-blue">
                      Health: {event.player_b_health} HP
                    </RetroText>
                  )}
                  {event.player_b_move !== undefined && (
                    <RetroText className="text-xs text-retro-blue">
                      Move: {getMoveType(event.player_b_move)} {event.player_b_power !== undefined && event.player_b_power > 0 && 
                        `(Power: ${event.player_b_power})`}
                    </RetroText>
                  )}
                </div>
              </div>
            </div>
          </div>
        );
      
      case 'Locked':
        return (
          <div className="border-4 border-retro-blue bg-retro-white p-4">
            <div className="flex justify-between items-start mb-2">
              <RetroText className="text-sm font-bold text-retro-blue">MOVE LOCKED</RetroText>
              <RetroText className="text-xs text-retro-blue">{time}</RetroText>
            </div>
            <div className="space-y-1">
              <RetroText className="text-xs text-retro-blue break-all">
                Battle: {event.battle}
              </RetroText>
              {event.locker && (
                <RetroText className="text-xs text-retro-blue break-all">
                  Locker: {event.locker}
                </RetroText>
              )}
              {event.observer && (
                <RetroText className="text-xs text-retro-blue break-all">
                  Observer: {event.observer}
                </RetroText>
              )}
              {event.round !== undefined && (
                <RetroText className="text-xs text-retro-blue">
                  Round: {event.round}
                </RetroText>
              )}
              {event.ciphertext && (
                <RetroText className="text-xs text-retro-blue break-all">
                  Ciphertext: {event.ciphertext}
                </RetroText>
              )}
            </div>
          </div>
        );
      
      case 'Transaction':
        const getInstructionDisplay = (instruction) => {
          if (typeof instruction === 'string') {
            return instruction;
          }
          if (instruction && typeof instruction === 'object') {
            if (instruction.type) return instruction.type;
            return Object.keys(instruction)[0] || 'Unknown';
          }
          return 'Unknown';
        };
        
        return (
          <div className="border-4 border-retro-white bg-retro-blue p-4">
            <div className="flex justify-between items-start mb-2">
              <RetroText className="text-sm font-bold text-retro-white">TRANSACTION</RetroText>
              <RetroText className="text-xs text-retro-white">{time}</RetroText>
            </div>
            <div className="space-y-1">
              {event.instruction && (
                <RetroText className="text-xs text-retro-white">
                  Instruction: {getInstructionDisplay(event.instruction)}
                </RetroText>
              )}
              {event.nonce !== undefined && (
                <RetroText className="text-xs text-retro-white">
                  Nonce: {event.nonce}
                </RetroText>
              )}
              {event.public && (
                <RetroText className="text-xs text-retro-white break-all">
                  Public: {event.public}
                </RetroText>
              )}
            </div>
          </div>
        );
      
      case 'Settled':
        // Determine winner name based on outcome
        let winnerName = 'DRAW';
        if (event.outcome === 'PlayerA' && event.player_a) {
          winnerName = generateTrainerName(event.player_a);
        } else if (event.outcome === 'PlayerB' && event.player_b) {
          winnerName = generateTrainerName(event.player_b);
        }
        
        // Calculate ELO changes
        const playerAEloChange = event.player_a_new.elo - event.player_a_old.elo;
        const playerBEloChange = event.player_b_new.elo - event.player_b_old.elo;
        
        return (
          <div className="border-4 border-retro-blue bg-retro-white p-4">
            <div className="flex justify-between items-start mb-2">
              <RetroText className="text-sm font-bold text-retro-blue">BATTLE SETTLED</RetroText>
              <RetroText className="text-xs text-retro-blue">{time}</RetroText>
            </div>
            <div className="space-y-1">
              <RetroText className="text-xs text-retro-blue break-all">
                Battle: {event.battle}
              </RetroText>
              <RetroText className="text-xs text-retro-blue font-bold">
                Winner: {winnerName}
              </RetroText>
              {event.round !== undefined && (
                <RetroText className="text-xs text-retro-blue">
                  Rounds: {event.round}
                </RetroText>
              )}
              
              <div className="mt-2 pt-2 border-t-2 border-retro-blue">
                <RetroText className="text-xs text-retro-blue font-bold">
                  {generateTrainerName(event.player_a)} (Player A)
                </RetroText>
                <RetroText className="text-xs text-retro-blue">
                  ELO: {event.player_a_new.elo} ({playerAEloChange > 0 ? '+' : ''}{playerAEloChange})
                </RetroText>
                
                <RetroText className="text-xs text-retro-blue font-bold mt-2">
                  {generateTrainerName(event.player_b)} (Player B)
                </RetroText>
                <RetroText className="text-xs text-retro-blue">
                  ELO: {event.player_b_new.elo} ({playerBEloChange > 0 ? '+' : ''}{playerBEloChange})
                </RetroText>
              </div>
            </div>
          </div>
        );
      
      default:
        return (
          <div className="border-4 border-retro-blue bg-retro-white p-4">
            <div className="flex justify-between items-start mb-2">
              <RetroText className="text-sm font-bold text-retro-blue">
                {event.type?.toUpperCase() || 'UNKNOWN'}
              </RetroText>
              <RetroText className="text-xs text-retro-blue">{time}</RetroText>
            </div>
            <div className="space-y-1">
              {Object.entries(event).filter(([key]) => key !== 'type' && key !== 'timestamp').map(([key, value]) => (
                <RetroText key={key} className="text-xs text-retro-blue">
                  {key}: {typeof value === 'object' ? JSON.stringify(value) : String(value)}
                </RetroText>
              ))}
            </div>
          </div>
        );
    }
  };

  return (
    <div className="bg-retro-blue min-h-screen">
      {/* Fixed Header */}
      <div className="sticky top-0 z-10 bg-retro-blue border-b-4 border-retro-white">
        <RetroBox className="p-4 sm:p-6 lg:p-8 w-full rounded-none border-x-0 border-t-0">
        <div className="flex flex-col sm:flex-row justify-between items-center gap-4">
          <RetroText className="text-xl sm:text-2xl lg:text-3xl font-bold text-retro-white">
            EVENT EXPLORER
          </RetroText>
          
          <div className="flex gap-2">
            <button
              onClick={() => setIsPaused(!isPaused)}
              className="border-4 border-retro-white bg-retro-blue px-4 py-2 hover:bg-retro-white hover:text-retro-blue transition-all text-retro-white group"
            >
              <RetroText className="text-sm font-bold text-retro-white group-hover:text-retro-blue">
                {isPaused ? 'RESUME' : 'PAUSE'}
              </RetroText>
            </button>
            
            <button
              onClick={onBack}
              className="border-4 border-retro-white bg-retro-blue px-4 py-2 hover:bg-retro-white hover:text-retro-blue transition-all text-retro-white group"
            >
              <RetroText className="text-sm font-bold text-retro-white group-hover:text-retro-blue">
                BACK
              </RetroText>
            </button>
          </div>
        </div>
        </RetroBox>
      </div>

      {/* Content Area */}
      <div className="p-2 sm:p-4 lg:p-8">
        {/* Event Stream Content */}
        {events.length === 0 ? (
          <RetroText className="text-retro-white">
            Waiting for events...
          </RetroText>
        ) : (
          <div className="space-y-3">
            {events.map((event, idx) => (
              <div 
                key={event.id || idx}
                className={newEventIds.has(event.id) ? 'animate-slideIn' : ''}
              >
                {renderEventCard(event)}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
};

export default EventExplorer;