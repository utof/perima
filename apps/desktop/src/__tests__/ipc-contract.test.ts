/**
 * Exhaustive cross-check of every Tauri IPC command's argument names.
 *
 * WHY this file exists: `#[tauri::command]` exposes Rust snake_case
 * parameters to JS as camelCase unless the handler sets
 * `rename_all = "snake_case"`. Sending the wrong case is a RUNTIME
 * rejection (`command <name> missing required key <arg>`) that nothing
 * else in the pipeline catches:
 *
 *   - `tsc -b` sees `Record<string, unknown>` and is satisfied.
 *   - `bindings.ts` is tauri-specta's TYPE output only; it emits no command
 *     wrappers, so `just bindings` never inspects argument names.
 *   - Component tests mock `../api` wholesale, so payloads never appear.
 *   - Per-command tests in `api.test.ts` only cover commands somebody
 *     remembered to write a test for.
 *
 * That last gap is what this file closes. It derives the command list from
 * `lib.rs`'s `invoke_handler` (the actual source of truth) rather than from
 * a list maintained here, so a newly-registered command is covered the
 * moment it is registered — no one has to remember anything.
 *
 * WHY it fails on its own blind spots: an audit that silently skips what it
 * cannot parse reports "all clear" while missing bugs. Three separate
 * assertions below fail loudly if a registered command cannot be matched to
 * a Rust signature or to a frontend call site, so a parser gap becomes a red
 * test rather than false confidence.
 *
 * If this test breaks because `commands.rs` / `lib.rs` moved, fix the paths
 * here — do not delete the test.
 */
