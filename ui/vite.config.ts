import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// In development the API runs separately (`opentrack serve`, port 8090);
// in production the server serves this build at `/`.
const api = process.env.OT_API_URL ?? 'http://127.0.0.1:8090'

export default defineConfig({
  plugins: [react()],
  server: {
    host: '0.0.0.0',
    proxy: { '/api': api, '/healthz': api },
  },
})
