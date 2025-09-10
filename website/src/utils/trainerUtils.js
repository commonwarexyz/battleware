// Shared utilities for generating trainer names from public keys
import wordsData from './bip39.txt?raw';

// Parse the BIP-39 words list
const words = wordsData.split('\n')
  .map(line => line.trim())
  .filter(line => line.length > 0);

// Ensure we have words loaded
if (words.length === 0) {
  console.error('Failed to load BIP-39 words');
}

export const generateTrainerName = (publicKey) => {
  // Convert public key to bytes array for consistent processing
  let bytes;
  if (typeof publicKey === 'string') {
    // Remove any 0x prefix if present
    const cleanKey = publicKey.startsWith('0x') ? publicKey.slice(2) : publicKey;
    // Convert hex string to bytes
    bytes = [];
    for (let i = 0; i < cleanKey.length; i += 2) {
      bytes.push(parseInt(cleanKey.substr(i, 2), 16));
    }
  } else if (publicKey instanceof Uint8Array) {
    bytes = Array.from(publicKey);
  } else if (Array.isArray(publicKey)) {
    bytes = publicKey;
  } else {
    // Fallback - use a default name
    return 'Trainer';
  }

  // Ensure we have enough bytes
  if (bytes.length < 4) {
    return 'Trainer';
  }

  // Fallback if words aren't loaded
  if (words.length === 0) {
    return 'Trainer';
  }

  // Use first 4 bytes to select two words from BIP-39 list
  const word1Index = ((bytes[0] << 8) | bytes[1]) % words.length;
  const word2Index = ((bytes[2] << 8) | bytes[3]) % words.length;

  const word1 = words[word1Index];
  const word2 = words[word2Index];

  // Safety check
  if (!word1 || !word2) {
    console.error('Word selection failed', { word1Index, word2Index, wordsLength: words.length });
    return 'Trainer';
  }

  // Capitalize first letter of each word
  const capitalize = (str) => str.charAt(0).toUpperCase() + str.slice(1);
  return `${capitalize(word1)} ${capitalize(word2)}`;
};

// Generate a shorter version for compact displays
export const generateTrainerShortName = (publicKey) => {
  const fullName = generateTrainerName(publicKey);
  const parts = fullName.split(' ');
  if (parts.length === 2) {
    // Return first word + first 3 letters of second word
    return `${parts[0]}${parts[1].substring(0, 3)}`;
  }
  return fullName;
};