import tailwindcss from "@tailwindcss/vite";
import vue from "@vitejs/plugin-vue";
import { defineConfig } from "vitest/config";
import { xtermDebugLogPlugin } from "./scripts/xterm-debug-log-plugin.mjs";

export default defineConfig({
  plugins: [vue(), tailwindcss(), xtermDebugLogPlugin()],
  clearScreen: false,
  server: {
    host: "127.0.0.1",
    port: 1420,
    strictPort: true,
  },
  // The Prettier worker code-splits, because its plugins are dynamic imports and a reader who only
  // ever formats CSS should not download the TypeScript parser. Rollup refuses to code-split a
  // worker bundled as an IIFE, and `es` is the format a module worker has anyway on every webview
  // this app runs in.
  worker: { format: "es" },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
    setupFiles: ["./src/test-setup.ts"],
    coverage: {
      provider: "v8",
      // Measure every production TS/Vue module; declarations/setup/tests and static SVGs are not executable code.
      include: ["src/**/*.{ts,vue}"],
      exclude: ["src/**/*.test.ts", "src/**/*.spec.ts", "src/test-setup.ts", "src/**/*.d.ts", "**/*.svg"],
      reporter: ["text-summary", "json-summary", "lcov"],
      reportsDirectory: "./coverage",
    },
  },
});
