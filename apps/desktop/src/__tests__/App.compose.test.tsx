import { describe, it, expect } from "vitest";
import { composeVisible, computeFacets, sortByRank } from "../lib/search";
import { file } from "../lib/__tests__/fixtures";

/**
 * App.tsx composition invariant.
 *
 * These snapshots pin the #25 regression. Any change to App.tsx's
 * visibleFiles / facetCounts derivation should be reflected here
 * deliberately — if a test breaks, the composition semantics changed
 * and the failure is the intended signal.
 *
 * MIRRORS: App.tsx derivation block — composeVisible, then sortByRank
 * when searchActive, then computeFacets. Keep in sync.
 */
describe("App composition invariant", () => {
  const files = [
    file("a", ["vacation"]),
    file("b", ["vacation", "sunset"]),
    file("c", ["sunset"]),
    file("d", []),
  ];

  function compose(
    tagId: string | null,
    hits: Set<string> | null,
    ranks: Map<string, number> = new Map(),
  ) {
    const searchActive = hits !== null;
    const base = composeVisible(files, tagId, hits);
    const visible = searchActive ? sortByRank(base, ranks) : base;
    const counts = computeFacets(visible);
    const mode: "all" | "facets" = searchActive ? "facets" : "all";
    const total = searchActive ? visible.length : files.length;
    return { visible: visible.map((f) => f.hash), counts, mode, total };
  }

  it("case 1: no search, no tag → all files, mode=all", () => {
    expect(compose(null, null)).toEqual({
      visible: ["a", "b", "c", "d"],
      counts: { vacation: 2, sunset: 2 },
      mode: "all",
      total: 4,
    });
  });

  it("case 2: tag filter only → tag-narrowed, mode=all", () => {
    expect(compose("vacation", null)).toEqual({
      visible: ["a", "b"],
      counts: { vacation: 2, sunset: 1 },
      mode: "all",
      total: 4, // full set count — no search active
    });
  });

  it("case 3: search only → hit-narrowed, sorted by rank, mode=facets", () => {
    const hits = new Set(["a", "b", "c"]);
    const ranks = new Map([
      ["a", -1.0],
      ["b", -2.5], // best
      ["c", -1.5],
    ]);
    expect(compose(null, hits, ranks)).toEqual({
      visible: ["b", "c", "a"],
      counts: { vacation: 2, sunset: 2 },
      mode: "facets",
      total: 3,
    });
  });

  it("case 4: search + tag → INTERSECTED, sorted by rank, mode=facets (the #25 pin)", () => {
    const hits = new Set(["a", "b", "c"]);
    const ranks = new Map([
      ["a", -1.0],
      ["b", -2.5],
      ["c", -1.5],
    ]);
    expect(compose("vacation", hits, ranks)).toEqual({
      visible: ["b", "a"],
      counts: { vacation: 2, sunset: 1 },
      mode: "facets",
      total: 2,
    });
  });

  it("case 5: search active but zero hits → empty list, mode=facets", () => {
    expect(compose(null, new Set())).toEqual({
      visible: [],
      counts: {},
      mode: "facets",
      total: 0,
    });
  });
});
