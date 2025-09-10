import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import { copyFileSync } from 'fs'
import { resolve } from 'path'

const backendUrl = process.env.VITE_URL || 'http://localhost:8080';
let backendOrigin = '';
try {
  const url = new URL(backendUrl);
  backendOrigin = url.origin;
} catch (e) {
  console.warn('Invalid VITE_URL:', backendUrl);
}

export default defineConfig({
  plugins: [
    react(),
    {
      name: 'html-transform',
      transformIndexHtml(html) {
        // Replace placeholder preconnect URLs with actual backend URL
        html = html.replace(/https:\/\/api\.example\.com/g, backendOrigin);
        
        // Ensure fetchpriority is added to the main script
        html = html.replace(
          /<script type="module" crossorigin src="(\/assets\/index-[^"]+\.js)"><\/script>/,
          '<script type="module" crossorigin src="$1" fetchpriority="high"></script>'
        );
        
        return html;
      }
    },
    {
      name: 'copy-files',
      closeBundle() {
        // Copy preview.png to dist folder after build
        try {
          copyFileSync(
            resolve(__dirname, 'preview.png'),
            resolve(__dirname, 'dist', 'preview.png')
          );
          console.log('✓ Copied preview.png to dist');
        } catch (err) {
          console.warn('Warning: Could not copy preview.png:', err.message);
        }
      }
    }
  ],
  define: {
    'import.meta.env.VITE_IDENTITY': JSON.stringify(process.env.VITE_IDENTITY || ''),
    'import.meta.env.VITE_URL': JSON.stringify(backendUrl)
  },
  server: {
    port: 3000
  },
  optimizeDeps: {
    exclude: ['./wasm/pkg/battleware_wasm.js']
  },
  build: {
    modulePreload: {
      polyfill: true
    }
  }
})