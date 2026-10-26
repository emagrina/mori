import { Fragment, createElement, type ReactNode } from "react";

/**
 * Interface languages. English is the source language: strings are written
 * in English in the code and are their own catalog keys (gettext style), so
 * a missing translation falls back to the English text.
 *
 *   t("Change Folder…")
 *   t("Move “{name}” to…", { name })
 *   tn(count, "{n} file", "{n} files")
 *   tx("Delete {name} permanently?", { name: <b>{entry.name}</b> })   // JSX values
 *   const LABELS = [msg("Photos")] … t(LABELS[0])                       // module-level tables
 *
 * Catalogs live in src/locales/<code>.json: "English": "translation", or for
 * tn() keys (the English plural form) an object of CLDR plural categories
 * ({ "one": …, "few": …, "many": …, "other": … }).
 * `npm run i18n:check` lists missing and unused keys.
 */

export interface Locale {
  code: string;
  /** Name in the language itself, as shown in the language picker. */
  name: string;
  dir: "ltr" | "rtl";
}

export const LOCALES: Locale[] = [
  { code: "en", name: "English", dir: "ltr" },
  { code: "es", name: "Español", dir: "ltr" },
  { code: "ca", name: "Català", dir: "ltr" },
  { code: "fr", name: "Français", dir: "ltr" },
  { code: "de", name: "Deutsch", dir: "ltr" },
  { code: "it", name: "Italiano", dir: "ltr" },
  { code: "pt", name: "Português", dir: "ltr" },
  { code: "nl", name: "Nederlands", dir: "ltr" },
  { code: "pl", name: "Polski", dir: "ltr" },
  { code: "uk", name: "Українська", dir: "ltr" },
  { code: "ru", name: "Русский", dir: "ltr" },
  { code: "tr", name: "Türkçe", dir: "ltr" },
  { code: "ar", name: "العربية", dir: "rtl" },
  { code: "he", name: "עברית", dir: "rtl" },
  { code: "hi", name: "हिन्दी", dir: "ltr" },
  { code: "bn", name: "বাংলা", dir: "ltr" },
  { code: "id", name: "Bahasa Indonesia", dir: "ltr" },
  { code: "vi", name: "Tiếng Việt", dir: "ltr" },
  { code: "th", name: "ไทย", dir: "ltr" },
  { code: "zh-Hans", name: "简体中文", dir: "ltr" },
  { code: "zh-Hant", name: "繁體中文", dir: "ltr" },
  { code: "ja", name: "日本語", dir: "ltr" },
  { code: "ko", name: "한국어", dir: "ltr" },
];

type Plural = Partial<Record<Intl.LDMLPluralRule, string>>;
type Catalog = Record<string, string | Plural>;

const catalogs = import.meta.glob<Catalog>("./locales/*.json", { import: "default" });

let lang = "en";
let catalog: Catalog = {};
let pluralRules = new Intl.PluralRules("en");
let numberFmt = new Intl.NumberFormat("en");

/** The active interface language. */
export const currentLocale = () => lang;
export const isRtl = () => LOCALES.find((l) => l.code === lang)?.dir === "rtl";

/** Best supported match for the system's preferred languages. */
export function systemLocale(preferred: readonly string[] = navigator.languages): string {
  for (const p of preferred) {
    const tag = p.toLowerCase();
    if (tag.startsWith("zh")) {
      // zh-TW, zh-HK, zh-MO and zh-Hant-* are Traditional; everything else Simplified.
      return /hant|-tw|-hk|-mo/.test(tag) ? "zh-Hant" : "zh-Hans";
    }
    const base = tag.split("-")[0];
    const hit = LOCALES.find((l) => l.code === (base === "iw" ? "he" : base === "in" ? "id" : base));
    if (hit) return hit.code;
  }
  return "en";
}

/** Loads the catalog for `code` (an unknown code means English). Call before the first render. */
export async function initI18n(code: string) {
  const loc = LOCALES.find((l) => l.code === code) ?? LOCALES[0];
  lang = loc.code;
  catalog = lang === "en" ? {} : ((await catalogs[`./locales/${lang}.json`]?.()) ?? {});
  pluralRules = new Intl.PluralRules(lang);
  numberFmt = new Intl.NumberFormat(lang);
  document.documentElement.lang = lang;
  document.documentElement.dir = loc.dir;
}

/** Marks an English string for translation without translating it (for module-level
 *  tables, which exist before the language is known); translate it with t() at render. */
export const msg = (text: string) => text;

/** A number in the interface language's notation. */
export const fmtNum = (n: number) => numberFmt.format(n);

const PLACEHOLDER = /\{(\w+)\}/g;

const value = (v: unknown) => (typeof v === "number" ? fmtNum(v) : String(v));

/** Translate `text` and fill its {placeholders}. Numbers are formatted for the language. */
export function t(text: string, params?: Record<string, string | number>): string {
  const s = catalog[text];
  const out = typeof s === "string" && s ? s : text;
  return params ? out.replace(PLACEHOLDER, (m, k: string) => (k in params ? value(params[k]) : m)) : out;
}

/** Plural-aware translation; {n} is the count. The catalog key is the English plural form. */
export function tn(n: number, one: string, other: string, params?: Record<string, string | number>): string {
  const s = catalog[other];
  let out: string | undefined;
  if (s && typeof s === "object") out = s[pluralRules.select(n)] || s.other;
  else if (typeof s === "string" && s) out = s;
  out ??= n === 1 ? one : other;
  return out.replace(PLACEHOLDER, (m, k: string) => (k === "n" ? fmtNum(n) : params && k in params ? value(params[k]) : m));
}

/** Like t(), but placeholder values may be React elements; returns a node. */
export function tx(text: string, params: Record<string, ReactNode>): ReactNode {
  const s = catalog[text];
  const out = typeof s === "string" && s ? s : text;
  return createElement(
    Fragment,
    null,
    ...out.split(PLACEHOLDER).map((part, i) => (i % 2 ? createElement(Fragment, { key: i }, part in params ? (typeof params[part] === "number" ? fmtNum(params[part] as number) : params[part]) : `{${part}}`) : part)),
  );
}
