import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';

// Tauri грузит собранные файлы по своему протоколу, поэтому пути к ассетам
// должны быть относительными. `dist` — то, что встраивается в бинарник.
export default defineConfig({
  plugins: [vue()],
  base: './',
  server: {
    host: '127.0.0.1',
    port: 1420,
    strictPort: true,
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
});
