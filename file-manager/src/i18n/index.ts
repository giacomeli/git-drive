// The app's languages: English (the source, every key's reference text) and
// Brazilian Portuguese, chosen in Settings or taken from the system's own
// preferences. Messages are flat keys ("area.thing") in one JSON file per
// area and language; a message is a text with {named} values, or a plural
// ({ one, other }, picked by Intl.PluralRules with the "count" value).
// Numbers, sizes and dates follow the language too.
import { createContext, useContext } from "react";
import en from "./locales/en";
import ptBR from "./locales/pt-BR";

import { fill, formatNumber, type Language, type LanguageChoice, type Message, type Params } from "./core";

export { fromSystem, LANGUAGES, resolve } from "./core";
export type { Language, LanguageChoice, Message, Params } from "./core";

export type Key = keyof typeof en;
/** Every language has every key (the compiler checks the Portuguese one against the English). */
export type Catalog = Record<Key, Message>;

const CATALOGS: Record<Language, Catalog> = { en: en as Catalog, "pt-BR": ptBR };

let current: Language = "en";

/** The language messages and formats use now (set by the provider). */
export function setLanguage(l: Language) {
  current = l;
  document.documentElement.lang = l;
}

export const language = () => current;

/** The message `key` in the current language, with its {values} filled; numbers formatted for the language. */
export function t(key: Key, params?: Params): string {
  return fill(CATALOGS[current][key] ?? CATALOGS.en[key] ?? key, current, params);
}

/** A number as the language writes it (1.234,5 in Portuguese). */
export function num(n: number, digits?: number): string {
  return formatNumber(current, n, digits);
}

/** A date and time (seconds since 1970) as the language writes it. */
export function dateTime(secs: number, opts: Intl.DateTimeFormatOptions = { dateStyle: "medium", timeStyle: "short" }): string {
  return new Date(secs * 1000).toLocaleString(current, opts);
}

/** The language in use and how to change it; a component that reads it re-renders when it changes. */
export interface I18n {
  language: Language;
  choice: LanguageChoice;
  setChoice: (c: LanguageChoice) => void;
}

export const I18nContext = createContext<I18n>({ language: "en", choice: "system", setChoice: () => {} });
export const useI18n = () => useContext(I18nContext);
