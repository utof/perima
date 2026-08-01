/**
 * Tests for {@link verifyMessage}.
 *
 * The load-bearing case is the partial sweep: when a volume is not
 * mounted, its rows are neither checked nor marked, and a message that
 * reports only "no changes" would read as a clean bill of health over
 * files the sweep never looked at.
 */

import { describe, expect, it } from "vitest";

import type { VerifyReport } from "../bindings";
import { verifyMessage } from "../queries/verify";

const report = (over: Partial<VerifyReport> = {}): VerifyReport => ({
  checked: 0,
  newly_missing: 0,
  recovered: 0,
  skipped_unmounted: 0,
  rows_written: 0,
  completed: true,
  ...over,
});

describe("verifyMessage", () => {
  it("reports a clean sweep as no changes", () => {
    const m = verifyMessage(report({ checked: 42 }));
    expect(m).toContain("Checked 42");
    expect(m).toContain("no changes");
    expect(m).not.toContain("skipped");
  });

  it("always surfaces skipped unmounted locations", () => {
    const m = verifyMessage(report({ checked: 78, skipped_unmounted: 417 }));
    expect(m).toContain("417");
    expect(m).toContain("not checked");
  });

  it("never claims 'no changes' alone when rows were skipped", () => {
    // A sweep that saw nothing because everything was on an unplugged
    // drive must not read as a healthy library.
    const m = verifyMessage(report({ checked: 0, skipped_unmounted: 500 }));
    expect(m).toContain("500");
    expect(m).toMatch(/not checked/);
  });

  it("reports missing and recovered counts", () => {
    const m = verifyMessage(report({ checked: 10, newly_missing: 3, recovered: 2 }));
    expect(m).toContain("3 now missing");
    expect(m).toContain("2 recovered");
    expect(m).not.toContain("no changes");
  });

  it("flags a cancelled sweep as having written nothing", () => {
    const m = verifyMessage(report({ checked: 5, completed: false }));
    expect(m).toContain("cancelled");
    expect(m).toContain("nothing written");
  });
});
