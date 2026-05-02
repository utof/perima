// Fontsource side-effect imports — registers @font-face rules for the
// variable woff2 font files Vite bundles. Imported by main.tsx BEFORE
// tokens.css so the @font-face declarations are registered before
// tokens.css references them via --font-display / --font-sans / --font-mono.
import "@fontsource-variable/fraunces";
import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
