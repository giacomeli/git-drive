import { useEffect, useMemo, type ReactNode } from "react";
import { api } from "../lib/api";
import { useStored } from "../lib/stored";
import { I18nContext, language as currentLanguage, resolve, setLanguage, type LanguageChoice } from "./index";

/**
 * The language of the window and of the backend's messages: the one chosen
 * in Settings, or the system's. Set before the children render, so every
 * message of this render is in it; the backend is told on each change.
 */
export function I18nProvider({ systemLocales, children }: { systemLocales: readonly string[]; children: ReactNode }) {
  const [choice, setChoice] = useStored<LanguageChoice>("language", "system");
  const language = resolve(choice, systemLocales);
  if (language !== currentLanguage() || document.documentElement.lang !== language) setLanguage(language);
  useEffect(() => {
    api.setLanguage(language).catch(() => {});
  }, [language]);
  const value = useMemo(() => ({ language, choice, setChoice }), [language, choice, setChoice]);
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}
