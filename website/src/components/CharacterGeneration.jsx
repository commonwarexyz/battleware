import React, { useState, useEffect } from 'react';
import RetroBox from './RetroBox';
import RetroText from './RetroText';
import { WasmWrapper } from '../api/wasm';
import { parseCreature as parseCreatureUtil, generateCreatureASCII } from '../utils/creatureUtils';

// Generate random trait arrays for preview creatures
const generateRandomTraits = () => {
  const traits = new Uint8Array(32);
  for (let i = 0; i < 32; i++) {
    traits[i] = Math.floor(Math.random() * 256);
  }
  return traits;
};

const CharacterGeneration = ({ onGenerate, account, onContinue, showExisting = true, onBack }) => {
  const [isGenerating, setIsGenerating] = useState(false);
  const [creature, setCreature] = useState(null);
  const [wasm, setWasm] = useState(null);
  const [previewCreature, setPreviewCreature] = useState(() => generateCreatureASCII(generateRandomTraits()));

  // Initialize WASM
  useEffect(() => {
    const initWasm = async () => {
      const wrapper = new WasmWrapper();
      await wrapper.init();
      setWasm(wrapper);
    };
    initWasm();
  }, []);

  // Animate preview creatures
  useEffect(() => {
    if (!creature && !isGenerating) {
      const interval = setInterval(() => {
        setPreviewCreature(generateCreatureASCII(generateRandomTraits()));
      }, 500); // Change every 0.5 seconds
      return () => clearInterval(interval);
    }
  }, [creature, isGenerating]);

  // Parse creature when account changes
  useEffect(() => {
    // Only show existing creature if showExisting is true
    if (account?.creature && wasm && showExisting && !isGenerating) {
      const creatureData = parseCreatureUtil(account.creature, wasm);
      setCreature(creatureData);
    }
  }, [account, wasm, showExisting]);

  // Handle new creature after generation
  useEffect(() => {
    if (account?.creature && wasm && isGenerating) {
      const creatureData = parseCreatureUtil(account.creature, wasm);
      setCreature(creatureData);
      setIsGenerating(false);
    }
  }, [account?.creature]);

  const handleGenerate = () => {
    setIsGenerating(true);
    setCreature(null);
    onGenerate();
  };

  const handleRetry = () => {
    setIsGenerating(true);
    setCreature(null);
    onGenerate();
  };

  return (
    <div className="bg-retro-blue p-2 sm:p-4 mx-auto max-w-[800px]">
      <RetroBox className="p-3 sm:p-6 max-w-[800px] mx-auto">
        <div className="border-b-4 border-retro-white pb-3 sm:pb-4 mb-4 sm:mb-6">
          <RetroText className="text-base sm:text-xl lg:text-2xl text-center font-bold">CREATURE GENERATION</RetroText>
          <RetroText className="text-[10px] sm:text-sm text-center text-retro-white mt-1 sm:mt-2">[ 100% ONCHAIN RANDOMNESS ]</RetroText>
        </div>

        {!creature && !isGenerating && (
          <div className="text-center py-3 sm:py-6">
            <RetroText className="text-xs sm:text-lg mb-3 sm:mb-6 leading-relaxed">
              SUMMON YOUR BATTLE COMPANION<br />
              AND ENTER THE ARENA
            </RetroText>

            {/* Animated creature preview */}
            <div className="mb-4 sm:mb-6">
              <div className="inline-block bg-retro-blue border-4 border-retro-white p-3 sm:p-4">
                <pre className="text-xs sm:text-sm lg:text-base leading-tight font-retro text-retro-white">
                  {previewCreature.map((line, i) => (
                    <div key={i}>{line}</div>
                  ))}
                </pre>
              </div>
              <RetroText className="text-xs sm:text-sm text-retro-white mt-2">[ PREVIEW ]</RetroText>
            </div>

            <div className="flex flex-col sm:flex-row justify-center gap-3 sm:gap-4">
              {onBack && (
                <button
                  onClick={onBack}
                  className="border-4 border-retro-white bg-retro-blue px-6 sm:px-10 py-3 sm:py-5 hover:bg-retro-white hover:text-retro-blue transition-all text-retro-white group"
                >
                  <RetroText className="text-base sm:text-xl lg:text-2xl font-bold text-retro-white group-hover:text-retro-blue">[BACK]</RetroText>
                </button>
              )}

              <button
                onClick={handleGenerate}
                className="border-4 border-retro-white bg-retro-blue px-6 sm:px-10 py-3 sm:py-5 hover:bg-retro-white hover:text-retro-blue transition-all text-retro-white group"
              >
                <RetroText className="text-base sm:text-xl lg:text-2xl font-bold text-retro-white group-hover:text-retro-blue">GENERATE</RetroText>
              </button>
            </div>
          </div>
        )}

        {isGenerating && (
          <div className="text-center py-6 sm:py-12">
            <div className="mb-6 sm:mb-10">
              <pre className="text-lg sm:text-xl lg:text-2xl leading-tight font-retro text-retro-white">
                {`▓▓▓▓
▒VRF▒
░░░░`}</pre>
            </div>
            <RetroText className="text-lg sm:text-xl lg:text-2xl mb-4 sm:mb-6 text-retro-white">GENERATING...</RetroText>
            <div className="inline-block bg-retro-blue text-retro-white p-3 sm:p-5 border-4 border-retro-white">
              <RetroText className="text-sm sm:text-base text-retro-white">WAITING FOR TRANSACTION...</RetroText>
            </div>
          </div>
        )}

        {creature && !isGenerating && (
          <div>
            <div className="border-4 border-retro-white bg-retro-blue p-3 sm:p-6 mb-4 sm:mb-6">
              {/* Creature Display */}
              <div className="text-center mb-4 sm:mb-6">
                <RetroText className="text-base sm:text-lg lg:text-xl mb-2 sm:mb-3 font-bold text-retro-white">{creature.name}</RetroText>
                <div className="bg-retro-blue border-4 border-retro-white p-2 sm:p-3 inline-block">
                  <pre className="text-xs sm:text-sm lg:text-base leading-tight font-retro text-retro-white">
                    {creature.ascii.map((line, i) => (
                      <div key={i}>{line}</div>
                    ))}
                  </pre>
                </div>
              </div>

              {/* Stats Section */}
              <div className="max-w-lg mx-auto">
                <div className="space-y-2">
                  <div className="bg-textured-white p-2">
                    <div className="flex justify-between items-center">
                      <RetroText className="text-xs sm:text-sm font-bold text-retro-blue">HEALTH</RetroText>
                      <RetroText className="text-xs sm:text-sm font-bold text-retro-blue">{creature.health} HP</RetroText>
                    </div>
                  </div>

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
            </div>

            <div className="text-center">
              {!showExisting && (
                <RetroText className="text-lg sm:text-xl mb-4 sm:mb-6 font-bold text-retro-white">
                  CREATURE GENERATED!
                </RetroText>
              )}
              <div className="flex flex-col sm:flex-row justify-center gap-2 sm:gap-4">
                {showExisting && onBack ? (
                  <>
                    <button
                      onClick={onBack}
                      className="border-4 border-retro-white bg-retro-blue px-4 sm:px-6 py-2 sm:py-3 hover:bg-retro-white hover:text-retro-blue transition-all text-retro-white group"
                    >
                      <RetroText className="text-sm sm:text-base lg:text-lg font-bold text-retro-white group-hover:text-retro-blue">[BACK]</RetroText>
                    </button>
                    <button
                      onClick={handleGenerate}
                      className="border-4 border-retro-white bg-retro-blue px-4 sm:px-6 py-2 sm:py-3 hover:bg-retro-white hover:text-retro-blue transition-all text-retro-white group"
                    >
                      <RetroText className="text-sm sm:text-base lg:text-lg font-bold text-retro-white group-hover:text-retro-blue">REGENERATE</RetroText>
                    </button>
                    <button
                      onClick={onContinue}
                      className="border-4 border-retro-white bg-retro-blue px-4 sm:px-6 py-2 sm:py-3 hover:bg-retro-white hover:text-retro-blue transition-all text-retro-white group"
                    >
                      <RetroText className="text-sm sm:text-base lg:text-lg font-bold text-retro-white group-hover:text-retro-blue">PLAY GAME</RetroText>
                    </button>
                  </>
                ) : (
                  <>
                    <button
                      onClick={handleRetry}
                      className="border-4 border-retro-white bg-retro-blue px-6 sm:px-8 py-3 sm:py-4 hover:bg-retro-white hover:text-retro-blue transition-all text-retro-white group"
                    >
                      <RetroText className="text-lg sm:text-xl font-bold text-retro-white group-hover:text-retro-blue">RETRY</RetroText>
                    </button>
                    <button
                      onClick={onContinue}
                      className="border-4 border-retro-white bg-retro-blue px-6 sm:px-8 py-3 sm:py-4 hover:bg-retro-white hover:text-retro-blue transition-all text-retro-white group"
                    >
                      <RetroText className="text-lg sm:text-xl font-bold text-retro-white group-hover:text-retro-blue">CONTINUE</RetroText>
                    </button>
                  </>
                )}
              </div>
            </div>
          </div>
        )}
      </RetroBox>
    </div>
  );
};

export default CharacterGeneration;