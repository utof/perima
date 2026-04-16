import { render, screen } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
import TagChip from "../components/TagChip";

// colorIndexFor is not exported, so we test it indirectly via the rendered chip.
function colorIndexFor(name: string): number {
  const bytes = new TextEncoder().encode(name);
  let sum = 0;
  for (const b of bytes) sum = (sum + b) % 256;
  return sum % 12;
}

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

  test("calls onRemove when x clicked", async () => {
    const onRemove = vi.fn();
    render(<TagChip tag={sampleTag} onRemove={onRemove} />);
    const btn = screen.getByRole("button", { name: /remove vacation/i });
    btn.click();
    expect(onRemove).toHaveBeenCalledTimes(1);
  });

  test("chip has a color class derived from tag name", () => {
    render(<TagChip tag={sampleTag} />);
    const chip = screen.getByTestId("tag-chip");
    // colorIndexFor("vacation") must be stable — pin the expected index so
    // an accidental formula change is caught immediately.
    const idx = colorIndexFor("vacation");
    const COLORS = [
      "bg-red-700","bg-orange-700","bg-amber-700","bg-yellow-700",
      "bg-lime-700","bg-green-700","bg-emerald-700","bg-teal-700",
      "bg-cyan-700","bg-sky-700","bg-blue-700","bg-indigo-700",
    ];
    expect(chip.className).toContain(COLORS[idx]);
  });
});
