import React, { useState, useEffect } from 'react';
import RetroBox from './RetroBox';
import RetroText from './RetroText';

const MatchmakingScreen = ({ onMatch, account }) => {
  const [dots, setDots] = useState('');
  const [resubmitCount, setResubmitCount] = useState(0);

  useEffect(() => {
    const interval = setInterval(() => {
      setDots(prev => prev.length >= 3 ? '' : prev + '.');
    }, 500);

    return () => clearInterval(interval);
  }, []);

  // Immediately start matchmaking on component mount
  useEffect(() => {
    onMatch();
  }, []);

  // Resubmit match transaction every 5 seconds to trigger timeout
  useEffect(() => {
    const resubmitInterval = setInterval(() => {
      console.log('Resubmitting match transaction to trigger timeout...');
      onMatch();
      setResubmitCount(prev => prev + 1);
    }, 5000);

    return () => clearInterval(resubmitInterval);
  }, [onMatch]);

  return (
    <div className="bg-retro-blue p-4 sm:p-8 mx-auto max-w-[800px]">
      <RetroBox className="p-8 sm:p-16 text-center max-w-[800px] mx-auto">
        <div className="mb-6 sm:mb-10">
          <RetroText className="text-4xl sm:text-6xl lg:text-7xl font-black">ARENA</RetroText>
          <RetroText className="text-[10px] sm:text-sm text-center text-retro-white mt-1 sm:mt-2">[ 100% ONCHAIN RANDOMNESS ]</RetroText>
        </div>

        <RetroText className="text-2xl sm:text-3xl lg:text-4xl mb-4 sm:mb-6 font-bold">MATCHMAKING</RetroText>

        <div className="inline-block bg-retro-blue text-retro-white p-4 sm:p-6 border-4 border-retro-white mb-6 sm:mb-10">
          <RetroText className="text-sm sm:text-base mb-2 sm:mb-3 text-retro-white">WAITING FOR BATTLE</RetroText>
          <RetroText className="text-xs sm:text-sm text-retro-white">TRAINERS WILL BE MATCHED RANDOMLY</RetroText>
        </div>

        <RetroText className="text-lg sm:text-xl lg:text-2xl">SEARCHING{dots}</RetroText>
      </RetroBox>
    </div>
  );
};

export default MatchmakingScreen;