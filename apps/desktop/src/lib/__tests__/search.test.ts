import { describe, it, expect } from "vitest";
import { buildFtsQuery, computeFacets } from "../search";
import { file } from "./fixtures";

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
