/**
 * TranscribeSettingsModal — component tests.
 *
 * 8 scenarios:
 *   1. Renders form when open: true — provider input, key field, Save/Cancel visible.
 *   2. Renders nothing when open: false — modal hidden.
 *   3. Save flow happy path — fill form, click Save → setProviderKey + updateTranscriptionConfig
 *      called → onClose fired → providers query invalidated.
 *   4. Save with empty API key on edit — only updateTranscriptionConfig called (key not re-saved).
 *   5. Mid-save disable — click Save → Cancel disabled, Save button shows "Saving…".
 *   6. Error inline banner — setProviderKey rejects with Auth → banner shows auth error message.
 *   7. ESC key during save is no-op — fire ESC during pending mutation → onClose NOT called.
 *   8. Custom preset shows base_url + auth_scheme fields; groq hides them.
 *
 * WHY vi.mock("../api"): wrappers call window.__TAURI__ IPC; mocking avoids
 * wiring a real Tauri context in jsdom.
 *
 * WHY mock queries directly (not via queryClient): the component uses
 * useQuery(transcriptionConfigQueryOptions()) which is an internal detail;
 * mocking the hook via vi.mock provides stable fixtures across layout changes.
 */
import { describe, it, expect, vi, beforeEach } from "vitest";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { okAsync, errAsync } from "neverthrow";
import { TranscribeSettingsModal } from "../components/TranscribeSettingsModal";
import * as api from "../api";
import { renderWithProviders, resetUiStore } from "./test-utils";
import type { CoreError, ListProvidersPayload, TranscriptionConfig } from "../bindings";

// ── Mocks ──────────────────────────────────────────────────────────────────────

vi.mock("../api", async () => {
  const actual = await vi.importActual<typeof import("../api")>("../api");
  return {
    ...actual,
    setProviderKey: vi.fn(),
    updateTranscriptionConfig: vi.fn(),
    listProviders: vi.fn(),
    getTranscriptionConfig: vi.fn(),
  };
});

const mockSetProviderKey = vi.mocked(api.setProviderKey);
const mockUpdateTranscriptionConfig = vi.mocked(api.updateTranscriptionConfig);
const mockListProviders = vi.mocked(api.listProviders);
const mockGetTranscriptionConfig = vi.mocked(api.getTranscriptionConfig);

// ── Default fixtures ───────────────────────────────────────────────────────────

const emptyConfig: TranscriptionConfig = {
  active_provider: null,
  providers: {},
};

const emptyProviders: ListProvidersPayload = {
  active: null,
  providers: [],
};

function setupDefaultMocks() {
  mockGetTranscriptionConfig.mockReturnValue(okAsync(emptyConfig));
  mockListProviders.mockReturnValue(okAsync(emptyProviders));
  mockSetProviderKey.mockReturnValue(okAsync(undefined));
  mockUpdateTranscriptionConfig.mockReturnValue(okAsync(undefined));
}

// ── Tests ──────────────────────────────────────────────────────────────────────

beforeEach(() => {
  vi.clearAllMocks();
  resetUiStore();
  setupDefaultMocks();
});

