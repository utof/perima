import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
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
        selectedTagId={"id-1"}
        onSelect={() => {}}
      />,
    );
    const vacationBtn = screen.getByRole("button", { name: /vacation/i });
    expect(vacationBtn).toHaveAttribute("aria-pressed", "true");
  });
});
