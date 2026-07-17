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
      "/api": {
        target: "http://127.0.0.1:3149",
        // Avoid buffering SSE from POST /api/update during `vite` dev.
        configure: (proxy) => {
          proxy.on("proxyRes", (proxyRes) => {
            const ct = proxyRes.headers["content-type"]
            if (typeof ct === "string" && ct.includes("text/event-stream")) {
              proxyRes.headers["cache-control"] = "no-cache"
              proxyRes.headers["x-accel-buffering"] = "no"
            }
          })
        },
      },
      "/ws": { target: "ws://127.0.0.1:3149", ws: true },
    },
  },
  build: {
    outDir: path.resolve(__dirname, "../crates/mizpah/static"),
    emptyOutDir: true,
  },
})
