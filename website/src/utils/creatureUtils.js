// Shared utilities for parsing and displaying creatures

export const generateCreatureName = (traits) => {
  const prefixes = ['Zap', 'Bolt', 'Spark', 'Storm', 'Blaze', 'Frost', 'Shadow', 'Crystal',
    'Thunder', 'Ember', 'Glacier', 'Venom', 'Stone', 'Wind', 'Nova', 'Plasma',
    'Lunar', 'Solar', 'Cosmic', 'Quantum', 'Void', 'Neon', 'Prism', 'Magma',
    'Cyber', 'Turbo', 'Hyper', 'Omega', 'Alpha', 'Sigma', 'Echo', 'Phantom',
    'Nitro', 'Pulse', 'Flux', 'Razor', 'Inferno', 'Arctic', 'Radiant', 'Obsidian'];
  const suffixes = ['mon', 'chu', 'zard', 'king', 'rex', 'ion', 'byte', 'bit',
    'dash', 'strike', 'claw', 'fang', 'wing', 'tail', 'horn', 'scale',
    'storm', 'nova', 'flux', 'core', 'prime', 'max', 'omega', 'alpha',
    'blade', 'spark', 'volt', 'wave', 'burst', 'flash', 'surge', 'force',
    'beast', 'titan', 'fury', 'blitz', 'drake', 'phoenix', 'hydra', 'sphinx'];

  const prefixIndex = traits[5] % prefixes.length;
  const suffixIndex = traits[6] % suffixes.length;

  return prefixes[prefixIndex] + suffixes[suffixIndex];
};

export const generateCreatureASCII = (traits) => {
  // Add text variation selector to prevent emoji rendering
  const textVariant = '\uFE0E';
  const patterns = ['▓▓', '██', '▒▒', '░░', '##', '**', '▀▀', '▄▄',
    '◆◆', '◇◇', '▣▣', '▢▢', '◈◈', '◊◊', '▨▨', '▧▧',
    '▪▪', '▫▫', '◼◼', '◻◻', '◾◾', '◽◽', '▬▬', '▭▭'].map(p =>
      p.split('').map(c => c + textVariant).join('')
    );
  const eyes = ['◉', '●', '○', '◐', '◑', '⬤', '◯', '☉',
    '⊙', '⊚', '⊛', '⊜', '⊝', '◎', '◍', '◌',
    '☀', '☁', '☂', '☃', '★', '☆', '✦', '✧'].map(e => e + textVariant);

  const pattern = patterns[traits[7] % patterns.length];
  const eye = eyes[traits[8] % eyes.length];

  // Determine body shape based on traits
  const shapeTypes = ['round', 'spiky', 'tall', 'wide', 'angular', 'curved', 'compact', 'elongated',
    'hexagonal', 'triangular', 'diamond', 'crystalline', 'mechanical', 'chaotic'];
  const shapeIndex = traits[9] % shapeTypes.length;
  const shape = shapeTypes[shapeIndex];

  switch (shape) {
    case 'round':
      return [
        `  ${pattern}${pattern}${pattern}  `,
        ` ${pattern}${eye}${pattern}${eye}${pattern} `,
        `${pattern}${pattern}${pattern}${pattern}${pattern}`,
        ` ${pattern}${pattern}${pattern}${pattern} `,
        `  ${pattern}  ${pattern}  `
      ];
    case 'spiky':
      return [
        `  ▲${textVariant} ${pattern} ▲${textVariant}  `,
        ` ${pattern}${eye}${pattern}${eye}${pattern} `,
        `<${pattern}${pattern}${pattern}${pattern}>`,
        ` ▼${textVariant}${pattern}${pattern}${pattern}▼${textVariant} `,
        `  ${pattern}  ${pattern}  `
      ];
    case 'tall':
      return [
        `  ${pattern}${pattern}  `,
        ` ${pattern}${eye}${eye}${pattern} `,
        ` ${pattern}${pattern}${pattern} `,
        ` ${pattern}${pattern}${pattern} `,
        ` ${pattern}  ${pattern} `
      ];
    case 'wide':
      return [
        ` ${pattern}${pattern}${pattern}${pattern}${pattern} `,
        `${pattern}${eye}${pattern}${pattern}${eye}${pattern}`,
        `${pattern}${pattern}${pattern}${pattern}${pattern}${pattern}`,
        ` ${pattern}${pattern}${pattern}${pattern}${pattern} `,
        `  ${pattern}    ${pattern}  `
      ];
    case 'angular':
      return [
        `  /${pattern}${pattern}\\  `,
        ` ${pattern}${eye}${pattern}${eye}${pattern} `,
        `/${pattern}${pattern}${pattern}${pattern}\\`,
        ` \\${pattern}${pattern}${pattern}/ `,
        `  ${pattern}  ${pattern}  `
      ];
    case 'curved':
      return [
        `  ∩${textVariant}${pattern}${pattern}∩${textVariant}  `,
        ` ${pattern}${eye}${pattern}${eye}${pattern} `,
        `(${pattern}${pattern}${pattern}${pattern})`,
        ` ∪${textVariant}${pattern}${pattern}${pattern}∪${textVariant} `,
        `  ${pattern}  ${pattern}  `
      ];
    case 'compact':
      return [
        ` ${pattern}${pattern}${pattern} `,
        `${pattern}${eye}${eye}${pattern}`,
        `${pattern}${pattern}${pattern}${pattern}`,
        ` ${pattern}${pattern} `,
        ` ▪${textVariant}  ▪${textVariant} `
      ];
    case 'elongated':
      return [
        `  ${pattern}${pattern}${pattern}${pattern}  `,
        ` ${pattern}${eye}${pattern}${pattern}${eye}${pattern} `,
        `${pattern}${pattern}${pattern}${pattern}${pattern}${pattern}`,
        ` ${pattern}${pattern}${pattern}${pattern}${pattern} `,
        `  ${pattern}    ${pattern}  `
      ];
    case 'hexagonal':
      return [
        `   /${pattern}${pattern}\\   `,
        `  /${eye}${pattern}${eye}\\  `,
        ` |${pattern}${pattern}${pattern}${pattern}| `,
        `  \\${pattern}${pattern}${pattern}/  `,
        `   \\${pattern}${pattern}/   `
      ];
    case 'triangular':
      return [
        `     ▲${textVariant}     `,
        `   /${pattern}\\   `,
        `  /${eye}${eye}\\  `,
        ` /${pattern}${pattern}${pattern}\\ `,
        `/${pattern}${pattern}${pattern}${pattern}\\`
      ];
    case 'diamond':
      return [
        `   ◆${textVariant}${pattern}◆${textVariant}   `,
        `  ◆${textVariant}${eye}${pattern}${eye}◆${textVariant}  `,
        ` ◆${textVariant}${pattern}${pattern}${pattern}${pattern}◆${textVariant} `,
        `  ◆${textVariant}${pattern}${pattern}${pattern}◆${textVariant}  `,
        `   ◆${textVariant}${pattern}◆${textVariant}   `
      ];
    case 'crystalline':
      return [
        `  ◈${textVariant}${pattern}${pattern}◈${textVariant}  `,
        ` ◈${textVariant}${eye}◈${textVariant}${eye}◈${textVariant} `,
        `◈${textVariant}${pattern}◈${textVariant}${pattern}◈${textVariant}${pattern}◈${textVariant}`,
        ` ◈${textVariant}${pattern}◈${textVariant}${pattern}◈${textVariant} `,
        `  ◈${textVariant}  ◈${textVariant}  `
      ];
    case 'mechanical':
      return [
        ` [${pattern}${pattern}${pattern}] `,
        `[${eye}]${pattern}[${eye}]`,
        `[${pattern}${pattern}${pattern}${pattern}]`,
        ` |${pattern}${pattern}${pattern}| `,
        ` |_||_| `
      ];
    default: // chaotic
      return [
        ` ◢${textVariant}${pattern}◣${textVariant}${pattern}◤${textVariant} `,
        `◥${textVariant}${eye}◢${textVariant}${pattern}◣${textVariant}${eye}◤${textVariant}`,
        `◢${textVariant}${pattern}◥${textVariant}◤${textVariant}${pattern}◣${textVariant}`,
        ` ◥${textVariant}${pattern}◢${textVariant}◣${textVariant}${pattern}◤${textVariant} `,
        `  ◥${textVariant}  ◤${textVariant}  `
      ];
  }
};


