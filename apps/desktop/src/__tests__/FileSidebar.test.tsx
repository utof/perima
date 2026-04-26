/**
 * FileSidebar — Task 14 spec §4.6.3 minimal stub tests.
 *
 * Branches under test:
 *   - Renders file UUID prefix (first 8 chars).
 *   - Renders "pending" label when hash is null.
 *   - Shows "Compute canonical hash" button when hash is null.
 *   - Clicking Compute triggers the mutation with the correct file_uuid.
 *   - Hides Compute button when hash is present (non-null).
 *   - Renders the full hash value when non-null.
 *   - Close button calls onClose.
 *
 * WHY mock `../queries/dedup`: the mutation hook calls `api.computeFullHash`
 * which invokes `window.__TAURI__`; mocking the hook avoids wiring a real
 * Tauri context in jsdom.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { fireEvent, screen } from "@testing-library/react";
import FileSidebar from "../components/FileSidebar";
import type { FileWithTagsPayload } from "../bindings";
import { renderWithProviders, resetUiStore } from "./test-utils";

// ── Mocks ─────────────────────────────────────────────────────────────────────

const mockMutate = vi.fn();

vi.mock("../queries/dedup", async () => {
  const actual = await vi.importActual<typeof import("../queries/dedup")>(
    "../queries/dedup",
  );
  return {
    ...actual,
    useComputeFullHash: () => ({
      mutate: mockMutate,
      isPending: false,
    }),
  };
});

// ── Fixtures ──────────────────────────────────────────────────────────────────

function makeFile(overrides?: Partial<FileWithTagsPayload>): FileWithTagsPayload {
  return {
    file_uuid: "12345678-0000-0000-0000-000000000001",
    hash: null,
    size: 4_096,
    volume_id: "vol-0000-0000-0000",
    relative_path: "photos/img_1.jpg",
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
    ...overrides,
  };
}

// ── Tests ─────────────────────────────────────────────────────────────────────

describe("FileSidebar", () => {
  beforeEach(() => {
    resetUiStore();
    mockMutate.mockClear();
  });

  it("renders the first 8 chars of the file UUID", () => {
    const file = makeFile();
    renderWithProviders(
      <FileSidebar file={file} onClose={vi.fn()} />,
    );
    const uuidEl = screen.getByTestId("file-uuid");
    // WHY slice(0,8): the component renders an 8-char prefix (matching
    // the file list's monospace column style). Full UUID is in the DB.
    expect(uuidEl.textContent).toContain(file.file_uuid.slice(0, 8));
  });

  it("shows 'pending' label and Compute button when hash is null", () => {
    const file = makeFile({ hash: null });
    renderWithProviders(
      <FileSidebar file={file} onClose={vi.fn()} />,
    );
    expect(screen.getByTestId("hash-pending")).toBeInTheDocument();
    expect(screen.getByTestId("compute-hash-btn")).toBeInTheDocument();
  });

  it("clicking Compute calls mutate with the correct file_uuid", () => {
    const file = makeFile({ hash: null });
    renderWithProviders(
      <FileSidebar file={file} onClose={vi.fn()} />,
    );
    fireEvent.click(screen.getByTestId("compute-hash-btn"));
    expect(mockMutate).toHaveBeenCalledOnce();
    expect(mockMutate).toHaveBeenCalledWith({ fileUuid: file.file_uuid });
  });

  it("hides Compute button and shows hash when full_hash is present", () => {
    const hash = "a".repeat(64);
    const file = makeFile({ hash });
    renderWithProviders(
      <FileSidebar file={file} onClose={vi.fn()} />,
    );
    expect(screen.queryByTestId("compute-hash-btn")).toBeNull();
    expect(screen.queryByTestId("hash-pending")).toBeNull();
    const hashEl = screen.getByTestId("full-hash");
    expect(hashEl.textContent).toBe(hash);
  });

  it("calls onClose when the close button is clicked", () => {
    const onClose = vi.fn();
    const file = makeFile();
    renderWithProviders(
      <FileSidebar file={file} onClose={onClose} />,
    );
    fireEvent.click(screen.getByLabelText("Close file detail"));
    expect(onClose).toHaveBeenCalledOnce();
  });
});
