import { describe, it, expect } from "vitest";
import { buildFtsQuery, computeFacets, composeVisible, sortByRank } from "../search";
import { file } from "./fixtures";

describe("buildFtsQuery", () => {
  // WHY assertions updated 2026-04-25: plain queries now auto-prefix the
  // last token. Phrase-passthrough and explicit `*` paths unchanged.
  it("quotes earlier tokens + auto-prefixes the last", () => {
    expect(buildFtsQuery("sunset photos")).toBe('"sunset" "photos"*');
  });

  it("auto-prefixes a single token", () => {
    expect(buildFtsQuery("mp4")).toBe('"mp4"*');
  });

  it("auto-prefixes a 2-character query (covers the 'Cl' → Claude case)", () => {
    expect(buildFtsQuery("Cl")).toBe('"Cl"*');
  });

  it("passes explicit phrase queries verbatim (whole-token mode)", () => {
    expect(buildFtsQuery('"blue ridge"')).toBe('"blue ridge"');
  });

  it("wraps explicit prefix queries with quoted-token + asterisk", () => {
    expect(buildFtsQuery("sunse*")).toBe('"sunse"*');
  });

  it("handles apostrophes inside quoted tokens (auto-prefixed)", () => {
    expect(buildFtsQuery("it's fine")).toBe('"it\'s" "fine"*');
  });

  it("handles slashes and dots inside quoted tokens (auto-prefixed)", () => {
    expect(buildFtsQuery("a/b.jpg")).toBe('"a/b.jpg"*');
  });

  it("returns empty string on whitespace-only input", () => {
    expect(buildFtsQuery("   ")).toBe("");
    expect(buildFtsQuery("")).toBe("");
  });

  it("strips parens and leading dashes before tokenising (auto-prefixed)", () => {
    // (foo OR bar) → parens stripped → tokens foo, OR, bar; last gets *
    expect(buildFtsQuery("(foo OR bar)")).toBe('"foo" "OR" "bar"*');
  });

  it("strips bare unpaired double-quote", () => {
    expect(buildFtsQuery('"')).toBe("");
  });

  it("strips leading dash on a token (FTS5 negation hazard, auto-prefixed)", () => {
    expect(buildFtsQuery("-foo")).toBe('"foo"*');
  });

  it("respects explicit prefix on multi-token queries", () => {
    expect(buildFtsQuery("foo bar*")).toBe('"foo" "bar"*');
  });
});

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

describe("composeVisible", () => {
  const files = [
    file("a", ["vacation"]),
    file("b", ["vacation", "sunset"]),
    file("c", ["sunset"]),
    file("d", []),
  ];

  it("returns all files when no filters", () => {
    expect(composeVisible(files, null, null)).toEqual(files);
  });

  it("filters by tag id", () => {
    expect(composeVisible(files, "vacation", null).map((f) => f.hash)).toEqual([
      "a",
      "b",
    ]);
  });

  it("filters by search hit set", () => {
    const hits = new Set(["b", "d"]);
    expect(composeVisible(files, null, hits).map((f) => f.hash)).toEqual([
      "b",
      "d",
    ]);
  });

  it("intersects tag and search filters (THE #25 regression)", () => {
    const hits = new Set(["a", "b", "c"]);
    expect(composeVisible(files, "vacation", hits).map((f) => f.hash)).toEqual([
      "a",
      "b",
    ]);
  });

  it("returns empty list on empty search hit set", () => {
    expect(composeVisible(files, null, new Set())).toEqual([]);
  });

  it("returns empty list on unknown tag id", () => {
    expect(composeVisible(files, "nonexistent", null)).toEqual([]);
  });
});

describe("sortByRank", () => {
  it("orders files by rank ascending (lower = better)", () => {
    const visible = [file("a", []), file("b", []), file("c", [])];
    const ranks = new Map([
      ["a", -1.0],
      ["b", -2.5],
      ["c", -1.5],
    ]);
    expect(sortByRank(visible, ranks).map((f) => f.hash)).toEqual([
      "b",
      "c",
      "a",
    ]);
  });

  it("appends files missing from rank map at the end", () => {
    const visible = [file("a", []), file("b", []), file("c", [])];
    const ranks = new Map([["a", -1.0]]);
    const result = sortByRank(visible, ranks).map((f) => f.hash);
    expect(result[0]).toBe("a");
    expect(result.slice(1).sort()).toEqual(["b", "c"]);
  });

  it("returns empty list unchanged", () => {
    expect(sortByRank([], new Map())).toEqual([]);
  });
});
