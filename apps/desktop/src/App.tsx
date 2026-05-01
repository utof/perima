/**
 * Root-route shell. Mounts inside <RouterProvider> via router.tsx, which
 * wraps us as `<App><Outlet /></App>` — i.e. the matched child route is
 * passed via `children`. Preserve this contract; do NOT switch to
 * importing <Outlet /> here.
 *
 * Owns: layout chrome (header / main slot / footer) + useDomainEvents()
 * mount + NotificationStack mount + ThemeToggle slot.
 *
 * Does NOT own: ThemeProvider (mounted in main.tsx, wrapping us);
 * file/tag/search composition (lives in routes/index.tsx).
 *
 * WHY direct utilities (font-display text-2xl font-semibold tracking-tight)
 * for the wordmark instead of .h1 / .display-xl: the wordmark is small
 * (~1.5rem), not hero-sized. The semantic typography classes bake in
 * size + opsz + line-height for editorial display use; mixing them with
 * utility-overrides relies on Tailwind layer order and is fragile.
 */
import type { ReactNode } from "react";
import { useDomainEvents } from "./hooks/useDomainEvents";
import SearchBar from "./components/SearchBar";
import ScanButton from "./components/ScanButton";
import StatusBar from "./components/StatusBar";
import ViewModeToggle from "./components/ViewModeToggle";
import NotificationStack from "./components/NotificationStack";
import ThemeToggle from "./components/ThemeToggle";

export default function App({ children }: { children?: ReactNode }) {
  useDomainEvents();
  return (
    <div className="min-h-screen flex flex-col bg-background text-foreground font-sans">
      <header className="flex items-center justify-between px-6 py-4 bg-card border-b border-border">
        <h1 className="font-display text-2xl font-semibold tracking-tight">perima</h1>
        <div className="flex items-center gap-3">
          <SearchBar />
          <ViewModeToggle />
          <ScanButton />
          <ThemeToggle />
        </div>
      </header>
      <NotificationStack />
      <main className="flex-1 min-h-0">{children}</main>
      <footer>
        <StatusBar />
      </footer>
    </div>
  );
}
