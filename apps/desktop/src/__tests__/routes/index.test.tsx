/**
 * IndexRoute composition tests — pin the #25 regression at the route level.
 *
 * WHY render the route (not pure-fn snapshots): pre-Batch-H, App.compose
 * tested composeVisible/sortByRank/computeFacets directly. Post-Batch-H,
 * the same logic lives inside IndexRoute and pulls inputs from
 * useFiles / useTags / useSearch + useUiStore. Mocking the api layer +
 * driving the store covers the same invariants end-to-end.
 *
 * MIRRORS: routes/index.tsx derivation block — composeVisible, then
 * sortByRank when searchActive, then computeFacets. Keep in sync.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { okAsync } from "neverthrow";
import { screen, waitFor } from "@testing-library/react";
import IndexRoute from "../../routes/index";
import { file } from "../../lib/__tests__/fixtures";
import * as api from "../../api";
import { renderWithProviders, resetUiStore } from "../test-utils";
import type { SearchHit, Tag } from "../../bindings";

vi.mock("../../api", async () => {
  const actual = await vi.importActual<typeof import("../../api")>("../../api");
  return {
    ...actual,
    listFilesWithTags: vi.fn(),
    listTags: vi.fn(),
    search: vi.fn(),
  };
});

const mockListFilesWithTags = vi.mocked(api.listFilesWithTags);
const mockListTags = vi.mocked(api.listTags);
const mockSearch = vi.mocked(api.search);

const tags: Tag[] = [
  { id: "vacation", name: "vacation", first_seen: "2026-01-01T00:00:00Z" },
  { id: "sunset", name: "sunset", first_seen: "2026-01-01T00:00:00Z" },
];

const files = [
  file("a", ["vacation"]),
  file("b", ["vacation", "sunset"]),
  file("c", ["sunset"]),
  file("d", []),
];

beforeEach(() => {
  vi.clearAllMocks();
  resetUiStore();
  // Default canned responses — individual tests override as needed.
  mockListFilesWithTags.mockReturnValue(okAsync(files));
  mockListTags.mockReturnValue(okAsync(tags));
  mockSearch.mockReturnValue(okAsync<SearchHit[]>([]));
});

afterEach(() => {
  vi.useRealTimers();
});

describe("IndexRoute composition", () => {
  it("case 1: no search, no tag → renders all files + sidebar All count = 4", async () => {
    renderWithProviders(<IndexRoute />);

    await waitFor(() => {
      // FileTable renders rows for each file — assert sidebar All-count of 4.
      const allBtn = screen.getByRole("button", { name: /^all/i });
      expect(allBtn.textContent).toContain("4");
    });

    // Both vacation + sunset rows visible in mode=all.
    // WHY anchored regex (added 2026-04-25): TagChip now also renders an X
    // button with aria-label "Remove vacation" for inline detach, so plain
    // /vacation/i matches multiple buttons. Anchor at start to scope to
    // the sidebar facet button whose accessible name begins with the tag
    // name itself.
    expect(screen.getByRole("button", { name: /^vacation/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /^sunset/i })).toBeInTheDocument();
  });

  it("case 2: tag filter only → narrowed; sidebar All count remains full set", async () => {
    renderWithProviders(<IndexRoute />, {
      initialStoreState: { selectedTagId: "vacation" },
    });

    await waitFor(() => {
      // mode=all (no search) — All count is files.length = 4.
      const allBtn = screen.getByRole("button", { name: /^all/i });
      expect(allBtn.textContent).toContain("4");
    });

    // vacation count chip = 2 (files a + b). Anchored to skip TagChip's
    // "Remove vacation" detach buttons in the table cells.
    const vacationBtn = screen.getByRole("button", { name: /^vacation/i });
    expect(vacationBtn.textContent).toContain("2");
  });

  it("case 3: search with hits → mode=facets; All count = visible-set size", async () => {
    // Three hits in rank order b > c > a — sortByRank inverts (most negative wins).
    mockSearch.mockReturnValue(
      okAsync<SearchHit[]>([
        { file_uuid: "uuid-a", blake3_hash: "a", volume_id: "vol", relative_path: "a.jpg", rank: -1.0 },
        { file_uuid: "uuid-b", blake3_hash: "b", volume_id: "vol", relative_path: "b.jpg", rank: -2.5 },
        { file_uuid: "uuid-c", blake3_hash: "c", volume_id: "vol", relative_path: "c.jpg", rank: -1.5 },
      ]),
    );

    renderWithProviders(<IndexRoute />, {
      initialStoreState: { debouncedQuery: '"sunset"' },
    });

    await waitFor(() => {
      // mode=facets — All count is the visible-set length = 3 (hits ∩ all = 3).
      const allBtn = screen.getByRole("button", { name: /^all/i });
      expect(allBtn.textContent).toContain("3");
    });
  });

  it("case 4: search + tag → INTERSECTED, mode=facets, All count = intersection size (#25 pin)", async () => {
    mockSearch.mockReturnValue(
      okAsync<SearchHit[]>([
        { file_uuid: "uuid-a", blake3_hash: "a", volume_id: "vol", relative_path: "a.jpg", rank: -1.0 },
        { file_uuid: "uuid-b", blake3_hash: "b", volume_id: "vol", relative_path: "b.jpg", rank: -2.5 },
        { file_uuid: "uuid-c", blake3_hash: "c", volume_id: "vol", relative_path: "c.jpg", rank: -1.5 },
      ]),
    );

    renderWithProviders(<IndexRoute />, {
      initialStoreState: {
        debouncedQuery: '"sunset"',
        selectedTagId: "vacation",
      },
    });

    await waitFor(() => {
      // hits = {a,b,c}, vacation = {a,b}; intersection = {a,b}; All = 2.
      const allBtn = screen.getByRole("button", { name: /^all/i });
      expect(allBtn.textContent).toContain("2");
    });
  });

  it("case 5: search active, zero hits → All count = 0; sidebar shows empty-state", async () => {
    mockSearch.mockReturnValue(okAsync<SearchHit[]>([]));

    renderWithProviders(<IndexRoute />, {
      initialStoreState: { debouncedQuery: '"missing"' },
    });

    await waitFor(() => {
      const allBtn = screen.getByRole("button", { name: /^all/i });
      expect(allBtn.textContent).toContain("0");
    });

    // mode=facets + zero counts → empty-state row in TagSidebar.
    expect(
      screen.getByText(/no tags in current results/i),
    ).toBeInTheDocument();
  });
});
