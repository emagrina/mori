// @vitest-environment jsdom
import { execFileSync } from "node:child_process";
import { readdirSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { fmtNum, initI18n, LOCALES, systemLocale, t, tn } from "../i18n";

describe("i18n", () => {
  it("falls back to English and fills placeholders", async () => {
    await initI18n("en");
    expect(t("Move “{name}” to…", { name: "a.jpg" })).toBe("Move “a.jpg” to…");
    expect(tn(1, "{n} file", "{n} files")).toBe("1 file");
    expect(tn(1200, "{n} file", "{n} files")).toBe("1,200 files");
  });

  it("uses the language's plural categories and number format", async () => {
    await initI18n("ru");
    const forms = [1, 3, 5, 21].map((n) => tn(n, "{n} file", "{n} files"));
    expect(new Set(forms).size).toBeGreaterThan(1);
    expect(forms.every((f) => /\d/.test(f) && !f.includes("files"))).toBe(true);
    await initI18n("de");
    expect(fmtNum(1234.5)).toBe("1.234,5");
    expect(document.documentElement.lang).toBe("de");
    await initI18n("ar");
    expect(document.documentElement.dir).toBe("rtl");
    await initI18n("en");
  });

  it("matches system languages to a supported one", () => {
    expect(systemLocale(["es-MX"])).toBe("es");
    expect(systemLocale(["zh-TW"])).toBe("zh-Hant");
    expect(systemLocale(["zh-CN"])).toBe("zh-Hans");
    expect(systemLocale(["iw-IL"])).toBe("he");
    expect(systemLocale(["xx", "pt-BR"])).toBe("pt");
    expect(systemLocale(["xx"])).toBe("en");
  });

  it("has a catalog for every language and none extra", () => {
    const files = readdirSync(join(process.cwd(), "src/locales")).map((f) => f.replace(".json", ""));
    expect(files.sort()).toEqual(LOCALES.filter((l) => l.code !== "en").map((l) => l.code).sort());
  });

  it("every catalog is complete, with matching placeholders", () => {
    // Throws (exit code 1) when a string is missing or a placeholder differs.
    execFileSync("node", [join(process.cwd(), "scripts/i18n-check.mjs")], { stdio: "pipe" });
  });
});
