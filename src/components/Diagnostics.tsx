import { useEffect, useState } from "react";
import { api, type DiagCheck, type DiagStatus } from "../api";
import { ProgressBar } from "./Analyzer";
import { ModalFrame } from "./Modal";

/** Set at build time from package.json (kept equal to Cargo.toml and tauri.conf.json by a test). */
declare const __MORI_VERSION__: string;

const MARK: Record<DiagStatus, string> = { pass: "✓", limited: "△", fail: "✕", info: "—" };
const SECTIONS: [DiagCheck["section"], string][] = [
  ["security", "Security"],
  ["media", "Media"],
  ["privacy", "Privacy"],
  ["network", "Network"],
  ["ephemeral", "Ephemeral mode"],
  ["logging", "Logging"],
];

/** A summary line: Ready only when every check it depends on passed now. */
function summary(checks: DiagCheck[]) {
  const by = (labels: string[]) => {
    const cs = checks.filter((c) => labels.includes(c.label));
    if (!cs.length) return { state: "Unavailable", note: "" };
    const bad = cs.find((c) => c.status === "fail");
    const lim = cs.find((c) => c.status === "limited");
    return bad ? { state: "Not working", note: bad.label } : lim ? { state: "Limited", note: lim.label } : { state: "Ready", note: "" };
  };
  return [
    ["Core browsing", by(["Real file type detection", "Symlink protection", "Local data store"])],
    ["Offline operation", by(["Web view remote content", "Decoder worker network access"])],
    ["Preview isolation", by(["Restricted decoder workers", "Preview isolation", "Resource limits"])],
    ["Media support", by(["JPEG", "PNG", "WebP", "GIF", "HEIC / HEIF", "PDF safe preview"])],
    ["Temporary sessions", by(["Temporary session", "Temporary files", "Startup cleanup"])],
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
          <h2>Mori Diagnostics</h2>
          <p className="muted small">Mori {__MORI_VERSION__}</p>
        </div>
        <button className="btn small" onClick={run} disabled={!checks}>
          Run Self-Test
        </button>
      </div>
      {!checks && (
        <>
          <ProgressBar value={0} indeterminate />
          <p className="muted small">Testing with synthetic files made in memory — none of your files, no network.</p>
        </>
      )}
      {checks && (
        <>
          <div className="diag-summary">
            {summary(checks).map(([label, s]) => (
              <div key={label}>
                <span>{label}</span>
                <span className={s.state === "Ready" ? "" : "attn"}>
                  {s.state}
                  {s.note && <span className="muted small"> · {s.note}</span>}
                </span>
              </div>
            ))}
          </div>
          <label className="toggle diag-toggle">
            <input type="checkbox" checked={details} onChange={(e) => setDetails(e.target.checked)} />
            <span className="switch" aria-hidden />
            Show details
          </label>
          <div className="diag-sections">
            {SECTIONS.map(([key, title]) => (
              <section key={key}>
                <h3>{title}</h3>
                {checks
                  .filter((c) => c.section === key)
                  .map((c) => (
                    <div key={c.label} className={`diag-row ${c.status}`}>
                      <span className="diag-mark" title={c.status === "info" ? "A fact about Mori's code or configuration, not a runtime test" : c.status}>
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
              <h3>Privacy limitations</h3>
              <p className="muted small">
                Mori minimises what it stores itself, but it can't control traces created outside it: file-system journals, the operating system's
                swap and memory compression, file-system snapshots (Time Machine, APFS), system crash reports, backups, storage-controller behaviour
                (wear-levelling on SSDs), Spotlight or other indexing, and third-party monitoring software.
              </p>
            </section>
          </div>
          <p className="muted small">✓ verified just now · △ limited · ✕ not working · — fact about Mori's code or configuration (not a runtime test)</p>
        </>
      )}
      <div className="dialog-actions">
        <button className="btn primary" onClick={onClose} autoFocus>
          Done
        </button>
      </div>
    </ModalFrame>
  );
}
