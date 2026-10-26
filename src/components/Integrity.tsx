import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { api, formatDate, formatSize, type Checksum, type Entry, type IntegrityDone, type IntegrityProgress, type SnapshotInfo, type VerifyResult } from "../api";
import { ProgressBar } from "./Analyzer";
import { Icon } from "./Icon";
import { ModalFrame } from "./Modal";
import { fmtNum, msg, t, tn } from "../i18n";

function copy(text: string, onNotice: (m: string) => void) {
  // Only when the user clicks Copy; Mori never reads the clipboard.
  navigator.clipboard.writeText(text).then(
    () => onNotice(t("SHA-256 copied")),
    () => onNotice(t("Couldn't copy")),
  );
}

function HashLine({ sum, onNotice }: { sum: string; onNotice: (m: string) => void }) {
  return (
    <div className="hash-line">
      <code className="mono">{sum}</code>
      <button className="btn small" onClick={() => copy(sum, onNotice)}>
        <Icon name="copy" size={12} /> {t("Copy")}
      </button>
    </div>
  );
}

function Facts({ c }: { c: Checksum }) {
  return (
    <dl className="facts">
      <dt>{t("File")}</dt>
      <dd className="wrap">{c.path || c.name}</dd>
      <dt>{t("Size")}</dt>
      <dd>
        {formatSize(c.size)} <span className="muted">{tn(c.size, "({n} byte)", "({n} bytes)")}</span>
      </dd>
      <dt>{t("Modified")}</dt>
      <dd>{formatDate(c.modified)}</dd>
      <dt>{t("Detected type")}</dt>
      <dd>{c.detected ?? "—"}</dd>
    </dl>
  );
}

/** SHA-256 of one file, calculated locally. */
export function ChecksumDialog({ entry, canSave, onClose, onNotice }: { entry: Entry; canSave: boolean; onClose: () => void; onNotice: (m: string) => void }) {
  const [c, setC] = useState<Checksum | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.checksumFile(entry.id).then(setC, (e) => setError(String(e)));
    return () => {
      api.checksumCancel();
    };
  }, [entry.id]);
  return (
    <ModalFrame onCancel={onClose}>
      <div className="integrity-head">{t("File Integrity")}</div>
      <h2 className="truncate">{entry.name}</h2>
      {!c && !error && (
        <>
          <ProgressBar value={0} indeterminate />
          <p className="muted small">{t("Reading the file on this computer…")}</p>
        </>
      )}
      {error && <p className="field-error">{error}</p>}
      {c && (
        <>
          <div className="integrity-label">SHA-256</div>
          <HashLine sum={c.sha256} onNotice={onNotice} />
          <Facts c={c} />
          <p className="dialog-note">
            {c.cached
              ? t(
                  "Calculated locally (remembered from earlier in this session; the file hasn't changed since). Nothing is sent anywhere. The value is kept in memory only until Mori quits or the session ends.",
                )
              : t("Calculated locally. Nothing is sent anywhere. The value is kept in memory only until Mori quits or the session ends.")}
          </p>
        </>
      )}
      <div className="dialog-actions">
        {c && canSave && (
          <button
            className="btn"
            title={t("Saves this checksum in Mori's local data so you can verify the file later")}
            onClick={() => api.integritySave(entry.id).then(() => (onNotice(t("Saving integrity check…")), onClose()), (e) => setError(String(e)))}
          >
            {t("Save Integrity Check…")}
          </button>
        )}
        <button className="btn primary" onClick={onClose} autoFocus>
          {t("Done")}
        </button>
      </div>
    </ModalFrame>
  );
}

