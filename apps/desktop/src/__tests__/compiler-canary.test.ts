import { describe, expect, it } from "vitest";
import * as babel from "@babel/core";
// WHY: @babel/plugin-syntax-jsx is needed so preset-typescript (which handles
// TypeScript stripping) doesn't reject JSX tokens — the Compiler itself requires
// real JSX in the source to recognise a React component and apply memoization.
import babelPluginSyntaxJsx from "@babel/plugin-syntax-jsx";

// WHY: Spec-mandated Compiler-activation canary. Runs babel-plugin-react-compiler
// directly in `annotation` mode on a known-good snippet, bypasses the Vite build
// pipeline entirely, and asserts the transformed source imports from
// `react/compiler-runtime` (React 19's built-in runtime path).
//
// This is stable against minifier renames (which is why the spec rejects
// bundle-grep as the primary verification).
//
// WHY "use memo" inside the function body (not at file level): the React Compiler
// treats "use memo" as a function-level directive (like "use strict") — placing
// it at the program level is parsed but silently ignored by the Compiler's
// annotation-mode opt-in logic. File-level opt-in requires `compilationMode: "all"`.
const CANARY_SOURCE = `
  import { useState } from "react";
  export function Canary() {
    "use memo";
    const [n, setN] = useState(0);
    return <div>{n}</div>;
  }
`;

describe("react-compiler transform", () => {
  it("emits react/compiler-runtime import for a 'use memo' annotated component", async () => {
    const result = await babel.transformAsync(CANARY_SOURCE, {
      filename: "canary.tsx",
      presets: [
        ["@babel/preset-typescript", { allExtensions: true, isTSX: true }],
      ],
      plugins: [
        babelPluginSyntaxJsx,
        [
          "babel-plugin-react-compiler",
          {
            // WHY: keep in sync with vite.config.ts's reactCompilerPreset target option;
            //      a silent drift would assert the wrong runtime import path.
            target: "19",
            compilationMode: "annotation",
          },
        ],
      ],
      babelrc: false,
      configFile: false,
    });
    expect(result?.code).toMatch(/from ["']react\/compiler-runtime["']/);
  });
});
