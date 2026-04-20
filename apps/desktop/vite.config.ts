import { defineConfig } from "vite";
import react, { reactCompilerPreset } from "@vitejs/plugin-react";
import babel from "@rolldown/plugin-babel";
import tailwindcss from "@tailwindcss/vite";

// WHY: Tailwind 4 uses the Vite plugin (not PostCSS) — no tailwind.config.js needed.
// WHY: React Compiler 1.0 runs via a separate `@rolldown/plugin-babel` pass
//      configured with `reactCompilerPreset()` exported by @vitejs/plugin-react.
//      This is the canonical wiring for plugin-react v6 (see the v6 README).
//      Compiler auto-memoizes components; DO NOT add manual useMemo/useCallback
//      for new code. See AGENTS.md "Frontend / React Compiler" pin.
// WHY: target: '19' is pinned explicitly even though '19' is the current
//      default — relying on the implicit default is a silent-failure hazard.
//      A future react-compiler minor could flip the default, switching our
//      runtime emit from `react/compiler-runtime` (React 19 built-in) to
//      `react-compiler-runtime` (17/18 polyfill) and defeating the Step 4
//      grep canary. Explicit pin costs one line and eliminates that class.
export default defineConfig({
  plugins: [
    react(),
    // WHY: panicThreshold defaults to `none` — Compiler silently skips bailout
    //      components rather than failing the build; acceptable for a greenfield
    //      frontend where the component surface is fully under our control.
    babel({ presets: [reactCompilerPreset({ target: "19" })] }),
    tailwindcss(),
  ],
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
    // WHY: tsc composite mode emits to dist-types; exclude it so vitest doesn't
    //      double-run the compiled JS alongside the original TSX.
    exclude: ["dist-types/**", "node_modules/**"],
    include: ["src/**/*.{test,spec}.{ts,tsx}"],
  },
});
