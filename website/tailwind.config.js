module.exports = {
  content: [
    "./index.html",
    "./src/**/*.{js,ts,jsx,tsx}",
  ],
  theme: {
    extend: {
      colors: {
        'retro-blue': '#0000ee',
        'retro-white': '#ffffff',
      },
      fontFamily: {
        'retro': ['"Press Start 2P"', 'Consolas', 'Monaco', 'Courier New', 'monospace'],
      },
      borderColor: {
        'retro-blue': '#0000ee',
        'retro-white': '#ffffff',
      },
      backgroundColor: {
        'retro-blue': '#0000ee',
        'retro-white': '#ffffff',
      },
      textColor: {
        'retro-blue': '#0000ee',
        'retro-white': '#ffffff',
      },
    },
  },
  plugins: [],
}