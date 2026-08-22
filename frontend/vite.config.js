import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { viteSingleFile } from 'vite-plugin-singlefile';
import path from 'path';
import fs from 'fs';

function renameToFrontendHtml() {
  return {
    name: 'rename-to-frontend-html',
    closeBundle() {
      const generated = path.resolve(__dirname, '../index.html');
      const target = path.resolve(__dirname, '../frontend.html');
      if (fs.existsSync(generated)) {
        fs.renameSync(generated, target);
      }
    }
  };
}

export default defineConfig({
  plugins: [
    svelte(),
    viteSingleFile({ removeOptionalTags: true }),
    renameToFrontendHtml()
  ],
  server: {
    proxy: {
      '/api': {
        target: 'http://localhost:8000',
        changeOrigin: true
      }
    }
  },
  build: {
    outDir: '../',
    emptyOutDir: false
  }
});
