import { fileURLToPath } from "node:url";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

const srcDir = fileURLToPath(new URL("./src", import.meta.url));

/**
 * Vitest configuration for UI smoke tests (R10, 2026-08-02).
 *
 * Aliases mirror tsconfig.json `paths` (@/* -> ./src/*; the historical
 * "@wr/core/*" alias was unified into "@" in 20261013_01 E3).
 */
export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: [{ find: "@", replacement: srcDir }]
  },
  test: {
    environment: "jsdom",
    include: ["tests/**/*.test.{ts,tsx}"],
    setupFiles: ["./tests/setup.ts"],
    css: false
  }
});
