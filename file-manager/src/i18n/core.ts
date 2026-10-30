// The pure part of the app's languages (no catalogs, no React): which
// language the system's preferences lead to, and how a message is filled.

export type Message = string | { one: string; other: string };
export type Params = Record<string, string | number>;

export const LANGUAGES = [
  { id: "en", name: "English" },
  { id: "pt-BR", name: "Português (Brasil)" },
] as const;
export type Language = (typeof LANGUAGES)[number]["id"];
/** What Settings keeps: a language, or the system's. */
export type LanguageChoice = "system" | Language;

/** The language of the system's preferences ("pt-BR", "pt_PT", "en-GB" ...), or English when none is here. */
export function fromSystem(locales: readonly string[]): Language {
  for (const raw of locales) {
    const l = raw.toLowerCase().replace("_", "-");
    if (l.startsWith("pt")) return "pt-BR";
    if (l.startsWith("en")) return "en";
  }
  return "en";
}

export function resolve(choice: LanguageChoice, systemLocales: readonly string[]): Language {
  return choice === "system" ? fromSystem(systemLocales) : choice;
}

/** A number as `lang` writes it. */
export function formatNumber(lang: Language, n: number, digits?: number): string {
  return new Intl.NumberFormat(lang, digits == null ? undefined : { minimumFractionDigits: digits, maximumFractionDigits: digits }).format(n);
}

/** `msg` in `lang`: the plural form for `count`, then each {value} filled (numbers formatted for the language). */
export function fill(msg: Message, lang: Language, params?: Params): string {
  let text: string;
  if (typeof msg === "string") text = msg;
  else text = new Intl.PluralRules(lang).select(Number(params?.count ?? 0)) === "one" ? msg.one : msg.other;
  if (!params) return text;
  return text.replace(/\{(\w+)\}/g, (whole, name: string) => {
    const v = params[name];
    if (v == null) return whole;
    return typeof v === "number" ? formatNumber(lang, v) : v;
  });
}
