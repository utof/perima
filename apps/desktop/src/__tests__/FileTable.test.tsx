import { render, screen } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import FileTable from "../components/FileTable";
import type { FileWithTags } from "../types";

const makeEntry = (n: number): FileWithTags => ({
  hash: "a".repeat(62) + String(n).padStart(2, "0"),
  size: 1024 * n,
  volume_id: "00000000-0000-0000-0000-00000000000" + n,
  relative_path: `/photos/img_${n}.jpg`,
  status: "active",
  first_seen: "2026-01-01T00:00:00Z",
  width: null,
  height: null,
  duration_ms: null,
  captured_at: null,
  camera_make: null,
  camera_model: null,
  codec: null,
  bitrate_bps: null,
  mime_type: null,
  thumbnail_path: null,
  thumbnail_status: null,
  tags: [],
});

describe("FileTable", () => {
  it("renders a row for each FileWithTags", () => {
    const files = [makeEntry(1), makeEntry(2), makeEntry(3)];
    render(<FileTable files={files} loading={false} />);

    // 3 data rows (the header row is a separate <thead>)
    const rows = screen.getAllByRole("row");
    // 1 header + 3 data rows
    expect(rows).toHaveLength(4);
    expect(screen.getByText(/img_1\.jpg/)).toBeInTheDocument();
    expect(screen.getByText(/img_2\.jpg/)).toBeInTheDocument();
    expect(screen.getByText(/img_3\.jpg/)).toBeInTheDocument();
  });

  it("shows empty-state message when files array is empty", () => {
    render(<FileTable files={[]} loading={false} />);
    expect(screen.getByText("No files indexed yet")).toBeInTheDocument();
  });

  it("shows loading indicator when loading prop is true", () => {
    render(<FileTable files={[]} loading={true} />);
    expect(screen.getByText("Loading...")).toBeInTheDocument();
  });
});
