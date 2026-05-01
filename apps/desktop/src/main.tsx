import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { queryClient } from "./lib/queryClient";
import { router } from "./router";
import { ThemeProvider } from "./lib/theme-provider";

// WHY this import order: fonts.ts emits @font-face rules (side-effect
// import) that tokens.css's --font-display / --font-sans / --font-mono
// reference. tokens.css MUST come AFTER fonts.ts so font-face rules
// are registered first. Reversing the order falls back to next font
// in stack on first paint.
import "./styles/fonts";
import "./styles/tokens.css";

const rootEl = document.getElementById("root");
if (!rootEl) throw new Error("Root element not found");

createRoot(rootEl).render(
  <StrictMode>
    <ThemeProvider>
      <QueryClientProvider client={queryClient}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </ThemeProvider>
  </StrictMode>,
);
