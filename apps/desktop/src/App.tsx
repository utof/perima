/**
 * Root-route component. Mounts inside <RouterProvider> via router.tsx.
 *
 * WHY this is "App" not "RootRoute": git-history continuity. The file
 * is "the root component" before and after Batch H; tests + imports
 * preserve their existing paths. Naming clarification: post-Batch-H,
 * "App" = root-route component, NOT application root. Application
 * root = main.tsx + RouterProvider + QueryClientProvider.
 *
 * Owns: layout chrome (header/footer) + useDomainEvents() mount.
 * Does NOT own: file/tag/search composition (lives in routes/index.tsx)
 * or any local useState/useEffect — server state via TanStack Query,
 * UI state via useUiStore.
 */
import type { ReactNode } from "react";
import { useDomainEvents } from "./hooks/useDomainEvents";
import SearchBar from "./components/SearchBar";
import ScanButton from "./components/ScanButton";
import StatusBar from "./components/StatusBar";
import ViewModeToggle from "./components/ViewModeToggle";
import NotificationStack from "./components/NotificationStack";

export default function App({ children }: { children?: ReactNode }) {
  useDomainEvents();
  return (
    <div className="bg-gray-900 text-gray-100 min-h-screen flex flex-col">
      <header className="flex items-center justify-between px-6 py-4 bg-gray-800 border-b border-gray-700">
        <h1 className="text-xl font-bold tracking-wide">perima</h1>
        <div className="flex items-center gap-3">
          <SearchBar />
          <ViewModeToggle />
          <ScanButton />
        </div>
      </header>
      <NotificationStack />
      {children}
      <footer>
        <StatusBar />
      </footer>
    </div>
  );
}
