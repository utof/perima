/**
 * StatusBar — store-driven post-Batch-H. Reads `scan.status` and
 * `scan.lastReport` from useUiStore.
 *
 * Branches under test:
 *   - status === "scanning"            → "Scanning…"
 *   - status !== "scanning" + report   → "Last scan: N files"
 *   - status === "idle", no report     → "Ready"
 */
import { describe, it, expect } from "vitest";
import { screen } from "@testing-library/react";
import StatusBar from "../components/StatusBar";
import type { ScanReport } from "../bindings";
import { renderWithProviders } from "./test-utils";

const mockReport: ScanReport = {
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
  it("shows 'Scanning…' when scan.status === 'scanning'", () => {
    renderWithProviders(<StatusBar />, {
      initialStoreState: { scan: { status: "scanning", lastReport: null } },
    });
    expect(screen.getByText(/scanning/i)).toBeInTheDocument();
  });

  it("shows 'Scanning…' even if a previous report is present", () => {
    renderWithProviders(<StatusBar />, {
      initialStoreState: { scan: { status: "scanning", lastReport: mockReport } },
    });
    expect(screen.getByText(/scanning/i)).toBeInTheDocument();
    // The "Last scan: ..." string MUST NOT render while scanning.
    expect(screen.queryByText(/last scan/i)).not.toBeInTheDocument();
  });

  it("shows 'Last scan: N files' when status !== 'scanning' and a report is present", () => {
    renderWithProviders(<StatusBar />, {
      initialStoreState: { scan: { status: "done", lastReport: mockReport } },
    });
    expect(screen.getByText(/last scan: 42 files/i)).toBeInTheDocument();
  });

  it("shows 'Ready' when status is idle and no report", () => {
    renderWithProviders(<StatusBar />, {
      initialStoreState: { scan: { status: "idle", lastReport: null } },
    });
    expect(screen.getByText(/^ready$/i)).toBeInTheDocument();
  });

  it("defaults to 'Ready' when no initial store state is supplied", () => {
    renderWithProviders(<StatusBar />);
    expect(screen.getByText(/^ready$/i)).toBeInTheDocument();
  });
});
