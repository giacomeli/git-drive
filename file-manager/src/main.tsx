import "@fontsource/poppins/400.css";
import "@fontsource/poppins/500.css";
import "@fontsource/poppins/600.css";
import "@fontsource/jetbrains-mono/400.css";
import "@fontsource/jetbrains-mono/600.css";
import "./ui/styles.css";
import "./files.css";

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { api, errorText } from "./lib/api";
import { isMac } from "./components/Chrome";
import { applyAccent } from "./ui/accent";
import { storedAppearance, themeOf } from "./lib/theme";
import { ToastProvider } from "./ui/ui";
import { I18nProvider } from "./i18n/Provider";

const root = createRoot(document.getElementById("root")!);

// The theme, too, before the first paint (the window's glass follows from the app).
document.documentElement.dataset.theme = themeOf(storedAppearance());

// The Control Center's accent is applied before the first paint, so both
// apps share one look and the window never flashes in the default colors.
api
  .boot()
  .then((boot) => {
    applyAccent(boot.accent);
    document.body.classList.toggle("platform-linux", !isMac);
    document.body.classList.toggle("glass-native", boot.glass !== "none");
    root.render(
      <StrictMode>
        <I18nProvider systemLocales={boot.locales}>
          <ToastProvider>
            <App boot={boot} />
          </ToastProvider>
        </I18nProvider>
      </StrictMode>,
    );
  })
  .catch((e) => {
    root.render(<p style={{ padding: 24, color: "#ff9db0", fontFamily: "sans-serif" }}>Could not start: {errorText(e)}</p>);
  });
