/**
 * TranscribeSettingsModal — provider key + config editor.
 *
 * Opens when the user clicks the gear icon beside the TranscribeButton in
 * FileSidebar. Supports adding/editing transcription providers (Groq, OpenAI,
 * Custom). Changes are persisted via two sequential Tauri commands:
 *   1. `set_provider_key` — writes the API key to the OS keyring (only when
 *      the user supplies a non-empty key; leaving it blank preserves the
 *      existing keyring entry).
 *   2. `update_transcription_config` — writes provider preset / model /
 *      base_url / auth_scheme + optionally sets the provider as active.
 *
 * WHY plain-div modal (no shadcn Dialog): the project has no UI component
 * library installed. We implement a minimal role="dialog" with an ESC handler
 * and backdrop click guard instead of pulling in Radix (unvetted dep).
 *
 * WHY no useMemo / useCallback: React Compiler 1.0 is enabled (CLAUDE.md).
 *
 * WHY useState for open state: the parent controls open/close; this component
 * holds only ephemeral form state (not global UI state → no Zustand needed).
 */
import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { queryOptions } from "@tanstack/react-query";
import * as api from "../api";
import { coreErrorMessage } from "../lib/coreError";
import type { CoreError, ProviderEntry, TranscriptionConfig } from "../bindings";

// ── Query key + options for the provider list ────────────────────────────────

/** Stable query key used to list providers and drive cache invalidation. */
const PROVIDERS_QUERY_KEY = ["providers", "list"] as const;

/**
 * Query options factory for the transcription config.
 * Used by the modal to populate initial form state when loading.
 */
function transcriptionConfigQueryOptions() {
  return queryOptions({
    queryKey: ["transcription", "config"] as const,
    queryFn: () =>
      api.getTranscriptionConfig().match(
        (cfg) => cfg,
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        (err) => { throw err; },
      ),
  });
}

/** Query options factory for the provider list (sidebar listing). */
function providersQueryOptions() {
  return queryOptions({
    queryKey: PROVIDERS_QUERY_KEY,
    queryFn: () =>
      api.listProviders().match(
        (p) => p,
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        (err) => { throw err; },
      ),
  });
}

// ── Preset logic ─────────────────────────────────────────────────────────────

/** Known preset metadata. Drives default-model placeholder text. */
const KNOWN_PRESETS: Record<string, { defaultModel: string }> = {
  groq: { defaultModel: "whisper-large-v3" },
  openai: { defaultModel: "whisper-1" },
};

/** Derive the preset dropdown value from a provider name. */
function derivePreset(name: string): "groq" | "openai" | "custom" {
  if (name === "groq") return "groq";
  if (name === "openai") return "openai";
  return "custom";
}

// ── Form state ────────────────────────────────────────────────────────────────

interface SettingsFormState {
  /** Provider name (the TOML key under `[transcription.providers.*]`). */
  name: string;
  /** Preset: groq | openai | custom. */
  preset: "groq" | "openai" | "custom";
  /** Optional model override; empty string = use preset default. */
  model: string;
  /** Base URL for custom preset only. */
  baseUrl: string;
  /** Auth scheme for custom preset only. */
  authScheme: string;
  /** New API key — write-only. Empty = preserve existing keyring entry. */
  apiKey: string;
  /** If true, the saved provider becomes active_provider. */
  setAsActive: boolean;
}

const INITIAL_FORM: SettingsFormState = {
  name: "",
  preset: "groq",
  model: "",
  baseUrl: "",
  authScheme: "Bearer",
  apiKey: "",
  setAsActive: false,
};

// ── Props ─────────────────────────────────────────────────────────────────────

/** Props for {@link TranscribeSettingsModal}. */
export interface TranscribeSettingsModalProps {
  /** Whether the modal is visible. */
  open: boolean;
  /** Called when the modal should close (Cancel, backdrop, ESC). */
  onClose: () => void;
}

// ── Component ─────────────────────────────────────────────────────────────────

/**
 * Modal dialog for managing transcription provider configuration.
 *
 * Renders nothing when `open` is false (unmounts cleanly — no hidden-but-
 * mounted pattern). The form is fresh on every open.
 *
 * Save flow:
 *   - If `apiKey` is non-empty: call `api.setProviderKey` first.
 *   - Call `api.updateTranscriptionConfig` with the constructed provider entry.
 *   - On success: invalidate providers query + call `onClose`.
 *   - On error: show inline banner; modal stays open so the user can retry.
 */
