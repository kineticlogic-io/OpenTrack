import react from '@vitejs/plugin-react'
import { defineConfig, searchForWorkspaceRoot } from 'vite'

// In development the API runs separately (`opentrack serve`, port 8090);
// in production the server serves this build at `/`.
const api = process.env.OT_API_URL ?? 'http://127.0.0.1:8090'

export default defineConfig({
  plugins: [react()],
  // The Sources page chunk carries CodeMirror and MapLibre (~290 kB gzipped); it is loaded
  // lazily, only when that page opens.
  build: { chunkSizeWarningLimit: 1200 },
  server: {
    host: '0.0.0.0',
    proxy: { '/api': api, '/healthz': api },
    // The Help page bundles the guides from ../docs/guides (see src/pages/help/guides.ts).
    fs: { allow: [searchForWorkspaceRoot(process.cwd()), '../docs/guides'] },
  },
})
