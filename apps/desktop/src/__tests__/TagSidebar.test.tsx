/**
 * TagSidebar — store-driven post-Batch-H Task 9. selection is read from
 * useUiStore, click handlers dispatch to the store. Tests render via
 * renderWithProviders so store seeding (selectedTagId) is straightforward.
 */
import { describe, expect, it, test, vi } from "vitest";
import { screen, fireEvent } from "@testing-library/react";
import TagSidebar from "../components/TagSidebar";
import { useUiStore } from "../stores/ui";
import { renderWithProviders } from "./test-utils";

const tags = [
  { id: "id-1", name: "vacation", first_seen: "2026-04-16T00:00:00Z" },
  { id: "id-2", name: "sunset", first_seen: "2026-04-16T00:00:00Z" },
];
const counts = { "id-1": 5, "id-2": 2 };

describe("TagSidebar", () => {
  test("renders All + each tag with counts", () => {
    renderWithProviders(
      <TagSidebar tags={tags} counts={counts} totalCount={7} mode="all" />,
    );
    expect(screen.getByText("All")).toBeInTheDocument();
    expect(screen.getByText("vacation")).toBeInTheDocument();
    expect(screen.getByText("sunset")).toBeInTheDocument();
    expect(screen.getByText("5")).toBeInTheDocument();
    expect(screen.getByText("2")).toBeInTheDocument();
  });

  test("clicking a tag dispatches setSelectedTagId(id) to the store", () => {
    const setSpy = vi.spyOn(useUiStore.getState(), "setSelectedTagId");
    renderWithProviders(
      <TagSidebar tags={tags} counts={counts} totalCount={7} mode="all" />,
    );
    fireEvent.click(screen.getByText("vacation"));
    expect(setSpy).toHaveBeenCalledWith("id-1");
  });

  test("clicking All dispatches setSelectedTagId(null)", () => {
    const setSpy = vi.spyOn(useUiStore.getState(), "setSelectedTagId");
    renderWithProviders(
      <TagSidebar tags={tags} counts={counts} totalCount={7} mode="all" />,
      { initialStoreState: { selectedTagId: "id-1" } },
    );
    fireEvent.click(screen.getByText("All"));
    expect(setSpy).toHaveBeenCalledWith(null);
  });

  test("selected tag has aria-pressed=true (from store)", () => {
    renderWithProviders(
      <TagSidebar tags={tags} counts={counts} totalCount={7} mode="all" />,
      { initialStoreState: { selectedTagId: "id-1" } },
    );
    const vacationBtn = screen.getByRole("button", { name: /vacation/i });
    expect(vacationBtn).toHaveAttribute("aria-pressed", "true");
  });

  test("All row shows total file count", () => {
    renderWithProviders(
      <TagSidebar tags={tags} counts={counts} totalCount={7} mode="all" />,
    );
    expect(screen.getByText("7")).toBeInTheDocument();
  });
});

describe("TagSidebar facets mode", () => {
  const facetTags = [
    { id: "t1", name: "vacation", first_seen: "2026-01-01T00:00:00Z" },
    { id: "t2", name: "sunset", first_seen: "2026-01-01T00:00:00Z" },
    { id: "t3", name: "beach", first_seen: "2026-01-01T00:00:00Z" },
  ];

  it("hides tags with 0 counts when mode=facets", () => {
    renderWithProviders(
      <TagSidebar
        tags={facetTags}
        counts={{ t1: 3, t2: 1 }}
        totalCount={4}
        mode="facets"
      />,
    );
    expect(screen.getByText("vacation")).toBeInTheDocument();
    expect(screen.getByText("sunset")).toBeInTheDocument();
    // t3 has no count → hidden in facets mode.
    expect(screen.queryByText("beach")).not.toBeInTheDocument();
  });

  it("shows empty-state row when all counts are 0", () => {
    renderWithProviders(
      <TagSidebar
        tags={facetTags}
        counts={{}}
        totalCount={0}
        mode="facets"
      />,
    );
    expect(screen.getByText("No tags in current results")).toBeInTheDocument();
  });

  it("All row count is totalCount in facets mode (= sum of counts)", () => {
    renderWithProviders(
      <TagSidebar
        tags={facetTags}
        counts={{ t1: 3, t2: 1 }}
        totalCount={4}
        mode="facets"
      />,
    );
    // The "All" row should show count 4 (= sum of visible).
    // WHY getByText + closest: the button's accessible name includes the count
    // span ("All 4"), so /^All$/i would not match the full accessible name.
    const allRow = screen.getByText("All").closest("button")!;
    expect(allRow.textContent).toContain("4");
  });

  it("shows all tags in mode=all regardless of counts", () => {
    renderWithProviders(
      <TagSidebar
        tags={facetTags}
        counts={{ t1: 3 }}
        totalCount={100}
        mode="all"
      />,
    );
    expect(screen.getByText("vacation")).toBeInTheDocument();
    expect(screen.getByText("sunset")).toBeInTheDocument();
    expect(screen.getByText("beach")).toBeInTheDocument();
  });
});
