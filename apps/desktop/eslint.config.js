import js from "@eslint/js";
import tsParser from "@typescript-eslint/parser";
import tsPlugin from "@typescript-eslint/eslint-plugin";
import tsdoc from "eslint-plugin-tsdoc";

// WHY: ESLint 9 uses flat config; no .eslintrc.* files.
// TypeScript type-checking rules require parserOptions.project — omitted here
// to keep lint fast; tsc handles type errors separately in the build step.
export default [
  js.configs.recommended,
  {
    files: ["**/*.{ts,tsx}"],
    plugins: {
      "@typescript-eslint": tsPlugin,
      tsdoc,
    },
    languageOptions: {
      parser: tsParser,
      parserOptions: {
        ecmaVersion: 2022,
        sourceType: "module",
        ecmaFeatures: { jsx: true },
      },
      globals: {
        window: "readonly",
        document: "readonly",
        console: "readonly",
        // WHY: WebView environment exposes WHATWG timers; we use them for
        // the 300ms file-event debounce in App.tsx.
        setTimeout: "readonly",
        clearTimeout: "readonly",
      },
    },
    rules: {
      // Carry over recommended TS rules that don't require type info.
      ...tsPlugin.configs.recommended.rules,
      "tsdoc/syntax": "warn",
      // WHY: non-null assertions are needed at the React root mount point.
      "@typescript-eslint/no-non-null-assertion": "off",
    },
  },
  {
    // WHY: Test files use vitest globals (describe, it, expect, vi).
    files: ["src/__tests__/**/*.{ts,tsx}"],
    languageOptions: {
      globals: {
        describe: "readonly",
        it: "readonly",
        expect: "readonly",
        vi: "readonly",
        beforeEach: "readonly",
        afterEach: "readonly",
        beforeAll: "readonly",
        afterAll: "readonly",
      },
    },
  },
  {
    ignores: ["dist/**", "dist-types/**", "node_modules/**"],
  },
];