import { describe, it, expect } from "vitest";
import { existsSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";

/**
 * Walk up from the runner's cwd to the workspace root.
 *
 * WHY not `import.meta.url`: vitest transforms modules and serves them under
 * a non-`file:` URL, so `fileURLToPath` throws. WHY not a fixed `../../`:
 * that silently breaks if the suite is ever invoked from the repo root
 * rather than `apps/desktop`. Probing for two markers is stable under both.
 */
function findRepoRoot(): string {
  let dir = resolve(process.cwd());
  for (;;) {
    if (
      existsSync(join(dir, "crates/desktop/src/lib.rs")) &&
      existsSync(join(dir, "apps/desktop/package.json"))
    ) {
      return dir;
    }
    const parent = dirname(dir);
    if (parent === dir) throw new Error("workspace root not found above " + process.cwd());
    dir = parent;
  }
}

const repoRoot = findRepoRoot();
const read = (rel: string): string => readFileSync(join(repoRoot, rel), "utf8");

const libRs = read("crates/desktop/src/lib.rs");
const commandsRs = read("crates/desktop/src/commands.rs");
const apiTs = read("apps/desktop/src/api.ts");

/**
 * Commands deliberately not reachable from `api.ts`. Adding a name here is
 * an explicit, reviewable opt-out — the point is that a gap must be stated,
 * never inferred from silence. Empty today: all 29 are called.
 */
const NOT_CALLED_FROM_FRONTEND: ReadonlySet<string> = new Set([]);

/** Non-argument parameters Tauri injects; never appear in the JS payload. */
const INJECTED = ["State<", "AppHandle", "Window<", "Channel", "WebviewWindow"];

const toCamel = (s: string): string =>
  s.split("_").map((w, i) => (i === 0 ? w : w[0].toUpperCase() + w.slice(1))).join("");

/** Split on commas at bracket depth 0 (so `Option<A, B>` stays intact). */
function splitTopLevel(src: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let cur = "";
  for (const ch of src) {
    if ("<([{".includes(ch)) depth++;
    else if (">)]}".includes(ch)) depth--;
    if (ch === "," && depth === 0) {
      parts.push(cur);
      cur = "";
    } else cur += ch;
  }
  parts.push(cur);
  return parts.map((p) => p.trim()).filter(Boolean);
}

/** Commands actually registered with Tauri — the source of truth. */
const registered: string[] = [
  ...new Set(
    [...libRs.matchAll(/commands::(\w+)/g)]
      .map((m) => m[1])
      // WHY filter: `lib.rs` also references non-command items (event handlers)
      // through the same `commands::` path.
      .filter((n) => new RegExp(`fn ${n}\\s*\\(`).test(commandsRs)),
  ),
].sort();

// Command name to its parsed `rename_all` attribute + argument list,
// read out of `commands.rs`.
const signatures = new Map<string, { renameAll: string | null; params: string[] }>();
for (const m of commandsRs.matchAll(
  /#\[tauri::command([^\]]*)\]([\s\S]{0,600}?)pub (?:async )?fn (\w+)\s*\(([\s\S]*?)\)\s*->/g,
)) {
  const [, attr, , name, paramSrc] = m;
  const renameAll = /rename_all\s*=\s*"(\w+)"/.exec(attr)?.[1] ?? null;
  const params = splitTopLevel(paramSrc)
    .map((p) => /^(\w+)\s*:\s*([\s\S]+)$/.exec(p))
    .filter((pm): pm is RegExpExecArray => pm !== null)
    .filter(([, nm, ty]) => !nm.startsWith("_") && !INJECTED.some((i) => ty.includes(i)))
    .map(([, nm]) => nm);
  signatures.set(name, { renameAll, params });
}

// Command name to the argument keys api.ts actually puts on the wire.
const callSites = new Map<string, string[]>();
for (const m of apiTs.matchAll(/fromInvoke\s*(?:<[^>]*>)?\s*\(\s*"(\w+)"\s*,\s*/g)) {
  const open = apiTs.indexOf("{", m.index + m[0].length - 1);
  if (open === -1) continue;
  let depth = 0;
  let close = open;
  for (let i = open; i < apiTs.length; i++) {
    if (apiTs[i] === "{") depth++;
    else if (apiTs[i] === "}" && --depth === 0) {
      close = i;
      break;
    }
  }
  callSites.set(
    m[1],
    splitTopLevel(apiTs.slice(open + 1, close)).map((k) => k.split(":")[0].trim()),
  );
}

describe("IPC argument-name contract (all registered commands)", () => {
  it("finds a non-trivial set of registered commands", () => {
    // Guards against the parser silently matching nothing and the whole
    // suite then passing vacuously.
    expect(registered.length).toBeGreaterThan(20);
  });

  it("parses a Rust signature for every registered command", () => {
    // Any name listed here is a parser blind spot, NOT a passing command:
    // it was registered in lib.rs but no signature could be read for it.
    const unparsed = registered.filter((c) => !signatures.has(c));
    expect(unparsed).toEqual([]);
  });

  it("finds a frontend call site for every registered command", () => {
    // Any name listed here is registered but never called from api.ts.
    // If that is deliberate, add it to NOT_CALLED_FROM_FRONTEND above.
    const uncalled = registered.filter(
      (c) => !callSites.has(c) && !NOT_CALLED_FROM_FRONTEND.has(c),
    );
    expect(uncalled).toEqual([]);
  });

  it("sends every argument under the name Tauri expects", () => {
    const problems: string[] = [];
    for (const cmd of registered) {
      const sig = signatures.get(cmd);
      const sent = callSites.get(cmd);
      if (!sig || !sent) continue; // already asserted above
      for (const param of sig.params) {
        // `rename_all = "snake_case"` means the raw name goes on the wire;
        // the default (no attribute) means camelCase.
        const expected = sig.renameAll === "snake_case" ? param : toCamel(param);
        if (!sent.includes(expected)) {
          problems.push(
            `${cmd}: expected key "${expected}" (Rust param \`${param}\`), api.ts sends [${sent.join(", ")}]`,
          );
        }
      }
    }
    // Each entry names the command, the Rust parameter, the key Tauri
    // expects, and what api.ts actually sends.
    expect(problems).toEqual([]);
  });
});
