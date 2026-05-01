/**
 * CollisionPill — design-token pill per spec §4.6.1.
 *
 * Branches under test:
 * - 0 groups: muted-foreground span, "no candidate duplicates"
 * - N groups, 0 verified: warning pill, "N duplicate(s)"
 * - 1 group, 0 verified: warning pill, "1 duplicate" (singular)
 * - N groups, 0 less than M less than N verified: warning pill, "N duplicates (M verified)"
 * - all verified: success pill, "all verified"
 *
 * WHY mock `@tanstack/react-router`: CollisionPill renders a Link to="/dedup"
 * which requires a router context. Unit tests for a leaf pill component do not
 * need full router routing; mocking Link as a plain anchor keeps the test
 * focused on color-state rendering without router scaffolding.
 */
import { describe, it, expect, vi } from "vitest";
import { screen } from "@testing-library/react";
import CollisionPill from "../components/CollisionPill";
import type { CollisionGroup } from "../bindings";
import { renderWithProviders } from "./test-utils";

// WHY mock Link: avoids RouterProvider scaffolding for a pure leaf component.
// The href is set to `to` so tests can assert navigation target if needed.
vi.mock("@tanstack/react-router", () => ({
  Link: ({
    to,
    className,
    children,
    title,
  }: {
    to: string;
    className?: string;
    children: React.ReactNode;
    title?: string;
  }) => (
    <a href={to} className={className} title={title}>
      {children}
    </a>
  ),
}));

// ── Helpers ──────────────────────────────────────────────────────────────────

function makeGroup(
  quickHash: string,
  verifiedState: CollisionGroup["verified_state"],
): CollisionGroup {
  return {
    quick_hash: quickHash,
    files: [],
    verified_state: verifiedState,
  };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

describe("CollisionPill", () => {
  it("renders muted 'no candidate duplicates' span when groups is empty", () => {
    renderWithProviders(<CollisionPill groups={[]} />);
    const el = screen.getByText(/no candidate duplicates/i);
    expect(el).toBeInTheDocument();
    // 0-groups state = plain span (not a link), text-muted-foreground.
    expect(el.tagName).toBe("SPAN");
    expect(el.className).toContain("text-muted-foreground");
  });

  it("renders warning pill for N unverified groups (plural)", () => {
    const groups = [
      makeGroup("aaa", "Unverified"),
      makeGroup("bbb", "Unverified"),
      makeGroup("ccc", "Unverified"),
    ];
    renderWithProviders(<CollisionPill groups={groups} />);
    const link = screen.getByRole("link");
    expect(link).toBeInTheDocument();
    expect(link.textContent).toMatch(/3 duplicates/i);
    expect(link.className).toContain("bg-warning");
  });

  it("renders warning pill with singular 'duplicate' label for 1 unverified group", () => {
    renderWithProviders(<CollisionPill groups={[makeGroup("aaa", "Unverified")]} />);
    const link = screen.getByRole("link");
    expect(link.textContent).toMatch(/1 duplicate$/i);
    // Must NOT be plural.
    expect(link.textContent).not.toMatch(/1 duplicates/i);
    expect(link.className).toContain("bg-warning");
  });

  it("renders warning pill with verified count when 0 < M < N verified", () => {
    const groups = [
      makeGroup("aaa", "VerifiedDuplicate"),
      makeGroup("bbb", "Unverified"),
      makeGroup("ccc", "VerifiedDistinct"),
    ];
    renderWithProviders(<CollisionPill groups={groups} />);
    const link = screen.getByRole("link");
    // 3 total, 2 verified (VerifiedDuplicate + VerifiedDistinct both count).
    expect(link.textContent).toMatch(/3 duplicates \(2 verified\)/i);
    expect(link.className).toContain("bg-warning");
  });

  it("renders success pill when all groups are verified", () => {
    const groups = [
      makeGroup("aaa", "VerifiedDuplicate"),
      makeGroup("bbb", "VerifiedDistinct"),
    ];
    renderWithProviders(<CollisionPill groups={groups} />);
    const link = screen.getByRole("link");
    expect(link.textContent).toMatch(/all verified/i);
    expect(link.className).toContain("bg-success");
  });

  it("link points to /dedup", () => {
    renderWithProviders(<CollisionPill groups={[makeGroup("aaa", "Unverified")]} />);
    const link = screen.getByRole("link");
    expect(link).toHaveAttribute("href", "/dedup");
  });

  it("Mixed state counts as unverified (not fully verified)", () => {
    const groups = [makeGroup("aaa", "Mixed")];
    renderWithProviders(<CollisionPill groups={groups} />);
    const link = screen.getByRole("link");
    // Mixed is not VerifiedDuplicate or VerifiedDistinct → not in verified count.
    // With 0 verified, label = "1 duplicate".
    expect(link.textContent).toMatch(/1 duplicate$/i);
    expect(link.className).toContain("bg-warning");
  });
});
