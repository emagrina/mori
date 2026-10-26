#!/usr/bin/env node
// Checks Mori's translation catalogs against the strings in the code.
//
//   node scripts/i18n-check.mjs              # summary per language; exit 1 if anything is missing
//   node scripts/i18n-check.mjs --keys DIR   # write ui.json / core.json (the English source keys) to DIR
//   node scripts/i18n-check.mjs --lint       # list literal English that is not wrapped in t()/tr!()
//
// UI strings: t("…"), tx("…"), msg("…"), tn(n, "one", "other") in src/.
// Rust strings: tr!("…"), trn!(n, "one", "other") in src-tauri/src/.
// Catalogs: src/locales/<code>.json and src-tauri/locales/<code>.json.
import { readdirSync, readFileSync, statSync, writeFileSync, mkdirSync } from "node:fs";
import { join, relative } from "node:path";

const repo = new URL("..", import.meta.url).pathname;
const args = process.argv.slice(2);

function files(dir, exts) {
  const out = [];
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) {
      if (!["__tests__", "locales", "node_modules"].includes(name)) out.push(...files(p, exts));
    } else if (exts.some((e) => name.endsWith(e))) out.push(p);
  }
  return out;
}

const STR = String.raw`"((?:[^"\\]|\\[\s\S])*)"`;

function unescapeJs(s) {
  return s.replace(/\\(u\{[0-9a-fA-F]+\}|u[0-9a-fA-F]{4}|x[0-9a-fA-F]{2}|[\s\S])/g, (_, e) => {
    if (e[0] === "u") return String.fromCodePoint(parseInt(e.replace(/[u{}]/g, ""), 16));
    if (e[0] === "x") return String.fromCharCode(parseInt(e.slice(1), 16));
    return { n: "\n", t: "\t", r: "\r", 0: "\0" }[e] ?? e;
  });
}

function unescapeRust(s) {
  return s
    .replace(/\\\n\s*/g, "") // line continuation
    .replace(/\\(u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|[\s\S])/g, (_, e) => {
      if (e[0] === "u") return String.fromCodePoint(parseInt(e.slice(2, -1), 16));
      if (e[0] === "x") return String.fromCharCode(parseInt(e.slice(1), 16));
      return { n: "\n", t: "\t", r: "\r", 0: "\0" }[e] ?? e;
    });
}

/** Source keys: Map<key, { plural?: one, files: Set }> */
function collect(dir, exts, single, plural, unescape) {
  const keys = new Map();
  const add = (k, file, one) => {
    const e = keys.get(k) ?? { files: new Set() };
    if (one !== undefined) e.one = one;
    e.files.add(relative(repo, file));
    keys.set(k, e);
  };
  for (const f of files(dir, exts)) {
    const src = readFileSync(f, "utf8");
    for (const m of src.matchAll(new RegExp(String.raw`${single}\(\s*${STR}`, "g"))) add(unescape(m[1]), f);
    for (const m of src.matchAll(new RegExp(String.raw`${plural}\([^";]*?,\s*${STR}\s*,\s*${STR}`, "g"))) add(unescape(m[2]), f, unescape(m[1]));
  }
  return keys;
}

const ui = collect(join(repo, "src"), [".ts", ".tsx"], String.raw`(?<![\w.])(?:t|tx|msg)`, String.raw`(?<![\w.])tn`, unescapeJs);
const core = collect(join(repo, "src-tauri/src"), [".rs"], String.raw`\btr!`, String.raw`\btrn!`, unescapeRust);

const placeholders = (s) => [...s.matchAll(/\{(\w+)\}/g)].map((m) => m[1]).sort().join(",");

if (args[0] === "--keys") {
  const dir = args[1] ?? ".";
  mkdirSync(dir, { recursive: true });
  const dump = (keys) => Object.fromEntries([...keys].sort(([a], [b]) => a.localeCompare(b)).map(([k, e]) => [k, e.one !== undefined ? { one: e.one, other: k } : k]));
  writeFileSync(join(dir, "ui.json"), JSON.stringify(dump(ui), null, 2) + "\n");
  writeFileSync(join(dir, "core.json"), JSON.stringify(dump(core), null, 2) + "\n");
  console.log(`${ui.size} UI keys, ${core.size} Rust keys → ${dir}`);
  process.exit(0);
}

if (args[0] === "--lint") {
  // Heuristic: JSX text with letters, and literal user-facing attributes.
  let n = 0;
  for (const f of files(join(repo, "src"), [".tsx"])) {
    const lines = readFileSync(f, "utf8").split("\n");
    lines.forEach((line, i) => {
      const hits = [
        ...line.matchAll(/>\s*([A-Za-z][^<>{}]*[A-Za-z.…?!])\s*(?:<|\{|$)/g),
        ...line.matchAll(/\b(?:title|aria-label|placeholder|alt|label)="([^"]*[A-Za-z]{2}[^"]*)"/g),
      ];
      for (const h of hits) {
        if (/=>|&&|\|\||===/.test(h[1])) continue;
        console.log(`${relative(repo, f)}:${i + 1}: ${h[1].trim()}`);
        n++;
      }
    });
  }
  console.log(`${n} possible untranslated strings`);
  process.exit(0);
}

let bad = false;
for (const [label, keys, dir] of [
  ["UI", ui, "src/locales"],
  ["Rust", core, "src-tauri/locales"],
]) {
  console.log(`${label}: ${keys.size} strings`);
  for (const name of readdirSync(join(repo, dir)).filter((n) => n.endsWith(".json")).sort()) {
    const cat = JSON.parse(readFileSync(join(repo, dir, name), "utf8"));
    const missing = [...keys.keys()].filter((k) => !cat[k]);
    const unused = Object.keys(cat).filter((k) => !keys.has(k));
    const wrong = [];
    for (const [k, e] of keys) {
      const v = cat[k];
      if (!v) continue;
      const forms = typeof v === "string" ? [v] : Object.values(v);
      const want = placeholders(k);
      for (const s of forms) {
        // A plural form may drop {n} (e.g. "one file"); otherwise placeholders must match.
        const got = placeholders(s);
        if (got !== want && !(e.one !== undefined && got === want.replace(/(^|,)n(,|$)/, "$1$2").replace(/^,|,$/g, ""))) wrong.push(`${k} → ${s}`);
      }
      if (typeof v === "object" && !v.other) wrong.push(`${k} → (no "other" form)`);
    }
    const status = missing.length || wrong.length ? "✗" : "✓";
    if (status === "✗") bad = true;
    console.log(`  ${status} ${name.padEnd(13)} missing ${String(missing.length).padStart(4)}  unused ${String(unused.length).padStart(4)}  placeholder errors ${wrong.length}`);
    if (args.includes("-v")) {
      for (const k of missing) console.log(`      missing: ${k}`);
      for (const k of unused) console.log(`      unused: ${k}`);
      for (const k of wrong) console.log(`      placeholders: ${k}`);
    }
  }
}
process.exit(bad ? 1 : 0);
