/**
 * Tests for {@link LocationStatusBadge} + {@link isUnavailable}.
 *
 * The point of the component is that an abnormal status is *visibly*
 * different from a normal one, so these assert on the distinction rather
 * than on exact class strings — pinning Tailwind classes would make the
 * test a change-detector for restyling.
 */

import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import {
  LocationStatusBadge,
  isUnavailable,
} from "../components/LocationStatusBadge";

describe("LocationStatusBadge", () => {
  it("renders the status text for every known state", () => {
    for (const s of ["active", "missing", "moved", "stale"]) {
      // WHY getAllByText: the badge nests an `sr-only` span inside the
      // visible one, so the outer element's text also contains the
      // label. Two matches is the correct shape, not a duplicate render
      // — assert the label is present rather than unique.
      const { container, unmount } = render(<LocationStatusBadge status={s} />);
      expect(screen.getAllByText(s, { exact: false }).length).toBeGreaterThan(0);
      expect(container.textContent).toContain(s);
      unmount();
    }
  });

  it("gives missing a visual treatment that active does not have", () => {
    const { container: activeC } = render(
      <LocationStatusBadge status="active" />,
    );
    const activeCls = activeC.querySelector("span")?.className ?? "";
    const { container: missingC } = render(
      <LocationStatusBadge status="missing" />,
    );
    const missingCls = missingC.querySelector("span")?.className ?? "";

    expect(missingCls).not.toEqual(activeCls);
    expect(missingCls).toMatch(/destructive/);
    expect(activeCls).not.toMatch(/destructive/);
  });

  it("carries the meaning in text, not only in colour", () => {
    render(<LocationStatusBadge status="missing" />);
    // Screen-reader text must state the condition; a colour-only signal
    // is invisible to assistive tech and to colour-blind users.
    expect(screen.getByText(/missing from disk/i)).toBeInTheDocument();
  });

  it("renders an unknown status neutrally instead of blanking the cell", () => {
    const { container } = render(<LocationStatusBadge status="quarantined" />);
    expect(container.textContent).toContain("quarantined");
    expect(container.querySelector("span")?.className ?? "").not.toMatch(
      /destructive/,
    );
  });
});

describe("isUnavailable", () => {
  it("is true only for missing", () => {
    expect(isUnavailable("missing")).toBe(true);
    for (const s of ["active", "moved", "stale", "anything-else"]) {
      expect(isUnavailable(s)).toBe(false);
    }
  });
});
