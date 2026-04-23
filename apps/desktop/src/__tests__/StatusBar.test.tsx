import { render, screen } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import StatusBar from "../components/StatusBar";
import type { ScanReport, CoreError } from "../bindings";

const mockResult: ScanReport = {
  files_seen: 42,
  files_new: 5,
  files_updated: 37,
  files_errored: 0,
  bytes_hashed: 4096,
  duration_ms: 100,
  interrupted: false,
  volume_label: null,
};

describe("StatusBar", () => {
  it("shows scan summary when scanResult is present", () => {
    render(<StatusBar scanResult={mockResult} error={null} />);
    expect(screen.getByText(/scanned 42 files/i)).toBeInTheDocument();
    expect(screen.getByText(/5 new/i)).toBeInTheDocument();
    expect(screen.getByText(/37 updated/i)).toBeInTheDocument();
  });

  it("shows error string when error is present", () => {
    const err: CoreError = { kind: "Internal", data: "disk read failure" };
    render(<StatusBar scanResult={null} error={err} />);
    const errEl = screen.getByText(/disk read failure/i);
    expect(errEl).toBeInTheDocument();
    // WHY: error text must be visually distinct (red) — check for a red class.
    expect(errEl.closest("[class]")).toHaveClass("text-red-400");
  });

  it("shows No scans yet when both scanResult and error are null", () => {
    render(<StatusBar scanResult={null} error={null} />);
    expect(screen.getByText("No scans yet")).toBeInTheDocument();
  });

  // WHY: These two tests pin the switch(err.kind) discriminated-union branches
  // added in Task 11 — NotFound renders distinct UX; all other variants fall
  // through to the generic catch-all path.

  it("shows 'No results found.' for NotFound errors (distinct UX branch)", () => {
    const err: CoreError = { kind: "NotFound", data: "query returned nothing" };
    render(<StatusBar scanResult={null} error={err} />);
    expect(screen.getByText("No results found.")).toBeInTheDocument();
  });

  it("shows generic error message for Internal errors (catch-all branch)", () => {
    const err: CoreError = { kind: "Internal", data: "unexpected db error" };
    render(<StatusBar scanResult={null} error={err} />);
    expect(screen.getByText(/something went wrong/i)).toBeInTheDocument();
    expect(screen.getByText(/unexpected db error/i)).toBeInTheDocument();
  });
});
