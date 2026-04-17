import { describe, it, expect } from "vitest";
import { buildFtsQuery, computeFacets } from "../search";
import type { FileWithTags } from "../../types";

describe("buildFtsQuery", () => {
  it("quotes plain tokens with implicit AND", () => {
    expect(buildFtsQuery("sunset photos")).toBe('"sunset" "photos"');
  });

  it("passes explicit phrase queries verbatim", () => {
    expect(buildFtsQuery('"blue ridge"')).toBe('"blue ridge"');
  });

  it("wraps prefix queries with quoted-token + asterisk", () => {
    expect(buildFtsQuery("sunse*")).toBe('"sunse"*');
  });

  it("handles apostrophes inside quoted tokens", () => {
    expect(buildFtsQuery("it's fine")).toBe('"it\'s" "fine"');
  });

  it("handles slashes and dots inside quoted tokens", () => {
    expect(buildFtsQuery("a/b.jpg")).toBe('"a/b.jpg"');
  });

  it("returns empty string on whitespace-only input", () => {
    expect(buildFtsQuery("   ")).toBe("");
    expect(buildFtsQuery("")).toBe("");
  });

  it("strips parens and leading dashes before tokenising", () => {
    // (foo OR bar) → parens stripped → tokens foo, OR, bar
    expect(buildFtsQuery("(foo OR bar)")).toBe('"foo" "OR" "bar"');
  });

  it("strips bare unpaired double-quote", () => {
    expect(buildFtsQuery('"')).toBe("");
  });

  it("strips leading dash on a token (FTS5 negation hazard)", () => {
    expect(buildFtsQuery("-foo")).toBe('"foo"');
  });

  it("wraps multi-token prefix queries with earlier tokens quoted + last token asterisked", () => {
    expect(buildFtsQuery("foo bar*")).toBe('"foo" "bar"*');
  });
});

function file(hash: string, tagIds: string[]): FileWithTags {
  return {
    hash,
    size: 0,
    volume_id: "vol",
    relative_path: `${hash}.jpg`,
    status: "active",
    first_seen: "2026-01-01T00:00:00Z",
    width: null,
    height: null,
    duration_ms: null,
    captured_at: null,
    camera_make: null,
    camera_model: null,
    codec: null,
    bitrate_bps: null,
    mime_type: null,
    thumbnail_path: null,
    thumbnail_status: null,
    tags: tagIds.map((id) => ({ id, name: `tag-${id}`, first_seen: "2026-01-01T00:00:00Z" })),
  };
}

describe("computeFacets", () => {
  it("returns empty object on empty file list", () => {
    expect(computeFacets([])).toEqual({});
  });

  it("returns empty object when no file has tags", () => {
    expect(computeFacets([file("a", []), file("b", [])])).toEqual({});
  });

  it("counts each tag occurrence across files", () => {
    const files = [
      file("a", ["vacation"]),
      file("b", ["vacation", "sunset"]),
      file("c", ["sunset"]),
    ];
    expect(computeFacets(files)).toEqual({
      vacation: 2,
      sunset: 2,
    });
  });

  it("handles many files with same tag", () => {
    const files = Array.from({ length: 5 }, (_, i) =>
      file(`h${i}`, ["vacation"]),
    );
    expect(computeFacets(files)).toEqual({ vacation: 5 });
  });
});
