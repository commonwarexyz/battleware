import React from 'react';
import RetroBox from './RetroBox';
import RetroText from './RetroText';
import { generateTrainerName } from '../utils/trainerUtils';

const GameOverScreen = ({ account, player, battle, onPlayAgain, onMainMenu, client, settlementEvent }) => {
  // Determine if player won
  const playerWon = player && battle && (
    (battle.player_a === player.publicKeyHex && battle.outcome === 'PlayerA') ||
    (battle.player_b === player.publicKeyHex && battle.outcome === 'PlayerB')
  );

  const isDraw = battle?.outcome === 'Draw';

  // Calculate ELO change
  let eloChange = 0;
  if (settlementEvent && player) {
    const isPlayerA = settlementEvent.player_a === player.publicKeyHex;
    if (isPlayerA && settlementEvent.player_a_new && settlementEvent.player_a_old) {
      eloChange = settlementEvent.player_a_new.elo - settlementEvent.player_a_old.elo;
    } else if (settlementEvent.player_b_new && settlementEvent.player_b_old) {
      eloChange = settlementEvent.player_b_new.elo - settlementEvent.player_b_old.elo;
    }
  }

  // Get leaderboard from settlement event
  const leaderboard = settlementEvent?.leaderboard || [];

  return (
    <div className="bg-retro-blue p-4 sm:p-8 mx-auto max-w-[800px]">
      <RetroBox className="p-8 sm:p-16 text-center max-w-[800px] mx-auto">
        <RetroText className="text-3xl sm:text-5xl lg:text-6xl mb-6 sm:mb-10 font-black">
          {isDraw ? 'DRAW!' : playerWon ? 'YOU WIN!' : 'YOU LOSE!'}
        </RetroText>

        <div className="mb-6 sm:mb-10">
          <pre className="text-xl sm:text-2xl lg:text-3xl leading-tight font-retro text-retro-white inline-block">
            {playerWon ?
              `▓▓▓▓▓▓▓▓
▒VICTORY▒
░░░░░░░░` :
              isDraw ?
                `████████
▒▒DRAW▒▒
░░░░░░░░` :
                `███████
▒DEFEAT▒
░░░░░░░`}</pre>
        </div>

        <RetroText className="text-lg sm:text-xl lg:text-2xl mb-6 sm:mb-8">
          {isDraw
            ? 'BOTH CREATURES FAINTED!'
            : playerWon
              ? 'OTHER CREATURE FAINTED!'
              : 'YOUR CREATURE FAINTED!'}
        </RetroText>

        {account && (
          <div className="border-4 border-retro-white bg-retro-white p-4 sm:p-6 mb-6 sm:mb-10 inline-block w-full max-w-[380px] sm:max-w-[450px]">
            <RetroText className="text-lg sm:text-xl mb-2 sm:mb-3 font-bold text-retro-blue">ELO RATING</RetroText>
            <RetroText className="text-2xl sm:text-3xl lg:text-4xl font-black mb-2 sm:mb-3 text-retro-blue">
              {account.elo}
              {eloChange !== 0 && (
                <span className="text-2xl sm:text-3xl lg:text-4xl ml-2 text-retro-blue">
                  ({eloChange > 0 ? '+' : ''}{eloChange})
                </span>
              )}
            </RetroText>
            <RetroText className="text-xs sm:text-base text-retro-blue whitespace-nowrap px-2">
              RECORD: {account.wins}W - {account.losses}L - {account.draws}D
            </RetroText>
          </div>
        )}

        {/* Leaderboard */}
        {leaderboard.length > 0 && (
          <div className="mb-6 sm:mb-10">
            <div className="border-4 border-retro-white bg-retro-white p-4 sm:p-6 inline-block w-full max-w-[380px] sm:max-w-[450px]">
              <RetroText className="text-lg sm:text-xl font-bold mb-2 sm:mb-3 text-retro-blue">TOP 5 PLAYERS</RetroText>
              <div className="space-y-1 text-left">
                {leaderboard.slice(0, 5).map((leaderboardEntry, idx) => {
                  // The first element is the public key as bytes, convert to hex (no 0x prefix)
                  const publicKeyBytes = leaderboardEntry[0];
                  const publicKeyHex = Array.from(publicKeyBytes).map(b => b.toString(16).padStart(2, '0')).join('');
                  const stats = leaderboardEntry[1];
                  const isCurrentPlayer = publicKeyHex.toLowerCase() === player?.publicKeyHex?.toLowerCase();
                  return (
                    <div
                      key={publicKeyHex}
                      className={`flex justify-between text-xs sm:text-sm gap-1 px-2 py-1 ${isCurrentPlayer ? 'bg-retro-blue text-retro-white font-bold' : ''}`}
                    >
                      <RetroText className={`font-retro ${isCurrentPlayer ? 'font-black text-retro-white' : 'text-retro-blue'}`}>
                        #{idx + 1} {generateTrainerName(publicKeyHex)}
                      </RetroText>
                      <RetroText className={`whitespace-nowrap ${isCurrentPlayer ? 'font-black text-retro-white' : 'font-bold text-retro-blue'}`}>
                        {stats.elo}
                      </RetroText>
                    </div>
                  );
                })}
              </div>
            </div>
          </div>
        )}

        <div className="space-y-4 sm:space-y-6">
          <button
            onClick={onPlayAgain}
            className="w-full max-w-[380px] sm:max-w-[450px] border-4 border-retro-white bg-retro-blue px-6 sm:px-10 py-3 sm:py-5 hover:bg-retro-white hover:text-retro-blue transition-all text-retro-white group"
          >
            <RetroText className="text-lg sm:text-xl lg:text-2xl font-bold text-retro-white group-hover:text-retro-blue">PLAY AGAIN</RetroText>
          </button>

          <button
            onClick={onMainMenu}
            className="w-full max-w-[380px] sm:max-w-[450px] border-4 border-retro-white bg-retro-blue px-6 sm:px-8 py-3 sm:py-4 hover:bg-retro-white hover:text-retro-blue transition-all text-retro-white block mx-auto group"
          >
            <RetroText className="text-base sm:text-lg lg:text-xl text-retro-white group-hover:text-retro-blue">MAIN MENU</RetroText>
          </button>
        </div>
      </RetroBox>
    </div>
  );
};

export default GameOverScreen;