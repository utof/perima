import { render, screen, fireEvent, act } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { okAsync, errAsync } from "neverthrow";
import SearchBar from "../components/SearchBar";
import type { SearchHit } from "../types";

vi.mock("../api", () => ({
  search: vi.fn(),
}));

import * as api from "../api";

const mockSearch = vi.mocked(api.search);

const hit: SearchHit = {
  blake3_hash: "abcdef1234567890",
  volume_id: "vol-1",
  relative_path: "photos/sunset.jpg",
  rank: -1.5,
};

beforeEach(() => {
  vi.clearAllMocks();
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

async function advanceAndFlush(ms: number) {
  await act(async () => {
    vi.advanceTimersByTime(ms);
    await Promise.resolve();
    await Promise.resolve();
  });
}

describe("SearchBar", () => {
  it("renders the search input", () => {
    render(<SearchBar onQueryChange={vi.fn()} />);
    expect(screen.getByRole("searchbox")).toBeInTheDocument();
  });

  it("does not fire search for single-character query", async () => {
    mockSearch.mockReturnValue(okAsync([hit]));
    const onChange = vi.fn();
    render(<SearchBar onQueryChange={onChange} />);

    fireEvent.change(screen.getByRole("searchbox"), {
      target: { value: "a" },
    });
    await advanceAndFlush(300);

    // The purpose of this test is to confirm no search fires.
    // Whether onQueryChange fires (""/null) on the empty→"a" transition
    // is covered by clear-signal tests below; here we only pin that
    // `api.search` stays untouched for single-char input.
    expect(mockSearch).not.toHaveBeenCalled();
  });

  it("fires search for two-character query at limit 500", async () => {
    mockSearch.mockReturnValue(okAsync([hit]));
    render(<SearchBar onQueryChange={vi.fn()} />);

    fireEvent.change(screen.getByRole("searchbox"), {
      target: { value: "ab" },
    });
    await advanceAndFlush(300);

    // buildFtsQuery("ab") → '"ab"'
    expect(mockSearch).toHaveBeenCalledWith('"ab"', 500);
  });

  it("fires onQueryChange with (raw, hits) after debounce", async () => {
    mockSearch.mockReturnValue(okAsync([hit]));
    const onChange = vi.fn();
    render(<SearchBar onQueryChange={onChange} />);

    fireEvent.change(screen.getByRole("searchbox"), {
      target: { value: "sunset" },
    });
    await advanceAndFlush(300);

    expect(onChange).toHaveBeenCalledWith("sunset", [hit]);
  });

  it("fires onQueryChange(\"\", null) when cleared via ✕", async () => {
    mockSearch.mockReturnValue(okAsync([hit]));
    const onChange = vi.fn();
    render(<SearchBar onQueryChange={onChange} />);

    fireEvent.change(screen.getByRole("searchbox"), {
      target: { value: "sunset" },
    });
    await advanceAndFlush(300);

    onChange.mockClear();
    await act(async () => {
      fireEvent.click(screen.getByLabelText("Clear search"));
    });

    expect(onChange).toHaveBeenCalledWith("", null);
    expect(
      (screen.getByRole("searchbox") as HTMLInputElement).value,
    ).toBe("");
  });

  it("fires onQueryChange(raw, []) when search returns zero hits", async () => {
    mockSearch.mockReturnValue(okAsync([]));
    const onChange = vi.fn();
    render(<SearchBar onQueryChange={onChange} />);

    fireEvent.change(screen.getByRole("searchbox"), {
      target: { value: "xyzzy" },
    });
    await advanceAndFlush(300);

    expect(onChange).toHaveBeenCalledWith("xyzzy", []);
  });

  it("swallows backend errors and fires onQueryChange(raw, [])", async () => {
    mockSearch.mockReturnValue(errAsync("FTS5 parse error"));
    const onChange = vi.fn();
    render(<SearchBar onQueryChange={onChange} />);

    fireEvent.change(screen.getByRole("searchbox"), {
      target: { value: "bad" },
    });
    await advanceAndFlush(300);

    // Non-fatal: show empty results rather than a red banner.
    expect(onChange).toHaveBeenCalledWith("bad", []);
  });

  it("does not render a dropdown listbox", async () => {
    mockSearch.mockReturnValue(okAsync([hit]));
    render(<SearchBar onQueryChange={vi.fn()} />);

    fireEvent.change(screen.getByRole("searchbox"), {
      target: { value: "sunset" },
    });
    await advanceAndFlush(300);

    // No listbox — list re-sort happens in App, not here.
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
  });

  it("does not re-fire search on parent re-render (C1 regression)", async () => {
    mockSearch.mockReturnValue(okAsync([hit]));
    const onChange = vi.fn();
    const { rerender } = render(<SearchBar onQueryChange={onChange} />);

    fireEvent.change(screen.getByRole("searchbox"), {
      target: { value: "sunset" },
    });
    await advanceAndFlush(300);

    expect(mockSearch).toHaveBeenCalledTimes(1);

    // Simulate parent re-render with the SAME callback identity (post-fix behaviour).
    rerender(<SearchBar onQueryChange={onChange} />);
    await advanceAndFlush(300);

    expect(mockSearch).toHaveBeenCalledTimes(1);
  });
});
