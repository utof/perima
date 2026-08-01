/**
 * Unit tests for `parseCoreError` and the `fromInvoke` round-trip.
 *
 * WHY: `parseCoreError` is the only non-trivial logic in api.ts.
 * These tests pin the three cases guaranteed by spec §4.3:
 *   1. Typed rejection → pass-through (NotFound, Io, etc.)
 *   2. Nested struct variant → preserved (Io with kind+message object)
 *   3. Unrecognised rejection → fallback Internal
 */
import { describe, it, expect, beforeEach } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import type { Mock } from "vitest";
import {
  attachTag,
  cancelTranscription,
  computeFullHash,
  parseCoreError,
  setProviderKey,
  transcribe,
} from "../api";
import type { CoreError, FileUuid } from "../bindings";

// ── parseCoreError unit tests ─────────────────────────────────────────

describe("parseCoreError", () => {
  it("passes through a typed NotFound rejection unchanged", () => {
    const raw: CoreError = { kind: "NotFound", data: "file not found" };
    const result = parseCoreError(raw);
    expect(result).toEqual<CoreError>({ kind: "NotFound", data: "file not found" });
  });

  it("passes through a typed Io rejection with nested kind+message", () => {
    const raw: CoreError = {
      kind: "Io",
      data: { kind: "PermissionDenied", message: "permission denied (os error 13)" },
    };
    const result = parseCoreError(raw);
    expect(result).toEqual<CoreError>({
      kind: "Io",
      data: { kind: "PermissionDenied", message: "permission denied (os error 13)" },
    });
  });

  it("falls back to Internal for an unrecognised rejection shape", () => {
    // Simulates a Tauri runtime error (command not registered, IPC failure, etc.)
    const raw = "command not registered";
    const result = parseCoreError(raw);
    expect(result).toEqual<CoreError>({
      kind: "Internal",
      data: "command not registered",
    });
  });

  it("falls back to Internal for an object with unknown kind", () => {
    const raw = { kind: "NonExistentVariant", data: "whatever" };
    const result = parseCoreError(raw);
    expect(result).toEqual<CoreError>({
      kind: "Internal",
      data: JSON.stringify(raw),
    });
  });

  it("falls back to Internal for null rejection", () => {
    const result = parseCoreError(null);
    expect(result).toEqual<CoreError>({
      kind: "Internal",
      data: "null",
    });
  });

  it("passes through all 8 known variant kinds (7 string-data + 1 struct-data Io)", () => {
    const kinds: Array<CoreError["kind"]> = [
      "NotFound",
      "Duplicate",
      "InvalidPath",
      "InvalidHash",
      "InvalidTag",
      "Unsupported",
      "Internal",
    ];
    for (const kind of kinds) {
      const raw = { kind, data: "test" };
      const result = parseCoreError(raw);
      expect(result.kind).toBe(kind);
    }
    // Io has a struct data field — test separately
    const ioRaw = { kind: "Io", data: { kind: "NotFound", message: "file missing" } };
    const ioResult = parseCoreError(ioRaw);
    expect(ioResult.kind).toBe("Io");
  });
});

// ── IPC argument-name contract ────────────────────────────────────────

/**
 * Pins the argument keys every `fromInvoke` payload puts on the wire.
 *
 * WHY this exists: `#[tauri::command]` exposes Rust snake_case parameters
 * to JS as camelCase unless the handler opts out with
 * `rename_all = "snake_case"` — which no handler in `crates/desktop` does.
 * A snake_case key here is therefore not a style nit: Tauri fails to
 * deserialize the payload and the command rejects at runtime with
 * `command <name> missing required key <arg>`, with nothing failing at
 * compile time to catch it.
 *
 * WHY it isn't covered elsewhere: `bindings.ts` is tauri-specta's *type*
 * output only — it carries no command wrappers, so the `just bindings`
 * drift gate never sees these argument names. Component tests mock
 * `../api` wholesale, so they never observe the payload either. This is
 * the only layer that pins the boundary.
 *
 * Two known-good commands are asserted alongside the transcription pair
 * so the camelCase convention itself stays pinned, not just the bugs.
 */
describe("invoke argument names (camelCase IPC contract)", () => {
  beforeEach(() => {
    (invoke as Mock).mockReset();
    (invoke as Mock).mockResolvedValue(undefined);
  });

  it("setProviderKey sends `apiKey`, not `api_key`", () => {
    void setProviderKey("groq", "sk-test-123");
    expect(invoke).toHaveBeenCalledWith("set_provider_key", {
      provider: "groq",
      apiKey: "sk-test-123",
    });
  });

  it("cancelTranscription sends `requestUuid`, not `request_uuid`", () => {
    void cancelTranscription("11111111-2222-3333-4444-555555555555");
    expect(invoke).toHaveBeenCalledWith("cancel_transcription", {
      requestUuid: "11111111-2222-3333-4444-555555555555",
    });
  });

  it("attachTag sends `tagName` (already-correct anchor)", () => {
    void attachTag("abc123", "favourite");
    expect(invoke).toHaveBeenCalledWith("attach_tag", {
      hash: "abc123",
      tagName: "favourite",
    });
  });

  it("computeFullHash sends `fileUuid` (already-correct anchor)", () => {
    void computeFullHash("dead-beef" as FileUuid);
    expect(invoke).toHaveBeenCalledWith("compute_full_hash", {
      fileUuid: "dead-beef",
    });
  });

  it("transcribe sends camelCase for all four args", () => {
    void transcribe({
      fileUuid: "u-1",
      fileName: "clip.mp4",
      source: "/media/clip.mp4",
      languageHint: null,
    });
    expect(invoke).toHaveBeenCalledWith("transcribe", {
      fileUuid: "u-1",
      fileName: "clip.mp4",
      source: "/media/clip.mp4",
      languageHint: null,
    });
  });
});
