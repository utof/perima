/**
 * Theme provider for dark / light / system mode.
 *
 * WHY raw localStorage (not Zustand persist mw): CLAUDE.md "Frontend
 * state (Batch H)" forbids `persist` middleware on the Zustand UI store
 * because that store holds ephemeral app state (search query, scan
 * status, notifications) that should NOT survive restarts. Theme is a
 * user setting, different category — persisting it is correct.
 * Implementing it via raw localStorage inside this provider keeps the
 * existing rule intact.
 *
 * WHY effectiveTheme exposed even though no consumer reads it this slice:
 * future component-level reads (e.g. Phosphor weight switching on
 * resolved theme) without re-deriving from theme + matchMedia. Marked
 * here so the implementer doesn't drop it as dead code.
 *
 * WHY systemTheme as separate state (not derived in render): we need to
 * subscribe to matchMedia "change" events and re-render when the OS pref
 * changes. Derived computation alone does not re-render on OS change.
 * effectiveTheme is then a pure derivation: theme === "system" ?
 * systemTheme : theme. No setState inside an effect body.
 */
import {
  createContext,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";

export type Theme = "dark" | "light" | "system";
export type EffectiveTheme = "dark" | "light";

interface ThemeContextValue {
  theme: Theme;
  effectiveTheme: EffectiveTheme;
  setTheme: (theme: Theme) => void;
}

const STORAGE_KEY = "perima-theme";

function readStoredTheme(): Theme {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (raw === "dark" || raw === "light" || raw === "system") return raw;
  } catch {
    // localStorage unavailable (private mode, SSR) — fall through.
  }
  return "system";
}

function getSystemTheme(): EffectiveTheme {
  if (typeof window === "undefined") return "dark";
  return window.matchMedia("(prefers-color-scheme: light)").matches
    ? "light"
    : "dark";
}

function applyEffectiveTheme(effective: EffectiveTheme): void {
  if (typeof document === "undefined") return;
  if (effective === "light") {
    document.documentElement.setAttribute("data-theme", "light");
  } else {
    document.documentElement.removeAttribute("data-theme");
  }
}

const ThemeContext = createContext<ThemeContextValue | null>(null);

/** Provides dark/light/system theme switching with localStorage persistence. */
export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setThemeState] = useState<Theme>(() => readStoredTheme());
  // WHY separate systemTheme state: subscribing to matchMedia "change"
  // events requires a stateful update path. effectiveTheme is then a
  // pure render-time derivation (no setState in useEffect body).
  const [systemTheme, setSystemTheme] = useState<EffectiveTheme>(() =>
    getSystemTheme(),
  );

  // Listen to OS-level pref changes; update systemTheme via event callback
  // (not synchronously inside the effect body — avoids react-hooks/set-state-in-effect).
  useEffect(() => {
    if (typeof window === "undefined") return;
    const mql = window.matchMedia("(prefers-color-scheme: light)");
    const handler = (e: MediaQueryListEvent) => {
      setSystemTheme(e.matches ? "light" : "dark");
    };
    mql.addEventListener("change", handler);
    return () => { mql.removeEventListener("change", handler); };
  }, []);

  // Derive effectiveTheme at render time — no effect needed.
  const effectiveTheme: EffectiveTheme =
    theme === "system" ? systemTheme : theme;

  // Sync data-theme attribute to the DOM whenever effectiveTheme changes.
  useEffect(() => {
    applyEffectiveTheme(effectiveTheme);
  }, [effectiveTheme]);

  const setTheme = (next: Theme) => {
    try {
      localStorage.setItem(STORAGE_KEY, next);
    } catch {
      // localStorage unavailable — theme still updates in-memory.
    }
    setThemeState(next);
  };

  return (
    <ThemeContext.Provider value={{ theme, effectiveTheme, setTheme }}>
      {children}
    </ThemeContext.Provider>
  );
}

/** Returns the current theme context. Must be used within {@link ThemeProvider}. */
export function useTheme(): ThemeContextValue {
  const ctx = useContext(ThemeContext);
  if (ctx === null) {
    throw new Error("useTheme must be used within <ThemeProvider>");
  }
  return ctx;
}
