import { useEffect, useState } from "react";
import { api, formatDate, formatSize, type FileReport, type RiskLevel } from "../api";
import { Icon } from "./Icon";

const LEVEL: Record<RiskLevel, string> = { high: "High attention", attention: "Attention", info: "Info" };

/**
 * Factual file report: real type, extension check, risk indicators,
 * permissions. Mori reports what it observed — never "safe" or "virus".
 */
export function Inspector({ id, onClose }: { id: string; onClose: () => void }) {
  const [report, setReport] = useState<FileReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    setReport(null);
    setError(null);
    api.fileReport(id).then(
      (r) => alive && setReport(r),
      (e) => alive && setError(String(e)),
    );
    return () => {
      alive = false;
    };
  }, [id]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);

  const r = report;
  const flagged = r?.findings.filter((f) => f.level !== "info") ?? [];
  return (
    <aside className="inspector" aria-label="File information">
      <header>
        <h2 className="truncate" title={r?.name}>
          {r?.name ?? "Info"}
        </h2>
        <button className="icon-btn" onClick={onClose} aria-label="Close">
          <Icon name="close" size={13} />
        </button>
      </header>
      {error && <p className="field-error">{error}</p>}
      {!r && !error && <div className="dot-spinner" />}
      {r && (
        <div className="inspector-body">
          <section className={`verdict ${flagged.length ? "flagged" : ""}`}>
            <Icon name={flagged.length ? "warning" : "info"} size={14} />
            <span>{r.summary}</span>
          </section>

          {r.kind === "file" && (
            <section>
              <h3>Type</h3>
              <dl>
                <dt>Detected</dt>
                <dd>{r.detected ? r.detected.label : "—"}</dd>
                <dt>MIME</dt>
                <dd className="mono">{r.detected?.mime ?? "—"}</dd>
                <dt>Extension</dt>
                <dd>
                  {r.ext ? `.${r.ext}` : "none"}
                  {r.extMatches === true && <span className="muted"> · matches content</span>}
                  {r.extMatches === false && <span className="attn"> · does not match content</span>}
                </dd>
              </dl>
            </section>
          )}

          {r.findings.length > 0 && (
            <section>
              <h3>Findings</h3>
              <ul className="findings">
                {r.findings.map((f, i) => (
                  <li key={i} className={`finding ${f.level}`}>
                    <div className="finding-head">
                      <span className="level">{LEVEL[f.level]}</span>
                      <span className="finding-title">{f.title}</span>
                    </div>
                    <p>{f.detail}</p>
                  </li>
                ))}
              </ul>
            </section>
          )}

          {r.kind === "link" && (
            <section>
              <h3>Symbolic link</h3>
              <dl>
                <dt>Points to</dt>
                <dd className="mono wrap">{r.linkTarget || "—"}</dd>
                <dt>Target</dt>
                <dd>{r.linkOutside ? "Outside this folder" : "Inside this folder"} · never followed by Mori</dd>
              </dl>
            </section>
          )}

          <section>
            <h3>General</h3>
            <dl>
              <dt>Kind</dt>
              <dd>{r.kind === "file" ? "File" : r.kind === "folder" ? "Folder" : "Symbolic link"}</dd>
              {r.kind === "file" && (
                <>
                  <dt>Size</dt>
                  <dd>
                    {formatSize(r.size)} <span className="muted">({r.size.toLocaleString()} bytes)</span>
                  </dd>
                </>
              )}
              <dt>Where</dt>
              <dd className="wrap">{r.path}</dd>
              <dt>Created</dt>
              <dd>{formatDate(r.created)}</dd>
              <dt>Modified</dt>
              <dd>{formatDate(r.modified)}</dd>
              <dt>Accessed</dt>
              <dd>{formatDate(r.accessed)}</dd>
            </dl>
          </section>

          {r.permissions && (
            <section>
              <h3>Permissions</h3>
              <dl>
                <dt>Mode</dt>
                <dd className="mono">
                  {r.permissions.mode} <span className="muted">({r.permissions.octal})</span>
                </dd>
                <dt>Owner</dt>
                <dd>{r.permissions.owner}</dd>
                <dt>Group</dt>
                <dd>{r.permissions.group}</dd>
                {r.permissions.special.length > 0 && (
                  <>
                    <dt>Special</dt>
                    <dd>{r.permissions.special.join(", ")}</dd>
                  </>
                )}
                {r.permissions.flags.length > 0 && (
                  <>
                    <dt>Flags</dt>
                    <dd>{r.permissions.flags.join(", ")}</dd>
                  </>
                )}
                <dt>ACL</dt>
                <dd>{r.permissions.aclEntries ? `${r.permissions.aclEntries} entries` : "none"}</dd>
                {r.permissions.hardLinks > 1 && (
                  <>
                    <dt>Hard links</dt>
                    <dd>{r.permissions.hardLinks}</dd>
                  </>
                )}
                {r.permissions.xattrs.length > 0 && (
                  <>
                    <dt>Attributes</dt>
                    <dd className="mono wrap">{r.permissions.xattrs.join("\n")}</dd>
                  </>
                )}
              </dl>
            </section>
          )}

          {(r.marks.insidePrivate || r.marks.insideProtected) && (
            <section>
              <h3>In Mori</h3>
              <dl>
                {r.marks.insidePrivate && (
                  <>
                    <dt>Private</dt>
                    <dd>{r.marks.private ? "This folder is private" : "Inside a private folder"}</dd>
                  </>
                )}
                {r.marks.insideProtected && (
                  <>
                    <dt>Never Modify</dt>
                    <dd>{r.marks.protected ? "This folder is protected" : "Inside a protected folder"}</dd>
                  </>
                )}
              </dl>
            </section>
          )}
          <p className="fineprint">These are observations, not a malware verdict. Mori is not an antivirus.</p>
        </div>
      )}
    </aside>
  );
}
