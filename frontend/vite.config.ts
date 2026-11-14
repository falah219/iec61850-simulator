import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// allowedHosts: true -> UI bisa dibuka lewat nama host server (bukan hanya localhost/IP); tool internal
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: { port: 4061, allowedHosts: true },
  preview: { port: 4061, allowedHosts: true },
})
