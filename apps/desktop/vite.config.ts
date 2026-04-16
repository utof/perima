import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// WHY: Tailwind 4 uses the Vite plugin (not PostCSS) — no tailwind.config.js needed.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    port: 5173,
    // WHY: Fail fast if port is taken; Tauri dev expects exactly 5173.
    strictPort: true,
  },
  build: {
    outDir: "dist",
    emptyOutDir: true,
  },
  test: {
    environment: "jsdom",
    setupFiles: ["src/__tests__/setup.ts"],
    globals: true,
    // WHY: tsc composite mode emits to dist-types; exclude it so vitest
    // doesn't double-run the compiled JS alongside the original TSX.
    exclude: ["dist-types/**", "node_modules/**"],
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
  },
});
