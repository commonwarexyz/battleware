import React, { useState, useEffect } from 'react';
import RetroBox from './RetroBox';
import RetroText from './RetroText';
import { WasmWrapper } from '../api/wasm';
import { parseCreature } from '../utils/creatureUtils';
import { generateTrainerName } from '../utils/trainerUtils';

// Define ASCII art as a constant to prevent formatter issues
const TITLE_ASCII = [
  ' \\ | / ',
  '  \\|/  ',
  '--***--',
  '  /|\\  ',
  ' / | \\ '
].join('\n');

const TitleScreen = ({ account, publicKeyHex, onStart, onRegenerate, onExplore, client }) => {
  const [wasm, setWasm] = useState(null);
  const [creature, setCreature] = useState(null);
  const [leaderboard, setLeaderboard] = useState([]);

  // Initialize WASM
  useEffect(() => {
    const initWasm = async () => {
      const wrapper = new WasmWrapper();
      await wrapper.init();
      setWasm(wrapper);
    };
    initWasm();
  }, []);

  // Parse creature when account changes
  useEffect(() => {
    if (account?.creature && wasm) {
      const parsedCreature = parseCreature(account.creature, wasm);
      setCreature(parsedCreature);
    }
  }, [account, wasm]);

  // Fetch leaderboard
  useEffect(() => {
    if (client) {
      const fetchLeaderboard = async () => {
        const players = await client.fetchLeaderboard();
        setLeaderboard(players);
      };
      fetchLeaderboard();
      // Refresh every 30 seconds
      const interval = setInterval(fetchLeaderboard, 30000);
      return () => clearInterval(interval);
    }
  }, [client]);

  return (
    <div className="bg-retro-blue p-2 sm:p-4 lg:p-8 mx-auto w-full space-y-4 sm:space-y-6">
      {/* Title and Start Game Box */}
      <RetroBox className="p-4 sm:p-8 lg:p-10 w-full">
        <div className="text-center">
          <div className="mb-10">
            <RetroText className="text-2xl sm:text-3xl lg:text-5xl font-black">BATTLEWARE</RetroText>
          </div>

          <div className="my-10">
            <div className="inline-block">
              <pre className="text-lg sm:text-2xl lg:text-3xl leading-tight font-retro text-retro-white">
                {TITLE_ASCII}
              </pre>
            </div>
          </div>

          <div className="space-y-3 mb-10 px-2">
            <RetroText className="text-[11px] sm:text-xl lg:text-2xl font-bold">GENERATE • MATCH • BATTLE</RetroText>
            <RetroText className="text-[10px] sm:text-lg lg:text-xl text-retro-white">MAY THE VRF BE IN YOUR FAVOR</RetroText>
            <RetroText className="text-[9px] sm:text-sm lg:text-base text-retro-white">[ 100% ONCHAIN GAMEPLAY ]</RetroText>
          </div>

          <div className="flex flex-col items-center space-y-4">
            <button
              onClick={onStart}
              className="w-full max-w-96 border-4 border-retro-white bg-retro-blue px-6 sm:px-10 py-3 sm:py-5 hover:bg-retro-white hover:text-retro-blue transition-all text-retro-white group"
            >
              <RetroText className="text-lg sm:text-2xl lg:text-3xl font-bold text-retro-white group-hover:text-retro-blue">START</RetroText>
            </button>
            <button
              onClick={onExplore}
              className="w-full max-w-96 border-4 border-retro-white bg-retro-blue px-6 sm:px-10 py-3 sm:py-5 hover:bg-retro-white hover:text-retro-blue transition-all text-retro-white group"
            >
              <RetroText className="text-lg sm:text-2xl lg:text-3xl font-bold text-retro-white group-hover:text-retro-blue">EXPLORE</RetroText>
            </button>
          </div>
        </div>
      </RetroBox>

      {/* Stats Box */}
      {account && (
        <RetroBox className="p-4 sm:p-6 lg:p-8 w-full">
          <RetroText className="text-base sm:text-lg lg:text-xl font-bold mb-3 text-retro-white">YOUR STATS</RetroText>
          <div className="space-y-1">
            <RetroText className="text-xs sm:text-sm lg:text-base text-retro-white">NAME: {generateTrainerName(publicKeyHex)}</RetroText>
            <RetroText className="text-xs sm:text-sm lg:text-base text-retro-white">ELO: {account.elo}</RetroText>
            <RetroText className="text-xs sm:text-sm lg:text-base text-retro-white">RECORD: {account.wins}W - {account.losses}L - {account.draws}D</RetroText>
            <RetroText className="text-xs sm:text-sm lg:text-base text-retro-white">TXS: {localStorage.getItem('battleware_nonce') || '0'}</RetroText>
          </div>
        </RetroBox>
      )}

      {/* Creature Box */}
      {account && creature && (
        <RetroBox className="p-4 sm:p-6 lg:p-8 w-full">
          <div className="flex flex-col sm:flex-row gap-4 sm:gap-6">
            {/* Left side - Creature ASCII */}
            <div className="flex-shrink-0 flex flex-col items-center">
              <RetroText className="text-base sm:text-lg lg:text-xl font-bold text-retro-white mb-2 text-center">{creature.name}</RetroText>
              <div className="inline-flex flex-col">
                <div className="bg-retro-blue border-4 border-retro-white p-3 sm:p-8">
                  <pre className="text-xs sm:text-sm leading-tight font-retro text-retro-white text-center">
                    {creature.ascii.join('\n')}
                  </pre>
                </div>
                <button
                  onClick={onRegenerate}
                  className="mt-3 text-xs border-4 border-retro-white bg-retro-blue px-3 py-2 hover:bg-retro-white hover:text-retro-blue transition-all text-retro-white group"
                >
                  <RetroText className="text-xs font-bold text-retro-white group-hover:text-retro-blue">VIEW</RetroText>
                </button>
              </div>
            </div>

            {/* Right side - Stats */}
            <div className="flex-grow space-y-2">
              {/* Health */}
              <div className="bg-textured-white p-2" style={{ padding: 'calc(0.5rem + 4px)' }}>
                <div className="flex justify-between">
                  <RetroText className="text-xs sm:text-sm font-bold text-retro-blue">HEALTH</RetroText>
                  <RetroText className="text-xs sm:text-sm font-bold text-retro-blue">{creature.health} HP</RetroText>
                </div>
              </div>

              {/* Moves */}
              {creature.moves.map((move, i) => (
                <div key={i} className="bg-retro-blue border-4 border-retro-white p-2">
                  <div className="flex justify-between items-center gap-2">
                    <RetroText className="text-xs sm:text-sm font-bold text-retro-white flex-1">{move.name}</RetroText>
                    <div className="text-right">
                      <RetroText className="text-xs text-retro-white block">
                        {move.isDefense ? 'REC' : 'PWR'}: {move.strength}
                      </RetroText>
                      <RetroText className="text-xs text-retro-white block">
                        PP: {move.usageLimit}
                      </RetroText>
                    </div>
                  </div>
                </div>
              ))}
            </div>
          </div>
        </RetroBox>
      )}

      {/* Leaderboard Box */}
      {leaderboard.length > 0 && (
        <RetroBox className="p-4 sm:p-6 lg:p-8 w-full">
          <RetroText className="text-base sm:text-lg lg:text-xl font-bold mb-3 text-retro-white">TOP PLAYERS</RetroText>
          <div className="space-y-1">
            {leaderboard.slice(0, 10).map((player, idx) => {
              const isCurrentPlayer = player.publicKey.toLowerCase() === publicKeyHex?.toLowerCase();
              return (
                <div
                  key={player.publicKey}
                  className={`flex justify-between text-xs sm:text-sm gap-2 px-2 py-1 ${isCurrentPlayer ? 'bg-retro-white text-retro-blue font-bold' : ''}`}
                >
                  <RetroText className={`${isCurrentPlayer ? 'font-black text-retro-blue' : 'text-retro-white'}`}>
                    #{idx + 1} {generateTrainerName(player.publicKey)}
                  </RetroText>
                  <RetroText className={`${isCurrentPlayer ? 'font-black text-retro-blue' : 'font-bold text-retro-white'}`}>
                    <span>{player.elo}</span>
                    <span className="hidden sm:inline ml-2">({player.wins}W-{player.losses}L-{player.draws}D)</span>
                  </RetroText>
                </div>
              );
            })}
          </div>
        </RetroBox>
      )}

      {/* Footer */}
      <div className="text-center w-full pt-6">
        <RetroText className="text-xs sm:text-sm lg:text-base text-retro-white">(C) 2025 COMMONWARE, INC.</RetroText>
      </div>
    </div>
  );
};

export default TitleScreen;