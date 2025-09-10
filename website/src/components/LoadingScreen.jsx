import React from 'react';
import RetroBox from './RetroBox';
import RetroText from './RetroText';

const LoadingScreen = () => {
  return (
    <div className="min-h-screen bg-retro-blue flex items-center justify-center p-4">
      <RetroBox>
        <div className="text-center">
          <div className="mb-6 sm:mb-8">
            <pre className="text-base sm:text-xl lg:text-2xl leading-tight font-retro inline-block text-retro-white">
{`▓▓▓▓▓▓▓▓
▒LOADING▒
░░░░░░░░`}</pre>
          </div>
          <RetroText className="text-sm sm:text-lg lg:text-xl">INITIALIZING BATTLEWARE...</RetroText>
        </div>
      </RetroBox>
    </div>
  );
};

export default LoadingScreen;