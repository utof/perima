import js from "@eslint/js";
import tsParser from "@typescript-eslint/parser";
import tsPlugin from "@typescript-eslint/eslint-plugin";
import tsdoc from "eslint-plugin-tsdoc";
import vitest from "eslint-plugin-vitest";
import react from "eslint-plugin-react";
import reactHooks from "eslint-plugin-react-hooks";

// WHY: ESLint 9 uses flat config; no .eslintrc.* files.

export default [
  js.configs.recommended,

  // ── TypeScript (all .ts/.tsx files) ──────────────────────────────────────
  // WHY flat/strict-type-checked: ships the full strict preset with type-aware
  // rules (no-unsafe-member-access, exhaustive switches, no-any, etc.).
  // parserOptions.project points to our tsconfig so type-aware rules work.
  ...tsPlugin.configs["flat/strict-type-checked"].map((c) => ({
    ...c,
    files: ["src/**/*.{ts,tsx}"],
  })),
  {
    files: ["src/**/*.{ts,tsx}"],
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
        // WHY: required for type-aware rules in strict-type-checked preset.
        project: "./tsconfig.json",
        tsconfigRootDir: import.meta.dirname,
      },
      globals: {
        window: "readonly",
        document: "readonly",
        console: "readonly",
        // WHY: WebView environment exposes WHATWG timers; we use them for
        // the 300ms file-event debounce in App.tsx.
        setTimeout: "readonly",
        clearTimeout: "readonly",
        // WHY: TextEncoder is a WHATWG Encoding API global available in
        // Tauri's WebView; used by TagChip for procedural color hashing.
        TextEncoder: "readonly",
        // WHY: DOM type globals needed by component code; TypeScript
        // knows these from lib.dom.d.ts but ESLint's no-undef rule
        // requires explicit declaration here.
        HTMLDivElement: "readonly",
        HTMLInputElement: "readonly",
        Node: "readonly",
        PointerEvent: "readonly",
      },
    },
    rules: {
      "tsdoc/syntax": "warn",
      // WHY: non-null assertions are needed at the React root mount point.
      "@typescript-eslint/no-non-null-assertion": "off",
      // WHY: `any` casts in the neverthrow/Tauri error path are intentional
      // (invoke returns unknown, cast to string is safe in context).
      // Downgraded to warn; tracked for cleanup in a follow-up.
      "@typescript-eslint/no-explicit-any": "warn",
      // WHY: `no-unused-vars` is replaced by the TS-aware version.
      "no-unused-vars": "off",
      "@typescript-eslint/no-unused-vars": [
        "error",
        { argsIgnorePattern: "^_", varsIgnorePattern: "^_" },
      ],
      // WHY: numbers in template literals (`${count} files`, `${n} B`) are
      // idiomatic JS/TS and always produce valid output. Requiring explicit
      // String(n) everywhere is noise without safety benefit.
      "@typescript-eslint/restrict-template-expressions": [
        "error",
        { allowNumber: true },
      ],
      // WHY: `+` with numbers and strings is idiomatic in formatting helpers.
      "@typescript-eslint/restrict-plus-operands": [
        "error",
        { allowNumberAndString: true },
      ],
    },
  },

  // ── React + React Hooks (all JSX/TSX source files) ───────────────────────
  // WHY react/flat.recommended + jsx-runtime: React 19 uses the new JSX
  // transform; `react-in-jsx-scope` / `jsx-uses-react` rules are wrong for it.
  // WHY react-hooks flat recommended-latest (v7): exhaustive-deps would have
  // caught the SearchBar feedback-loop bug (c1d5c17). Long overdue.
  {
    files: ["src/**/*.{ts,tsx,jsx}"],
    ...react.configs.flat.recommended,
    ...react.configs.flat["jsx-runtime"],
    settings: { react: { version: "detect" } },
  },
  {
    files: ["src/**/*.{ts,tsx,jsx}"],
    ...reactHooks.configs.flat["recommended-latest"],
    rules: {
      ...reactHooks.configs.flat["recommended-latest"].rules,
      // WHY: react-hooks v7 adds many new experimental rules. The classic two
      // (rules-of-hooks, exhaustive-deps) are the critical ones; downgrade the
      // rest to warn until we've audited the codebase against v7's semantics.
      // Follow-up: https://github.com/utof/perima (file issue for each warn below)
      "react-hooks/static-components": "warn",
      "react-hooks/use-memo": "warn",
      "react-hooks/void-use-memo": "warn",
      "react-hooks/preserve-manual-memoization": "warn",
      "react-hooks/immutability": "warn",
      "react-hooks/globals": "warn",
      "react-hooks/refs": "warn",
      "react-hooks/set-state-in-effect": "warn",
      "react-hooks/error-boundaries": "warn",
      "react-hooks/purity": "warn",
      "react-hooks/set-state-in-render": "warn",
      "react-hooks/config": "warn",
      "react-hooks/gating": "warn",
    },
  },

  // ── Vitest (test files only) ──────────────────────────────────────────────
  // WHY: catches .only/.skip left in committed tests, expect without matchers,
  // beforeEach without describe, etc.
  {
    files: [
      "src/__tests__/**/*.{ts,tsx}",
      "src/**/__tests__/**/*.{ts,tsx}",
    ],
    plugins: { vitest },
    rules: {
      ...vitest.configs.recommended.rules,
    },
    languageOptions: {
      globals: {
        ...vitest.environments.env.globals,
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