export function TranscribeSettingsModal({ open, onClose }: TranscribeSettingsModalProps) {
  const qc = useQueryClient();

  // ── Form state ─────────────────────────────────────────────────────────────
  // WHY no useEffect reset: when open=false we return null (full unmount),
  // so useState initialises fresh on every open. No effect needed.
  const [form, setForm] = useState<SettingsFormState>(INITIAL_FORM);
  const [saveError, setSaveError] = useState<string | null>(null);

  // ── Queries ────────────────────────────────────────────────────────────────
  // WHY both queries: providers list → sidebar; config → initial active_provider.
  const providersQuery = useQuery(providersQueryOptions());
  // Config query is used to read active_provider for pre-populating the checkbox.
  // We don't destructure data — just used as a reference on save.
  const configQuery = useQuery(transcriptionConfigQueryOptions());

  // ── Save mutation ──────────────────────────────────────────────────────────
  const saveMutation = useMutation({
    mutationFn: async (f: SettingsFormState) => {
      // Step 1: persist key if supplied (write-only; empty = keep existing).
      if (f.apiKey.trim() !== "") {
        const r1 = await api.setProviderKey(f.name.trim(), f.apiKey.trim());
        if (r1.isErr()) {
          // eslint-disable-next-line @typescript-eslint/only-throw-error
          throw r1.error;
        }
      }

      // Step 2: build the updated TranscriptionConfig.
      // WHY refuse on undefined: substituting `{ providers: {} }` would
      // wipe every other configured provider on Save (the spread on the
      // next block would write a config containing only the new entry).
      // The Save button is also gated on configQuery.isSuccess to prevent
      // the user reaching this branch in normal use; this is belt + braces.
      if (configQuery.data === undefined) {
        const err: CoreError = {
          kind: "Internal",
          data: "Existing transcription config not loaded yet — refusing to overwrite providers.",
        };
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        throw err;
      }
      const existingConfig: TranscriptionConfig = configQuery.data;

      const entry: ProviderEntry = {
        preset: f.preset,
        model: f.model.trim() !== "" ? f.model.trim() : null,
        base_url: f.preset === "custom" && f.baseUrl.trim() !== "" ? f.baseUrl.trim() : null,
        auth_scheme: f.preset === "custom" && f.authScheme.trim() !== "" ? f.authScheme.trim() : null,
      };

      const updatedConfig: TranscriptionConfig = {
        active_provider: f.setAsActive ? f.name.trim() : existingConfig.active_provider,
        providers: {
          ...existingConfig.providers,
          [f.name.trim()]: entry,
        },
      };

      const r2 = await api.updateTranscriptionConfig(updatedConfig);
      if (r2.isErr()) {
        // eslint-disable-next-line @typescript-eslint/only-throw-error
        throw r2.error;
      }
    },
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: PROVIDERS_QUERY_KEY });
      onClose();
    },
    onError: (err: unknown) => {
      // err is CoreError (registered as defaultError via queryClient.ts augmentation)
      // For non-CoreError shapes (shouldn't happen from our api wrappers), fallback gracefully.
      if (typeof err === "object" && err !== null && "kind" in err) {
        setSaveError(coreErrorMessage(err as Parameters<typeof coreErrorMessage>[0]));
      } else {
        setSaveError("An unexpected error occurred. Please try again.");
      }
    },
  });

  // ── Event handlers ─────────────────────────────────────────────────────────

  function handleKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    // WHY guard: mid-save, ESC must not close the modal (spec §5.3).
    if (e.key === "Escape" && !saveMutation.isPending) {
      onClose();
    }
  }

  function handleBackdropClick() {
    // WHY guard: same mid-save lock as ESC.
    if (!saveMutation.isPending) {
      onClose();
    }
  }

  function handlePresetChange(e: React.ChangeEvent<HTMLSelectElement>) {
    const preset = e.target.value as "groq" | "openai" | "custom";
    setForm((prev) => ({
      ...prev,
      preset,
      // Auto-sync provider name to the preset when the name is empty or matches a known preset.
      name: prev.name === "" || prev.name in KNOWN_PRESETS ? preset : prev.name,
    }));
  }

  function handleNameChange(e: React.ChangeEvent<HTMLInputElement>) {
    const name = e.target.value;
    setForm((prev) => ({
      ...prev,
      name,
      // Auto-derive preset from name for known values.
      preset: derivePreset(name),
    }));
  }

  // WHY React.SyntheticEvent (not React.FormEvent): FormEvent is deprecated in
  // @types/react 19+; SyntheticEvent is the preferred base type for form events.
  function handleSave(e: React.SyntheticEvent) {
    e.preventDefault();
    setSaveError(null);
    saveMutation.mutate(form);
  }

  // ── Early exit when closed ─────────────────────────────────────────────────
  if (!open) return null;

  // ── Derived values for rendering ───────────────────────────────────────────
  const modelPlaceholder = form.name in KNOWN_PRESETS
    ? `Default: ${KNOWN_PRESETS[form.name]!.defaultModel}`
    : "Model (optional)";

  const isPending = saveMutation.isPending;

  // ── Render ─────────────────────────────────────────────────────────────────
  return (
    // WHY outer div is not aria-hidden: it is the backdrop but the accessible
    // dialog lives inside it. aria-hidden on the backdrop would hide the dialog
    // from the accessibility tree. The outer div is inert from the AT perspective
    // only because role="dialog" + aria-modal="true" on the inner panel causes
    // assistive technology to treat the rest of the page as aria-hidden by spec.
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/50"
      onClick={handleBackdropClick}
    >
      {/* Dialog panel — stop backdrop click propagation from inside */}
      <div
        role="dialog"
        aria-modal="true"
        aria-label="Transcription settings"
        className="relative w-full max-w-lg rounded-xl bg-popover border border-border shadow-lg p-6 flex flex-col gap-4"
        onClick={(e) => { e.stopPropagation(); }}
        onKeyDown={handleKeyDown}
        // WHY tabIndex: div needs focusability so keyDown fires on ESC without
        // needing a focusable child to bubble from.
        tabIndex={-1}
      >
        {/* Header */}
        <h2 className="text-lg font-semibold text-foreground">
          Transcription Settings
        </h2>

        {/* Existing providers list */}
        {providersQuery.data && providersQuery.data.providers.length > 0 && (
          <section className="flex flex-col gap-1">
            <p className="text-xs font-medium text-muted-foreground uppercase tracking-wide">
              Configured providers
            </p>
            <ul className="flex flex-col gap-1">
              {providersQuery.data.providers.map((p) => (
                <li key={p.name}>
                  <button
                    type="button"
                    className="w-full text-left px-3 py-2 rounded-md text-sm hover:bg-muted transition-colors duration-micro"
                    onClick={() => {
                      // Populate form for editing the selected provider.
                      setForm((prev) => ({
                        ...prev,
                        name: p.name,
                        preset: derivePreset(p.name),
                        model: p.model ?? "",
                        // Base URL / auth scheme not available from list payload;
                        // user can fill if needed. WHY: ProviderListEntry omits these
                        // fields (they are in the full config). The user can open the
                        // full config via getTranscriptionConfig in a future phase.
                        apiKey: "",
                        setAsActive: providersQuery.data.active === p.name,
                      }));
                      setSaveError(null);
                    }}
                  >
                    <span className="font-medium">{p.name}</span>
                    <span className="ml-2 text-muted-foreground">
                      {p.preset}
                      {p.model ? ` / ${p.model}` : ""}
                      {p.has_key ? " (key set)" : ""}
                      {providersQuery.data.active === p.name ? " (active)" : ""}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
            <hr className="border-border my-1" />
          </section>
        )}

        {/* Error banner */}
        {saveError !== null && (
          <div
            role="alert"
            className="rounded-md bg-destructive/10 border border-destructive/30 px-4 py-3 text-sm text-destructive"
          >
            {saveError}
          </div>
        )}

        {/* Form */}
        <form onSubmit={handleSave} className="flex flex-col gap-3" noValidate>
          {/* Provider name */}
          <div className="flex flex-col gap-1">
            <label
              htmlFor="ts-name"
              className="text-sm font-medium text-foreground"
            >
              Provider name
            </label>
            <input
              id="ts-name"
              type="text"
              value={form.name}
              onChange={handleNameChange}
              placeholder="e.g. groq"
              className="rounded-md border border-border bg-background px-3 py-2 text-sm text-foreground placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            />
          </div>

          {/* Preset dropdown */}
          <div className="flex flex-col gap-1">
            <label
              htmlFor="ts-preset"
              className="text-sm font-medium text-foreground"
            >
              Preset
            </label>
            <select
              id="ts-preset"
              value={form.preset}
              onChange={handlePresetChange}
              className="rounded-md border border-border bg-background px-3 py-2 text-sm text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <option value="groq">Groq</option>
              <option value="openai">OpenAI</option>
              <option value="custom">Custom</option>
            </select>
          </div>

          {/* Model (optional) */}
          <div className="flex flex-col gap-1">
            <label
              htmlFor="ts-model"
              className="text-sm font-medium text-foreground"
            >
              Model
              <span className="ml-1 text-xs text-muted-foreground">(optional)</span>
            </label>
            <input
              id="ts-model"
              type="text"
              value={form.model}
              onChange={(e) => { setForm((prev) => ({ ...prev, model: e.target.value })); }}
              placeholder={modelPlaceholder}
              className="rounded-md border border-border bg-background px-3 py-2 text-sm text-foreground placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            />
          </div>

          {/* Custom-preset-only fields */}
          {form.preset === "custom" && (
            <>
              <div className="flex flex-col gap-1">
                <label
                  htmlFor="ts-base-url"
                  className="text-sm font-medium text-foreground"
                >
                  Base URL
                </label>
                <input
                  id="ts-base-url"
                  type="url"
                  value={form.baseUrl}
                  onChange={(e) => { setForm((prev) => ({ ...prev, baseUrl: e.target.value })); }}
                  placeholder="https://api.example.com/v1"
                  className="rounded-md border border-border bg-background px-3 py-2 text-sm text-foreground placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                />
              </div>

              <div className="flex flex-col gap-1">
                <label
                  htmlFor="ts-auth-scheme"
                  className="text-sm font-medium text-foreground"
                >
                  Auth scheme
                </label>
                <select
                  id="ts-auth-scheme"
                  value={form.authScheme}
                  onChange={(e) => { setForm((prev) => ({ ...prev, authScheme: e.target.value })); }}
                  className="rounded-md border border-border bg-background px-3 py-2 text-sm text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                >
                  <option value="Bearer">Bearer</option>
                  <option value="X-API-Key">X-API-Key</option>
                  <option value="None">None</option>
                </select>
              </div>
            </>
          )}

          {/* API key — write-only password input */}
          <div className="flex flex-col gap-1">
            <label
              htmlFor="ts-api-key"
              className="text-sm font-medium text-foreground"
            >
              API key
              <span className="ml-1 text-xs text-muted-foreground">
                (leave blank to keep existing)
              </span>
            </label>
            {/* WHY type="password": key must never be shown in plaintext.
                WHY autoComplete="new-password": prevents browser from auto-filling
                existing saved passwords into this field. */}
            <input
              id="ts-api-key"
              type="password"
              value={form.apiKey}
              onChange={(e) => { setForm((prev) => ({ ...prev, apiKey: e.target.value })); }}
              placeholder="••••••••"
              autoComplete="new-password"
              className="rounded-md border border-border bg-background px-3 py-2 text-sm text-foreground placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            />
          </div>

          {/* Set as active checkbox */}
          <label className="flex items-center gap-2 text-sm text-foreground cursor-pointer">
            <input
              type="checkbox"
              checked={form.setAsActive}
              onChange={(e) => { setForm((prev) => ({ ...prev, setAsActive: e.target.checked })); }}
              className="rounded border-border text-primary focus-visible:ring-2 focus-visible:ring-ring"
            />
            Set as active provider
          </label>

          {/* Actions */}
          <div className="flex justify-end gap-2 pt-2">
            <button
              type="button"
              onClick={onClose}
              disabled={isPending}
              className="inline-flex items-center justify-center rounded-full px-5 py-2 text-sm font-medium bg-muted text-foreground hover:bg-muted/70 transition-colors duration-micro focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40 disabled:pointer-events-none"
            >
              Cancel
            </button>
            {/* WHY disable Save when name empty or config not loaded: an
                empty trimmed name would write providers[""] (corrupt config
                row); a missing config snapshot would wipe other providers
                on the spread. Both are data-integrity hazards, not just
                UX nits. */}
            <button
              type="submit"
              disabled={
                isPending ||
                form.name.trim() === "" ||
                !configQuery.isSuccess
              }
              title={
                form.name.trim() === ""
                  ? "Provider name required"
                  : !configQuery.isSuccess
                    ? "Loading existing config…"
                    : undefined
              }
              className="inline-flex items-center justify-center gap-2 rounded-full px-5 py-2 text-sm font-medium bg-primary text-primary-foreground hover:bg-primary/90 transition-colors duration-micro focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-40 disabled:pointer-events-none"
            >
              {isPending ? "Saving…" : "Save"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
