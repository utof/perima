import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { okAsync } from "neverthrow";
import ScanButton from "../components/ScanButton";
import type { ScanResult } from "../types";

// WHY: api module calls invoke internally; mock it at the module level so
// tests never touch the real Tauri runtime.
vi.mock("../api", () => ({
  scan: vi.fn(),
}));

// The dialog mock is set up globally in setup.ts.
import { open as dialogOpen } from "@tauri-apps/plugin-dialog";
import * as api from "../api";

const mockOpen = vi.mocked(dialogOpen);
const mockScan = vi.mocked(api.scan);

const mockResult: ScanResult = { total: 10, new: 3, existing: 7, errors: 0 };

beforeEach(() => {
  vi.clearAllMocks();
});

describe("ScanButton", () => {
  it("renders the Scan Folder button", () => {
    render(
      <ScanButton
        onScanComplete={vi.fn()}
        onScanStart={vi.fn()}
        scanning={false}
      />,
    );
    expect(screen.getByRole("button", { name: /scan folder/i })).toBeInTheDocument();
  });

  it("shows Scanning... and is disabled while scanning prop is true", () => {
    render(
      <ScanButton
        onScanComplete={vi.fn()}
        onScanStart={vi.fn()}
        scanning={true}
      />,
    );
    const btn = screen.getByRole("button");
    expect(btn).toBeDisabled();
    expect(btn).toHaveTextContent(/scanning/i);
  });

  it("on click: opens dialog, calls scan, then onScanComplete", async () => {
    mockOpen.mockResolvedValue("/home/user/photos");
    // WHY: okAsync creates a proper ResultAsync that satisfies the neverthrow type.
    mockScan.mockReturnValue(okAsync(mockResult));

    const onScanStart = vi.fn();
    const onScanComplete = vi.fn();

    render(
      <ScanButton
        onScanComplete={onScanComplete}
        onScanStart={onScanStart}
        scanning={false}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /scan folder/i }));

    await waitFor(() => {
      expect(mockOpen).toHaveBeenCalledWith({ directory: true, multiple: false });
      expect(mockScan).toHaveBeenCalledWith("/home/user/photos", false);
      expect(onScanStart).toHaveBeenCalled();
      // WHY: onScanComplete now receives (result, path) so App.tsx can
      // auto-start the filesystem watcher on the scanned folder.
      expect(onScanComplete).toHaveBeenCalledWith(mockResult, "/home/user/photos");
    });
  });
});