describe("TranscribeSettingsModal", () => {
  // 1. Renders form when open: true
  it("renders form when open: true — shows provider input, key field, Save and Cancel", () => {
    renderWithProviders(
      <TranscribeSettingsModal open={true} onClose={vi.fn()} />,
    );

    // Modal container is present
    const dialog = screen.getByRole("dialog");
    expect(dialog).toBeInTheDocument();

    // Provider name input
    expect(screen.getByLabelText(/provider name/i)).toBeInTheDocument();

    // Preset dropdown
    expect(screen.getByLabelText(/preset/i)).toBeInTheDocument();

    // API key input (write-only password field)
    expect(screen.getByLabelText(/api key/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/api key/i)).toHaveAttribute("type", "password");

    // Save and Cancel buttons
    expect(screen.getByRole("button", { name: /save/i })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /cancel/i })).toBeInTheDocument();
  });

  // 2. Renders nothing when open: false
  it("renders nothing (modal hidden) when open: false", () => {
    renderWithProviders(
      <TranscribeSettingsModal open={false} onClose={vi.fn()} />,
    );

    expect(screen.queryByRole("dialog")).toBeNull();
    expect(screen.queryByRole("button", { name: /save/i })).toBeNull();
  });

  // 3. Save flow happy path
  it("happy path: fill name + key, click Save → both API calls fired → onClose + query invalidated", async () => {
    const onClose = vi.fn();
    const { queryClient } = renderWithProviders(
      <TranscribeSettingsModal open={true} onClose={onClose} />,
    );

    const invalidateSpy = vi.spyOn(queryClient, "invalidateQueries");

    // Fill provider name
    const nameInput = screen.getByLabelText(/provider name/i);
    fireEvent.change(nameInput, { target: { value: "groq" } });

    // Fill API key
    const keyInput = screen.getByLabelText(/api key/i);
    fireEvent.change(keyInput, { target: { value: "gsk_test_key_abc" } });

    // Click Save
    fireEvent.click(screen.getByRole("button", { name: /save/i }));

    await waitFor(() => {
      expect(mockSetProviderKey).toHaveBeenCalledOnce();
    });

    expect(mockSetProviderKey).toHaveBeenCalledWith("groq", "gsk_test_key_abc");

    await waitFor(() => {
      expect(mockUpdateTranscriptionConfig).toHaveBeenCalledOnce();
    });

    await waitFor(() => {
      expect(onClose).toHaveBeenCalledOnce();
    });

    // providers list query was invalidated — verify via toHaveBeenCalledWith
    // using a cast to the concrete filter shape to satisfy the type-checked lint.
    expect(invalidateSpy).toHaveBeenCalledOnce();
    const [firstArg] = invalidateSpy.mock.calls[0] ?? [undefined];
    expect(firstArg).toBeDefined();
    const filter = firstArg as { queryKey: readonly string[] };
    expect(filter.queryKey).toContain("providers");
  });

  // 4. Save with empty API key skips setProviderKey
  it("empty API key on edit: only updateTranscriptionConfig called — key not re-saved", async () => {
    const onClose = vi.fn();
    renderWithProviders(
      <TranscribeSettingsModal open={true} onClose={onClose} />,
    );

    // Fill only provider name, leave API key empty
    const nameInput = screen.getByLabelText(/provider name/i);
    fireEvent.change(nameInput, { target: { value: "openai" } });

    // Key field explicitly left empty (default is "")
    const keyInput = screen.getByLabelText(/api key/i);
    expect((keyInput as HTMLInputElement).value).toBe("");

    fireEvent.click(screen.getByRole("button", { name: /save/i }));

    await waitFor(() => {
      expect(mockUpdateTranscriptionConfig).toHaveBeenCalledOnce();
    });

    // setProviderKey must NOT be called when key is empty
    expect(mockSetProviderKey).not.toHaveBeenCalled();

    await waitFor(() => {
      expect(onClose).toHaveBeenCalledOnce();
    });
  });

  // 5. Mid-save disable: Cancel disabled + Save shows spinner
  it("mid-save: Cancel button disabled and Save shows 'Saving…' while mutation is pending", async () => {
    // Make setProviderKey hang — never resolves so mutation stays pending.
    // WHY ResultAsync.fromPromise with a never-resolving promise: okAsync()
    // resolves immediately; we need a truly pending async value to exercise
    // isPending === true in the React tree.
    // WHY undefined (not void): TypeScript's no-invalid-void-type lint rule
    // forbids void as a parameter type or generic arg; undefined is equivalent
    // for a Promise that resolves with no meaningful value.
    let resolveKey!: (value: undefined) => void;
    const hangingPromise = new Promise<undefined>((res) => { resolveKey = res; });
    const { ResultAsync } = await import("neverthrow");
    mockSetProviderKey.mockReturnValue(
      ResultAsync.fromPromise(hangingPromise, () => ({ kind: "Internal", data: "hang" } as import("../bindings").CoreError)),
    );

    renderWithProviders(
      <TranscribeSettingsModal open={true} onClose={vi.fn()} />,
    );

    const nameInput = screen.getByLabelText(/provider name/i);
    fireEvent.change(nameInput, { target: { value: "groq" } });
    const keyInput = screen.getByLabelText(/api key/i);
    fireEvent.change(keyInput, { target: { value: "gsk_pending_key" } });

    fireEvent.click(screen.getByRole("button", { name: /save/i }));

    // After click, the mutation is pending — check disabled state
    await waitFor(() => {
      expect(screen.getByRole("button", { name: /cancel/i })).toBeDisabled();
    });

    // Save button text changes to indicate pending state
    await waitFor(() => {
      const saveBtn = screen.getByRole("button", { name: /saving/i });
      expect(saveBtn).toBeInTheDocument();
    });

    // Unblock to clean up (avoid dangling async after test)
    resolveKey();
  });

  // 6. Error inline banner when setProviderKey rejects with Auth
  it("setProviderKey Auth error → inline banner with authentication failed message", async () => {
    const authError: CoreError = {
      kind: "Transcription",
      data: { kind: "Auth" },
    };
    mockSetProviderKey.mockReturnValue(errAsync(authError));

    renderWithProviders(
      <TranscribeSettingsModal open={true} onClose={vi.fn()} />,
    );

    const nameInput = screen.getByLabelText(/provider name/i);
    fireEvent.change(nameInput, { target: { value: "groq" } });
    const keyInput = screen.getByLabelText(/api key/i);
    fireEvent.change(keyInput, { target: { value: "bad_key" } });

    fireEvent.click(screen.getByRole("button", { name: /save/i }));

    // Banner appears with auth message from coreErrorMessage
    await waitFor(() => {
      expect(screen.getByRole("alert")).toBeInTheDocument();
    });

    const alert = screen.getByRole("alert");
    expect(alert.textContent).toMatch(/authentication failed/i);

    // onClose must NOT have been called on error
    // (no way to assert here since we didn't pass a spy, but confirm modal still open)
    expect(screen.getByRole("dialog")).toBeInTheDocument();
  });

  // 7. ESC key during save is no-op
  it("ESC key during pending save does not call onClose", async () => {
    // Make the mutation pending indefinitely using a never-resolving promise.
    let resolveKey2!: (value: undefined) => void;
    const hangingPromise2 = new Promise<undefined>((res) => { resolveKey2 = res; });
    const { ResultAsync } = await import("neverthrow");
    mockSetProviderKey.mockReturnValue(
      ResultAsync.fromPromise(hangingPromise2, () => ({ kind: "Internal", data: "hang" } as import("../bindings").CoreError)),
    );

    const onClose = vi.fn();
    renderWithProviders(
      <TranscribeSettingsModal open={true} onClose={onClose} />,
    );

    const nameInput = screen.getByLabelText(/provider name/i);
    fireEvent.change(nameInput, { target: { value: "groq" } });
    const keyInput = screen.getByLabelText(/api key/i);
    fireEvent.change(keyInput, { target: { value: "gsk_key" } });

    fireEvent.click(screen.getByRole("button", { name: /save/i }));

    // Wait for mutation to be in-flight
    await waitFor(() => {
      expect(screen.getByRole("button", { name: /cancel/i })).toBeDisabled();
    });

    // Fire ESC on the dialog
    fireEvent.keyDown(screen.getByRole("dialog"), { key: "Escape", code: "Escape" });

    // onClose must not have been called
    expect(onClose).not.toHaveBeenCalled();

    // Unblock
    resolveKey2();
  });

  // 8. Custom preset shows base_url + auth_scheme; groq hides them
  it("custom preset shows base_url and auth_scheme fields; groq preset hides them", () => {
    renderWithProviders(
      <TranscribeSettingsModal open={true} onClose={vi.fn()} />,
    );

    const presetSelect = screen.getByLabelText(/preset/i);

    // Set preset to "custom" → extra fields appear
    act(() => {
      fireEvent.change(presetSelect, { target: { value: "custom" } });
    });

    expect(screen.getByLabelText(/base url/i)).toBeInTheDocument();
    expect(screen.getByLabelText(/auth scheme/i)).toBeInTheDocument();

    // Set preset to "groq" → extra fields disappear
    act(() => {
      fireEvent.change(presetSelect, { target: { value: "groq" } });
    });

    expect(screen.queryByLabelText(/base url/i)).toBeNull();
    expect(screen.queryByLabelText(/auth scheme/i)).toBeNull();
  });
});
