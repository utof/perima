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
import DedupRoute from "./routes/dedup";

const rootRoute = createRootRoute({
  component: () => <App><Outlet /></App>,
});

const indexRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/",
  component: IndexRoute,
});

// WHY placeholder: the /dedup route body lands in Task 13. The stub here
// registers the path so `<Link to="/dedup">` in CollisionPill type-checks
// (TanStack Router validates `to` props against the registered route tree).
const dedupRoute = createRoute({
  getParentRoute: () => rootRoute,
  path: "/dedup",
  component: DedupRoute,
});

const routeTree = rootRoute.addChildren([indexRoute, dedupRoute]);

export const router = createRouter({
  routeTree,
  history: createHashHistory(),
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
