import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

const bff = process.env.PANEL_BFF_URL ?? "http://127.0.0.1:8125";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: { "/api": bff, "/health": bff },
  },
  build: {
    outDir: "dist",
    sourcemap: false,
    chunkSizeWarningLimit: 800,
  },
  test: {
    include: ["test/**/*.test.ts"],
    environment: "node",
    setupFiles: ["test/setup.ts"],
  },
});
