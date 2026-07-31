import { screen } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import FileTable from "../components/FileTable";
import type { FileWithTagsPayload } from "../bindings";
import { renderWithProviders } from "./test-utils";

const makeEntry = (n: number): FileWithTagsPayload => ({
  // WHY (Task 11): `file_uuid` is the React key + stable surrogate. Distinct
  // per-fixture so the table doesn't collapse rows on a duplicate key.
  file_uuid: `00000000-0000-0000-0000-${String(n).padStart(12, "0")}`,
  hash: "a".repeat(62) + String(n).padStart(2, "0"),
  quick_hash: null,
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
  absolute_path: `/mnt/test/photos/img_${n}.jpg`,
  tags: [],
});

describe("FileTable", () => {
  it("renders a row for each FileWithTagsPayload", () => {
    const files = [makeEntry(1), makeEntry(2), makeEntry(3)];
    renderWithProviders(<FileTable files={files} loading={false} />);

    // 3 data rows (the header row is a separate <thead>)
    const rows = screen.getAllByRole("row");
    // 1 header + 3 data rows
    expect(rows).toHaveLength(4);
    expect(screen.getByText(/img_1\.jpg/)).toBeInTheDocument();
    expect(screen.getByText(/img_2\.jpg/)).toBeInTheDocument();
    expect(screen.getByText(/img_3\.jpg/)).toBeInTheDocument();
  });

  it("shows empty-state message when files array is empty", () => {
    renderWithProviders(<FileTable files={[]} loading={false} />);
    expect(screen.getByText("No files indexed yet")).toBeInTheDocument();
  });

  it("shows loading indicator when loading prop is true", () => {
    renderWithProviders(<FileTable files={[]} loading={true} />);
    expect(screen.getByText("Loading...")).toBeInTheDocument();
  });

  it("renders tag chips for files with tags", () => {
    const file = makeEntry(1);
    file.tags = [
      { id: "t1", name: "vacation", first_seen: "2026-01-01T00:00:00Z" },
      { id: "t2", name: "sunset", first_seen: "2026-01-01T00:00:00Z" },
    ];
    renderWithProviders(<FileTable files={[file]} loading={false} />);
    expect(screen.getByText("vacation")).toBeInTheDocument();
    expect(screen.getByText("sunset")).toBeInTheDocument();
  });

  it("renders all tag chips inline (no overflow cap on the interactive cell)", () => {
    // WHY: prior behaviour was slice(0, 3) + "+N" badge for read-only chips.
    // The TAGS cell is now interactive (per-row tag input + click-to-detach
    // chip buttons), so capping would hide tags the user wants to edit.
    // Trade-off: very-many-tags rows wrap onto multiple lines.
    const file = makeEntry(1);
    file.tags = [
      { id: "t1", name: "a", first_seen: "2026-01-01T00:00:00Z" },
      { id: "t2", name: "b", first_seen: "2026-01-01T00:00:00Z" },
      { id: "t3", name: "c", first_seen: "2026-01-01T00:00:00Z" },
      { id: "t4", name: "d", first_seen: "2026-01-01T00:00:00Z" },
    ];
    renderWithProviders(<FileTable files={[file]} loading={false} />);
    expect(screen.getByText("a")).toBeInTheDocument();
    expect(screen.getByText("b")).toBeInTheDocument();
    expect(screen.getByText("c")).toBeInTheDocument();
    expect(screen.getByText("d")).toBeInTheDocument();
  });

  it("renders an inline + tag input on every row", () => {
    const file = makeEntry(1);
    renderWithProviders(<FileTable files={[file]} loading={false} />);
    expect(
      screen.getByLabelText(`Add tag to file ${file.hash.slice(0, 8)}`),
    ).toBeInTheDocument();
  });
});
