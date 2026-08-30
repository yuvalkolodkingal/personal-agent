import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: { strictPort: true, host: "127.0.0.1" },
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target:
      process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13",
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (id.includes("/node_modules/@xterm/")) return "vendor-xterm";
        },
      },
    },
  },
  test: { environment: "jsdom" },
});
