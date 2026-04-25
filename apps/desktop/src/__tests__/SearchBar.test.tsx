/**
 * SearchBar — dual-field store dispatch.
 *
 * Behaviours under test:
 *   - typing updates `searchQuery` synchronously (per keystroke).
 *   - debouncedQuery follows after 300ms with `buildFtsQuery(input)`.
 *   - input below MIN_QUERY_LEN (2) clears `debouncedQuery` to "".
 *   - parent re-render does not re-fire the dispatch loop (no IPC drift).
 *
 * Mocks: none for `api` — SearchBar no longer calls `api.search` directly
 * (Batch H Task 8b moved that into `useSearch` via TanStack Query). The
 * test only asserts on store mutations.
 *
 * WHY fireEvent.change (not userEvent.type): userEvent v14 with fake
 * timers requires `advanceTimers` callback wiring which interacts poorly
 * with React 19's controlled-input scheduling — keystrokes hang waiting
 * for microtasks that fake-timers won't advance. fireEvent.change is
 * synchronous + the same React-controlled-input path our prod code hits.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, fireEvent, screen } from "@testing-library/react";
import SearchBar from "../components/SearchBar";
import { useUiStore } from "../stores/ui";
import { renderWithProviders, resetUiStore } from "./test-utils";

beforeEach(() => {
  resetUiStore();
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

function getInput(): HTMLInputElement {
  return screen.getByRole<HTMLInputElement>("searchbox");
}

async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

describe("SearchBar", () => {
  it("renders a search input bound to searchQuery", () => {
    renderWithProviders(<SearchBar />);
    const input = getInput();
    expect(input).toBeInTheDocument();
    expect(input.value).toBe("");
  });

  it("typing updates searchQuery synchronously per keystroke", () => {
    renderWithProviders(<SearchBar />);
    fireEvent.change(getInput(), { target: { value: "ab" } });
    expect(useUiStore.getState().searchQuery).toBe("ab");
  });

  it("does NOT set debouncedQuery for single-char input even past 300ms", async () => {
    renderWithProviders(<SearchBar />);
    fireEvent.change(getInput(), { target: { value: "a" } });
    await advance(300);
    expect(useUiStore.getState().debouncedQuery).toBe("");
  });

  it("sets debouncedQuery for two-char input after 300ms", async () => {
    renderWithProviders(<SearchBar />);
    fireEvent.change(getInput(), { target: { value: "ab" } });

    // Before 300ms: debouncedQuery still empty.
    expect(useUiStore.getState().debouncedQuery).toBe("");

    await advance(300);

    // buildFtsQuery("ab") → '"ab"*' per the auto-prefix sanitiser
    // (added 2026-04-25: plain queries get last-token-prefix matching).
    expect(useUiStore.getState().debouncedQuery).toBe('"ab"*');
  });

  it("sets debouncedQuery for a multi-word input after 300ms", async () => {
    renderWithProviders(<SearchBar />);
    fireEvent.change(getInput(), { target: { value: "sunset" } });
    await advance(300);
    expect(useUiStore.getState().debouncedQuery).toBe('"sunset"*');
  });

  it("clearing the input back below MIN_QUERY_LEN resets debouncedQuery to ''", async () => {
    renderWithProviders(<SearchBar />);
    fireEvent.change(getInput(), { target: { value: "sunset" } });
    await advance(300);
    expect(useUiStore.getState().debouncedQuery).toBe('"sunset"*');

    fireEvent.change(getInput(), { target: { value: "" } });
    expect(useUiStore.getState().searchQuery).toBe("");

    // Falling below MIN_QUERY_LEN takes the immediate-clear path.
    // Need a render flush for the effect to run; advance(0) does that.
    await advance(0);
    expect(useUiStore.getState().debouncedQuery).toBe("");
  });

  it("does not re-dispatch debouncedQuery when re-rendered with the same input (C1 regression)", async () => {
    const { rerender } = renderWithProviders(<SearchBar />);
    fireEvent.change(getInput(), { target: { value: "sunset" } });
    await advance(300);
    expect(useUiStore.getState().debouncedQuery).toBe('"sunset"*');

    // Spy on the underlying store setState — patching individual action
    // functions via vi.spyOn would change their identity and cause the
    // SearchBar useEffect deps to fire (false positive). setState is the
    // single funnel every action passes through.
    const setStateSpy = vi.spyOn(useUiStore, "setState");

    // Re-render with same children — store state unchanged.
    rerender(<SearchBar />);
    await advance(300);

    // No keystroke + no input change → no new debounced setState.
    expect(setStateSpy).not.toHaveBeenCalled();
  });
});
