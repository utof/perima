import { render, screen } from "@testing-library/react";
import { describe, expect, it, test, vi } from "vitest";
import TagSidebar from "../components/TagSidebar";

describe("TagSidebar", () => {
  const tags = [
    { id: "id-1", name: "vacation", first_seen: "2026-04-16T00:00:00Z" },
    { id: "id-2", name: "sunset", first_seen: "2026-04-16T00:00:00Z" },
  ];
  const counts = { "id-1": 5, "id-2": 2 };

  test("renders All + each tag", () => {
    render(
      <TagSidebar
        tags={tags}
        counts={counts}
        totalCount={7}
        selectedTagId={null}
        onSelect={() => {}}
      />,
    );
    expect(screen.getByText("All")).toBeInTheDocument();
    expect(screen.getByText("vacation")).toBeInTheDocument();
    expect(screen.getByText("sunset")).toBeInTheDocument();
    expect(screen.getByText("5")).toBeInTheDocument();
    expect(screen.getByText("2")).toBeInTheDocument();
  });

  test("clicking a tag calls onSelect with its id", () => {
    const onSelect = vi.fn();
    render(
      <TagSidebar
        tags={tags}
        counts={counts}
        totalCount={7}
        selectedTagId={null}
        onSelect={onSelect}
      />,
    );
    screen.getByText("vacation").click();
    expect(onSelect).toHaveBeenCalledWith("id-1");
  });

  test("clicking All calls onSelect with null", () => {
    const onSelect = vi.fn();
    render(
      <TagSidebar
        tags={tags}
        counts={counts}
        totalCount={7}
        selectedTagId={"id-1"}
        onSelect={onSelect}
      />,
    );
    screen.getByText("All").click();
    expect(onSelect).toHaveBeenCalledWith(null);
  });

  test("selected tag has aria-pressed=true", () => {
    render(
      <TagSidebar
        tags={tags}
        counts={counts}
        totalCount={7}
        selectedTagId={"id-1"}
        onSelect={() => {}}
      />,
    );
    const vacationBtn = screen.getByRole("button", { name: /vacation/i });
    expect(vacationBtn).toHaveAttribute("aria-pressed", "true");
  });

  test("All row shows total file count", () => {
    render(
      <TagSidebar
        tags={tags}
        counts={counts}
        totalCount={7}
        selectedTagId={null}
        onSelect={() => {}}
      />,
    );
    expect(screen.getByText("7")).toBeInTheDocument();
  });
});

describe("TagSidebar facets mode", () => {
  const tags = [
    { id: "t1", name: "vacation", first_seen: "2026-01-01T00:00:00Z" },
    { id: "t2", name: "sunset", first_seen: "2026-01-01T00:00:00Z" },
    { id: "t3", name: "beach", first_seen: "2026-01-01T00:00:00Z" },
  ];

  it("hides tags with 0 counts when mode=facets", () => {
    render(
      <TagSidebar
        tags={tags}
        counts={{ t1: 3, t2: 1 }}
        totalCount={4}
        selectedTagId={null}
        onSelect={vi.fn()}
        mode="facets"
      />,
    );
    expect(screen.getByText("vacation")).toBeInTheDocument();
    expect(screen.getByText("sunset")).toBeInTheDocument();
    // t3 has no count → hidden in facets mode.
    expect(screen.queryByText("beach")).not.toBeInTheDocument();
  });

  it("shows empty-state row when all counts are 0", () => {
    render(
      <TagSidebar
        tags={tags}
        counts={{}}
        totalCount={0}
        selectedTagId={null}
        onSelect={vi.fn()}
        mode="facets"
      />,
    );
    expect(screen.getByText("No tags in current results")).toBeInTheDocument();
  });

  it("All row count is totalCount in facets mode (= sum of counts)", () => {
    render(
      <TagSidebar
        tags={tags}
        counts={{ t1: 3, t2: 1 }}
        totalCount={4}
        selectedTagId={null}
        onSelect={vi.fn()}
        mode="facets"
      />,
    );
    // The "All" row should show count 4 (= sum of visible).
    // WHY getByText + closest: the button's accessible name includes the count
    // span ("All 4"), so /^All$/i would not match the full accessible name.
    // Navigating to the wrapping button via closest is the stable pattern here.
    const allRow = screen.getByText("All").closest("button")!;
    expect(allRow.textContent).toContain("4");
  });

  it("shows all tags in mode=all regardless of counts", () => {
    render(
      <TagSidebar
        tags={tags}
        counts={{ t1: 3 }}
        totalCount={100}
        selectedTagId={null}
        onSelect={vi.fn()}
        mode="all"
      />,
    );
    expect(screen.getByText("vacation")).toBeInTheDocument();
    expect(screen.getByText("sunset")).toBeInTheDocument();
    expect(screen.getByText("beach")).toBeInTheDocument();
  });
});
