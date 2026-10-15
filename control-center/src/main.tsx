import "@fontsource/poppins/400.css";
import "@fontsource/poppins/500.css";
import "@fontsource/poppins/600.css";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/600.css";
import "./styles.css";

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { applyAccent } from "./lib/accent";
import { api } from "./lib/api";
import { ToastProvider } from "./components/ui";

function render() {
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <ToastProvider>
        <App />
      </ToastProvider>
    </StrictMode>,
  );
}

// The saved accent is applied before the first paint, so the window never
// flashes in the default colors; a slow or failing read renders anyway.
Promise.race([api.uiPrefs(), new Promise<null>((resolve) => setTimeout(() => resolve(null), 400))])
  .then((prefs) => {
    if (prefs) applyAccent(prefs.accent);
  })
  .catch(() => {})
  .finally(render);
