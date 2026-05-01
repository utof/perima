import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
import TagChip from "../components/TagChip";

describe("TagChip", () => {
  const sampleTag = {
    id: "00000000-0000-0000-0000-000000000001",
    name: "vacation",
    first_seen: "2026-04-16T00:00:00Z",
  };

  test("renders the tag name", () => {
    render(<TagChip tag={sampleTag} />);
    expect(screen.getByText("vacation")).toBeInTheDocument();
  });

  test("no remove button when onRemove is absent", () => {
    render(<TagChip tag={sampleTag} />);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  test("calls onRemove when x clicked", () => {
    const onRemove = vi.fn();
    render(<TagChip tag={sampleTag} onRemove={onRemove} />);
    const btn = screen.getByRole("button", { name: /remove vacation/i });
    btn.click();
    expect(onRemove).toHaveBeenCalledTimes(1);
  });

  test("chip uses uniform bg-muted class (design-system-v1 drops per-tag coloring)", () => {
    render(<TagChip tag={sampleTag} />);
    const chip = screen.getByTestId("tag-chip");
    // WHY: design-system-v1 replaces the 12-color procedural palette with a
    // single uniform bg-muted capsule. Tag identity is conveyed by name text.
    expect(chip.className).toContain("bg-muted");
  });
});
