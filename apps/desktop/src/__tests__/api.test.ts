/**
 * Unit tests for `parseCoreError` and the `fromInvoke` round-trip.
 *
 * WHY: `parseCoreError` is the only non-trivial logic in api.ts.
 * These tests pin the three cases guaranteed by spec §4.3:
 *   1. Typed rejection → pass-through (NotFound, Io, etc.)
 *   2. Nested struct variant → preserved (Io with kind+message object)
 *   3. Unrecognised rejection → fallback Internal
 */
import { describe, it, expect } from "vitest";
import { parseCoreError } from "../api";
import type { CoreError } from "../bindings";

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

  it("passes through all 8 known variant kinds", () => {
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