/** Exact comparison of two files (byte equality), not visual similarity. */
export function CompareDialog({ a, b, onClose, onNotice }: { a: Entry; b: Entry; onClose: () => void; onNotice: (m: string) => void }) {
  const [r, setR] = useState<{ a: Checksum; b: Checksum; same: boolean } | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api.compareFiles(a.id, b.id).then(setR, (e) => setError(String(e)));
    return () => {
      api.checksumCancel();
    };
  }, [a.id, b.id]);
  return (
    <ModalFrame onCancel={onClose} wide>
      <div className="integrity-head">{t("Compare Integrity")}</div>
      {!r && !error && <ProgressBar value={0} indeterminate />}
      {error && <p className="field-error">{error}</p>}
      {r && (
        <>
          <h2 className={`verdict-line ${r.same ? "same" : "different"}`}>{r.same ? t("SAME CONTENT") : t("DIFFERENT CONTENT")}</h2>
          <p className="muted small">
            {r.same
              ? t("The SHA-256 values match: the two files are byte-for-byte identical.")
              : t("The SHA-256 values differ: the files are not identical. They may still look alike — visual similarity is what Similar Media estimates.")}
          </p>
          {[r.a, r.b].map((c, i) => (
            <div key={i} className="compare-file">
              <div className="integrity-label">
                {i === 0 ? t("File A") : t("File B")} · {c.name}
              </div>
              <HashLine sum={c.sha256} onNotice={onNotice} />
              <div className="muted small">
                {formatSize(c.size)} · {c.detected ?? t("unknown type")} · {c.path}
              </div>
            </div>
          ))}
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

/** Saved integrity snapshots: verify, delete; progress of a running check. */
export function IntegrityPanel({ onClose, onNotice }: { onClose: () => void; onNotice: (m: string) => void }) {
  const [list, setList] = useState<SnapshotInfo[] | null>(null);
  const [progress, setProgress] = useState<IntegrityProgress | null>(null);
  const [result, setResult] = useState<VerifyResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = () => api.integrityList().then(setList);
  useEffect(() => {
    load();
    const un = [
      listen<IntegrityProgress>("integrity-progress", (e) => setProgress(e.payload)),
      listen<IntegrityDone>("integrity-done", (e) => {
        setProgress(null);
        if (e.payload.status === "failed") setError(e.payload.message);
        if (e.payload.result) setResult(e.payload.result);
        load();
      }),
    ];
    return () => un.forEach((p) => p.then((f) => f()));
  }, []);
  return (
    <ModalFrame onCancel={onClose} wide>
      <h2>{t("Integrity Snapshots")}</h2>
      <p className="muted small">
        {t(
          "Saved checksums of files and folders, kept only in Mori's local data on this computer (never next to your files). Verify recalculates SHA-256 and reports what changed.",
        )}
      </p>
      {progress && (
        <div className="integrity-progress">
          <ProgressBar value={progress.bytesTotal ? progress.bytesDone / progress.bytesTotal : 0} indeterminate={progress.stage === "listing"} />
          <span className="muted small">
            {progress.paused
              ? t("Paused")
              : progress.stage === "listing"
                ? t("Listing files…")
                : t("Hashing · {done} of {total}", { done: formatSize(progress.bytesDone), total: formatSize(progress.bytesTotal) })}
          </span>
          <button className="btn small" onClick={() => api.integrityPause(!progress.paused)}>
            {progress.paused ? t("Resume") : t("Pause")}
          </button>
          <button className="btn small" onClick={() => api.integrityCancel()}>
            {t("Cancel")}
          </button>
        </div>
      )}
      {error && <p className="field-error">{error}</p>}
      {result && <VerifyReport r={result} />}
      <div className="op-list">
        {list?.map((s) => (
          <div key={s.id} className="op-row history">
            <span className="truncate op-path">
              <Icon name={s.kind === "folder" ? "folder" : "other"} size={12} /> {s.label}{" "}
              <span className="muted small">
                · {tn(s.files, "{n} file", "{n} files")} · {formatSize(s.bytes)}
                {s.skipped.privateFolders > 0 && " · " + tn(s.skipped.privateFolders, "{n} private folder not included", "{n} private folders not included")}
                {s.skipped.links > 0 && " · " + tn(s.skipped.links, "{n} link not followed", "{n} links not followed")}
              </span>
            </span>
            <span className="muted op-size">{formatDate(s.created)}</span>
            <span className="snap-actions">
              <button className="btn small" disabled={!s.available || !!progress} title={s.available ? t("Recalculate and compare") : t("Its drive or folder isn't available")} onClick={() => (setResult(null), setError(null), api.integrityVerify(s.id).catch((e) => setError(String(e))))}>
                {t("Verify")}
              </button>
              <button className="icon-btn" title={t("Delete this snapshot (Mori's record only)")} onClick={() => api.integrityDelete(s.id).then(load, (e) => onNotice(String(e)))}>
                <Icon name="trash" size={13} />
              </button>
            </span>
          </div>
        ))}
        {list && !list.length && <p className="muted small op-row">{t("No snapshots yet. Use “Create Integrity Snapshot” on a folder or “Save Integrity Check” on a file.")}</p>}
      </div>
      <div className="dialog-actions">
        <button className="btn primary" onClick={onClose} autoFocus>
          {t("Done")}
        </button>
      </div>
    </ModalFrame>
  );
}

function VerifyReport({ r }: { r: VerifyResult }) {
  const clean = !r.changed.length && !r.missing.length && !r.added.length && !r.unreadable.length;
  const list = (title: string, items: string[]) =>
    items.length > 0 && (
      <details className="verify-list">
        <summary>
          {t(title)} · {fmtNum(items.length)}
        </summary>
        <ul>
          {items.slice(0, 200).map((p) => (
            <li key={p} className="mono">
              {p}
            </li>
          ))}
        </ul>
      </details>
    );
  return (
    <div className="verify-report">
      <h3 className={`verdict-line ${clean ? "same" : "different"}`}>{clean ? t("UNCHANGED") : t("CHANGED")}</h3>
      <div className="muted small">
        {r.snapshot.label} · {tn(r.unchanged, "{n} file unchanged", "{n} files unchanged")}
      </div>
      {list(msg("Changed"), r.changed)}
      {list(msg("Missing"), r.missing)}
      {list(msg("New"), r.added)}
      {list(msg("Unreadable"), r.unreadable)}
    </div>
  );
}
