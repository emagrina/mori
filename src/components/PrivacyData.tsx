import { useEffect, useState } from "react";
import { api, formatSize, type DataCategory, type LocalData } from "../api";
import { Icon } from "./Icon";
import { ModalFrame } from "./Modal";
import { t, tx } from "../i18n";

const DEFAULT: DataCategory[] = ["cache", "analysis", "history"];

/**
 * Everything Mori keeps on this computer, with sizes, and granular
 * clearing. Never touches the user's files.
 */
export function PrivacyData({ onClose, onCleared }: { onClose: () => void; onCleared: (msg: string, reset: boolean) => void }) {
  const [data, setData] = useState<LocalData | null>(null);
  const [chosen, setChosen] = useState<Set<DataCategory>>(() => new Set(DEFAULT));
  const [busy, setBusy] = useState(false);
  const [reset, setReset] = useState(false);
  const [typed, setTyped] = useState("");
  const [error, setError] = useState<string | null>(null);
  const load = () => api.localData().then(setData);
  useEffect(() => {
    load();
  }, []);
  const toggle = (c: DataCategory) =>
    setChosen((s) => {
      const n = new Set(s);
      if (n.has(c)) n.delete(c);
      else n.add(c);
      return n;
    });
  const clear = async () => {
    setBusy(true);
    try {
      const freed = await api.clearMoriData([...chosen]);
      onCleared(t("Cleared {size} of Mori data. Your files were not touched.", { size: formatSize(freed) }), false);
      load();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const doReset = async () => {
    setBusy(true);
    try {
      const freed = await api.resetMori();
      onCleared(t("Mori was reset ({size} of its data removed). Your files were not touched.", { size: formatSize(freed) }), true);
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };
  if (reset) {
    return (
      <ModalFrame onCancel={() => !busy && setReset(false)}>
        <div className="dialog-icon danger">
          <Icon name="refresh" size={20} />
        </div>
        <h2>{t("Reset Mori?")}</h2>
        <p>
          {tx(
            "Removes {all} on this computer: indexes, thumbnails, fingerprints, analysis decisions, history, tags, favorites, private and protected folder rules, integrity snapshots, settings and window state. Mori returns to a fresh state.",
            { all: <strong>{t("all of Mori's own data")}</strong> },
          )}
        </p>
        <p className="dialog-note">
          {tx("{never} This only removes Mori's records about them.", {
            never: <strong>{t("Files on your drives are never deleted or changed.")}</strong>,
          })}
        </p>
        <label className="op-typed">
          {tx("Type {word} to confirm", { word: <strong>RESET</strong> })}
          <input className="text-input slim" value={typed} onChange={(e) => setTyped(e.target.value)} autoFocus spellCheck={false} />
        </label>
        {error && <p className="field-error">{error}</p>}
        <div className="dialog-actions">
          <button className="btn" onClick={() => setReset(false)} disabled={busy}>
            {t("Cancel")}
          </button>
          <button className="btn primary danger-fill" disabled={typed !== "RESET" || busy} onClick={doReset}>
            {t("Reset Mori")}
          </button>
        </div>
      </ModalFrame>
    );
  }
  const total = data?.groups.reduce((n, g) => n + g.bytes, 0) ?? 0;
  return (
    <ModalFrame onCancel={onClose} wide>
      <h2>{t("Privacy & Local Data")}</h2>
      <p className="muted small">
        {t(
          "Everything Mori keeps on this computer ({size}). Mori has no account, no cloud and no telemetry; nothing here leaves this computer.",
          { size: formatSize(total) },
        )}
      </p>
      {data && (
        <>
          <div className="data-list">
            {data.groups
              .filter((g) => g.category !== "settings")
              .map((g) => (
                <label key={g.category} className="data-row">
                  <input type="checkbox" checked={chosen.has(g.category)} onChange={() => toggle(g.category)} />
                  <span className="data-main">
                    <span className="data-title">
                      {g.title} <span className="muted">· {formatSize(g.bytes)}</span>
                    </span>
                    <span className="muted small">{g.what}</span>
                    <span className="muted small">{t("Can reveal: {what}", { what: g.sensitivity })}</span>
                  </span>
                </label>
              ))}
          </div>
          <p className="muted small">
            {data.session.temporary
              ? t(
                  "In memory right now (gone when Mori quits or this temporary session ends): {checksums} checksums, {history} undo entries, {analyses} analysis results.",
                  { checksums: data.session.checksums, history: data.session.history, analyses: data.session.analyses },
                )
              : t("In memory right now (gone when Mori quits): {checksums} checksums, {history} undo entries, {analyses} analysis results.", {
                  checksums: data.session.checksums,
                  history: data.session.history,
                  analyses: data.session.analyses,
                })}
          </p>
          <details className="verify-list">
            <summary>{t("Where it is stored")}</summary>
            <ul>
              <li className="mono">{data.dataDir}</li>
              <li className="mono">{data.cacheDir}</li>
              {data.groups.flatMap((g) => g.locations.map((l) => <li key={l.path} className="mono">{`${l.path} · ${formatSize(l.bytes)}`}</li>))}
            </ul>
          </details>
        </>
      )}
      {error && <p className="field-error">{error}</p>}
      <div className="dialog-actions">
        <button className="btn danger-text" onClick={() => setReset(true)} disabled={busy}>
          {t("Reset Mori…")}
        </button>
        <span className="spacer" />
        <button className="btn" onClick={onClose} disabled={busy}>
          {t("Done")}
        </button>
        <button className="btn primary" disabled={!chosen.size || busy} onClick={clear}>
          {busy ? t("Clearing…") : t("Clear Selected Data")}
        </button>
      </div>
    </ModalFrame>
  );
}
