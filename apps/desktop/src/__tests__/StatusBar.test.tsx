import { render, screen } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import StatusBar from "../components/StatusBar";
import type { ScanResult } from "../types";

const mockResult: ScanResult = { total: 42, new: 5, existing: 37, errors: 0 };

describe("StatusBar", () => {
  it("shows scan summary when scanResult is present", () => {
    render(<StatusBar scanResult={mockResult} error={null} />);
    expect(screen.getByText(/scanned 42 files/i)).toBeInTheDocument();
    expect(screen.getByText(/5 new/i)).toBeInTheDocument();
    expect(screen.getByText(/37 existing/i)).toBeInTheDocument();
  });

  it("shows error string when error is present", () => {
    render(<StatusBar scanResult={null} error="disk read failure" />);
    const errEl = screen.getByText(/disk read failure/i);
    expect(errEl).toBeInTheDocument();
    // WHY: error text must be visually distinct (red) — check for a red class.
    expect(errEl.closest("[class]")).toHaveClass("text-red-400");
  });

  it("shows No scans yet when both scanResult and error are null", () => {
    render(<StatusBar scanResult={null} error={null} />);
    expect(screen.getByText("No scans yet")).toBeInTheDocument();
  });
});
