import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// The Hub serves no CORS headers, and the refresh cookie is scoped to
// /api/v1/auth with SameSite=Lax. Proxying keeps the browser same-origin so
// cookies, Origin checks and credentialed refresh all behave like production.
const HUB = process.env.NAGISALAKE_HUB_URL ?? 'http://127.0.0.1:9091'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) },
  },
  server: {
    port: 3000,
    strictPort: true,
    proxy: {
      '/api': { target: HUB, changeOrigin: false },
      '/healthz': { target: HUB, changeOrigin: false },
    },
  },
})