export const generateMoveNames = (traits) => {
  const defenseNames = ['Barrier', 'Shield', 'Guard', 'Protect', 'Restore', 'Heal', 'Recover', 'Mend',
    'Aegis', 'Ward', 'Fortify', 'Deflect', 'Absorb', 'Regenerate', 'Sanctuary', 'Bulwark',
    'Armor', 'Cocoon', 'Mirror', 'Counter', 'Parry', 'Evade', 'Cloak', 'Veil'];
  const attackPrefixes = ['Fire', 'Ice', 'Thunder', 'Shadow', 'Mega', 'Hyper', 'Ultra', 'Giga',
    'Plasma', 'Cosmic', 'Chaos', 'Primal', 'Divine', 'Infernal', 'Spectral', 'Quantum',
    'Vortex', 'Nebula', 'Crystal', 'Adamant', 'Ethereal', 'Temporal', 'Dimensional', 'Apocalypse'];
  const attackSuffixes = ['Strike', 'Blast', 'Punch', 'Kick', 'Slash', 'Beam', 'Wave', 'Claw',
    'Cannon', 'Storm', 'Barrage', 'Eruption', 'Cyclone', 'Meteor', 'Comet', 'Nova',
    'Burst', 'Crush', 'Rend', 'Shatter', 'Devastate', 'Annihilate', 'Obliterate', 'Pulverize'];

  return {
    defense: defenseNames[traits[10] % defenseNames.length],
    attack1: attackPrefixes[traits[11] % attackPrefixes.length] + ' ' +
      attackSuffixes[traits[12] % attackSuffixes.length],
    attack2: attackPrefixes[traits[13] % attackPrefixes.length] + ' ' +
      attackSuffixes[traits[14] % attackSuffixes.length],
    attack3: attackPrefixes[traits[15] % attackPrefixes.length] + ' ' +
      attackSuffixes[traits[16] % attackSuffixes.length]
  };
};

export const parseCreature = (creatureData, wasm) => {
  const traits = creatureData.traits;

  // Get full creature data from WASM if available
  let fullCreatureData = null;
  if (wasm) {
    fullCreatureData = wasm.generateCreatureFromTraits(new Uint8Array(traits));
  }

  // Get move data from fullData
  const moveData = fullCreatureData?.moves || [];

  const name = generateCreatureName(traits);
  const moveNames = generateMoveNames(traits);

  // Filter out move 0 (no-op) and adjust indices for UI
  const filteredMoves = moveData.slice(1).map((move, index) => ({
    index: index + 1, // UI expects moves 1-4
    name: index === 0 ? moveNames.defense : moveNames[`attack${index}`],
    strength: move.strength,
    usageLimit: move.usage_limit,
    isDefense: move.is_defense
  }));

  return {
    name,
    health: fullCreatureData?.health || traits[0],
    moves: filteredMoves,
    ascii: generateCreatureASCII(traits),
    traits
  };
};