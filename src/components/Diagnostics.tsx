import { useEffect, useState } from "react";
import { api, type DiagCheck, type DiagStatus } from "../api";
import { ProgressBar } from "./Analyzer";
import { ModalFrame } from "./Modal";
import { msg, t } from "../i18n";

/** Set at build time from package.json (kept equal to Cargo.toml and tauri.conf.json by a test). */
declare const __MORI_VERSION__: string;

const MARK: Record<DiagStatus, string> = { pass: "✓", limited: "△", fail: "✕", info: "—" };
const STATUS_LABEL: Record<DiagStatus, string> = { pass: msg("pass"), limited: msg("limited"), fail: msg("fail"), info: msg("info") };
const SECTIONS: [DiagCheck["section"], string][] = [
  ["security", msg("Security")],
  ["media", msg("Media")],
  ["privacy", msg("Privacy")],
  ["network", msg("Network")],
  ["ephemeral", msg("Ephemeral mode")],
  ["logging", msg("Logging")],
];

/** A summary line: Ready only when every check it depends on passed now. */
function summary(checks: DiagCheck[]) {
  // Checks are matched by their stable id (their label is translated).
  const by = (ids: string[]) => {
    const cs = checks.filter((c) => ids.includes(c.id));
    if (!cs.length) return { state: msg("Unavailable"), note: "" };
    const bad = cs.find((c) => c.status === "fail");
    const lim = cs.find((c) => c.status === "limited");
    return bad ? { state: msg("Not working"), note: bad.label } : lim ? { state: msg("Limited"), note: lim.label } : { state: msg("Ready"), note: "" };
  };
  return [
    [msg("Core browsing"), by(["Real file type detection", "Symlink protection", "Local data store"])],
    [msg("Offline operation"), by(["Web view remote content", "Decoder worker network access"])],
    [msg("Preview isolation"), by(["Restricted decoder workers", "Preview isolation", "Resource limits"])],
    [msg("Media support"), by(["JPEG", "PNG", "WebP", "GIF", "HEIC / HEIF", "PDF safe preview"])],
    [msg("Temporary sessions"), by(["Temporary session", "Temporary files", "Startup cleanup"])],
  ] as const;
}

/** What this installation can actually do, verified by a self-test on synthetic data. */
export function Diagnostics({ onClose }: { onClose: () => void }) {
  const [checks, setChecks] = useState<DiagCheck[] | null>(null);
  const [details, setDetails] = useState(false);
  const run = () => {
    setChecks(null);
    api.runDiagnostics().then(setChecks);
  };
  useEffect(run, []);
  return (
    <ModalFrame onCancel={onClose} wide>
      <div className="diag-head">
        <div>
          <h2>{t("Mori Diagnostics")}</h2>
          <p className="muted small">Mori {__MORI_VERSION__}</p>
        </div>
        <button className="btn small" onClick={run} disabled={!checks}>
          {t("Run Self-Test")}
        </button>
      </div>
      {!checks && (
        <>
          <ProgressBar value={0} indeterminate />
          <p className="muted small">{t("Testing with synthetic files made in memory — none of your files, no network.")}</p>
        </>
      )}
      {checks && (
        <>
          <div className="diag-summary">
            {summary(checks).map(([label, s]) => (
              <div key={label}>
                <span>{t(label)}</span>
                <span className={s.state === "Ready" ? "" : "attn"}>
                  {t(s.state)}
                  {s.note && <span className="muted small"> · {s.note}</span>}
                </span>
              </div>
            ))}
          </div>
          <label className="toggle diag-toggle">
            <input type="checkbox" checked={details} onChange={(e) => setDetails(e.target.checked)} />
            <span className="switch" aria-hidden />
            {t("Show details")}
          </label>
          <div className="diag-sections">
            {SECTIONS.map(([key, title]) => (
              <section key={key}>
                <h3>{t(title)}</h3>
                {checks
                  .filter((c) => c.section === key)
                  .map((c) => (
                    <div key={c.id} className={`diag-row ${c.status}`}>
                      <span className="diag-mark" title={c.status === "info" ? t("A fact about Mori's code or configuration, not a runtime test") : t(STATUS_LABEL[c.status])}>
                        {MARK[c.status]}
                      </span>
                      <span className="diag-label">{c.label}</span>
                      <span className="diag-value">{c.value}</span>
                      {(details || c.status === "fail" || c.status === "limited") && <span className="diag-detail">{c.detail}</span>}
                    </div>
                  ))}
              </section>
            ))}
            <section>
              <h3>{t("Privacy limitations")}</h3>
              <p className="muted small">
                {t(
                  "Mori minimises what it stores itself, but it can't control traces created outside it: file-system journals, the operating system's swap and memory compression, file-system snapshots (Time Machine, APFS), system crash reports, backups, storage-controller behaviour (wear-levelling on SSDs), Spotlight or other indexing, and third-party monitoring software.",
                )}
              </p>
            </section>
          </div>
          <p className="muted small">{t("✓ verified just now · △ limited · ✕ not working · — fact about Mori's code or configuration (not a runtime test)")}</p>
        </>
      )}
      <div className="dialog-actions">
        <button className="btn primary" onClick={onClose} autoFocus>
          {t("Done")}
        </button>
      </div>
    </ModalFrame>
  );
}
