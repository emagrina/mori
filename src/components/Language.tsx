import { useEffect, useMemo, useState } from "react";
import { api } from "../api";
import { currentLocale, LOCALES, systemLocale, t } from "../i18n";
import { Icon } from "./Icon";
import { ModalFrame } from "./Modal";

/**
 * Interface language. The choice is saved with Mori's settings and applied by
 * reloading the interface, which keeps the current folder and view.
 */
export function LanguageDialog({ onClose }: { onClose: () => void }) {
  // undefined until the saved choice is known; null follows the system.
  const [saved, setSaved] = useState<string | null | undefined>(undefined);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Each language in the current one ("Japanese"), next to its own name ("日本語").
  const names = useMemo(() => {
    try {
      return new Intl.DisplayNames([currentLocale()], { type: "language" });
    } catch {
      return null;
    }
  }, []);
  useEffect(() => {
    api.savedLanguage().then(setSaved, () => setSaved(null));
  }, []);
  const system = LOCALES.find((l) => l.code === systemLocale())!;
  const choose = async (pref: string | null) => {
    if (pref === saved) return onClose();
    setBusy(true);
    setError(null);
    try {
      await api.setLanguage(pref ?? system.code, { pref });
      location.reload();
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };
  const row = (pref: string | null, label: string, detail: string | undefined) => (
    <button key={pref ?? "system"} className={`tag-option lang-option ${saved === pref ? "all" : ""}`} disabled={busy || saved === undefined} onClick={() => choose(pref)} role="menuitemradio" aria-checked={saved === pref}>
      <span className="truncate" lang={pref ?? undefined}>
        <bdi>{label}</bdi>
      </span>
      {detail && <span className="muted truncate lang-detail"><bdi>{detail}</bdi></span>}
      {saved === pref && <Icon name="check" size={14} className="check" />}
    </button>
  );
  return (
    <ModalFrame onCancel={onClose}>
      <div className="dialog-icon">
        <Icon name="globe" size={20} />
      </div>
      <h2>{t("Language")}</h2>
      <div className="tag-pick lang-pick" role="menu">
        {row(null, t("System Language"), system.name)}
        <div className="sep" />
        {LOCALES.map((l) => {
          const local = names?.of(l.code);
          return row(l.code, l.name, local && local.toLowerCase() !== l.name.toLowerCase() ? local : undefined);
        })}
      </div>
      {error && <p className="field-error">{error}</p>}
      <div className="dialog-actions">
        <button className="btn" onClick={onClose}>
          {t("Done")}
        </button>
      </div>
    </ModalFrame>
  );
}
