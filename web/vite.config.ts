import path from "path"
import tailwindcss from "@tailwindcss/vite"
import react from "@vitejs/plugin-react"
import { defineConfig } from "vite"

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  server: {
    proxy: {
      "/api": "http://127.0.0.1:1738",
      "/ws": { target: "ws://127.0.0.1:1738", ws: true },
    },
  },
  build: {
    outDir: path.resolve(__dirname, "../crates/mizpah/static"),
    emptyOutDir: true,
  },
})
