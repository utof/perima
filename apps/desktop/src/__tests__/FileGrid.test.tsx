import { render, screen } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import FileGrid from "../components/FileGrid";
import type { FileWithMetadata } from "../types";

/**
 * Build a {@link FileWithMetadata} with every non-relevant field zeroed
 * out. Only the caller-supplied overrides (hash + thumbnail state) matter
 * for these assertions.
 */
function makeFile(overrides: Partial<FileWithMetadata>): FileWithMetadata {
  return {
    hash: "0".repeat(64),
    size: 1024,
    volume_id: "00000000-0000-0000-0000-000000000000",
    relative_path: "photos/example.jpg",
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
    ...overrides,
  };
}

describe("FileGrid", () => {
  it("renders an <img> for ready tiles and placeholders for others", () => {
    const files: FileWithMetadata[] = [
      makeFile({
        hash: "a".repeat(64),
        relative_path: "photos/ready.jpg",
        thumbnail_path: "/var/data/perima/thumbnails/aa/ready.webp",
        thumbnail_status: "ready",
      }),
      makeFile({
        hash: "b".repeat(64),
        relative_path: "photos/pending.jpg",
        thumbnail_path: null,
        thumbnail_status: "pending",
      }),
      makeFile({
        hash: "c".repeat(64),
        relative_path: "photos/failed.jpg",
        thumbnail_path: null,
        thumbnail_status: "failed",
      }),
    ];

    render(<FileGrid files={files} />);

    // Ready tile renders an <img> with the mocked convertFileSrc output
    // (setup.ts echoes the path back unchanged).
    const img = screen.getByRole("img", { name: "ready.jpg" });
    expect(img).toBeInTheDocument();
    expect(img).toHaveAttribute(
      "src",
      "/var/data/perima/thumbnails/aa/ready.webp",
    );

    // Pending + failed tiles render placeholder glyphs (no <img>).
    expect(screen.getByTestId("placeholder-pending")).toHaveTextContent(
      "\u2026",
    );
    expect(screen.getByTestId("placeholder-failed")).toHaveTextContent(
      "\u26A0",
    );

    // Exactly one <img> — the other two tiles are placeholder-only.
    expect(screen.getAllByRole("img")).toHaveLength(
      // 1 real img + 2 placeholder divs tagged role="img" for a11y
      3,
    );
  });

  it("shows empty-state when files is empty", () => {
    render(<FileGrid files={[]} />);
    expect(screen.getByText("No files indexed yet")).toBeInTheDocument();
  });

  it("shows loading indicator when loading prop is true", () => {
    render(<FileGrid files={[]} loading={true} />);
    expect(screen.getByRole("status")).toHaveTextContent("Loading...");
  });
});
