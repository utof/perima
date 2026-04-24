/**
 * TanStack Router root. Code-based routes (NOT file-based) — single
 * route in v0.6.x doesn't justify codegen ceremony. Re-evaluate at
 * Phase 6 (route count \> 3).
 *
 * WHY createHashHistory: Tauri serves dist/index.html static; HTML5
 * history mode would need rewrite rules for direct navigation.
 *
 * WHY App as root component: App.tsx is the root-route shell
 * (provider stack + layout chrome + useDomainEvents mount). It
 * accepts children and renders Outlet via children.
 */
import {
  createRouter,
  createRootRoute,
  createRoute,
  createHashHistory,
  Outlet,
} from "@tanstack/react-router";
import App from "./App";
import IndexRoute from "./routes/index";

const rootRoute = createRootRoute({
  component: () => <App><Outlet /></App>,
});

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: IndexRoute,
});

const routeTree = rootRoute.addChildren([indexRoute]);

export const router = createRouter({
  routeTree,
  history: createHashHistory(),
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
