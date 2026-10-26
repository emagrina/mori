import { useEffect, useState } from "react";
import { api, CATEGORY_LABEL, formatDate, formatSize, type FileMeta, type FileReport, type MetaField, type RiskLevel } from "../api";
import { fmtNum, msg, t, tn, tx } from "../i18n";
import { review } from "../review";
import { Icon } from "./Icon";

const LEVEL: Record<RiskLevel, string> = { high: msg("High attention"), attention: msg("Attention"), info: msg("Info") };

/**
 * Factual file report: real type, extension check, risk indicators,
 * permissions. Mori reports what it observed — never "safe" or "virus".
 */
export function Inspector({ id, onClose, onNotice, over = false }: { id: string; onClose: () => void; onNotice?: (msg: string) => void; /** Shown over the preview. */ over?: boolean }) {
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
    <aside className={`inspector ${over ? "over" : ""}`} aria-label={t("File information")}>
      <header>
        <h2 className="truncate" title={r?.name}>
          {r?.name ?? t("Info")}
        </h2>
        <button className="icon-btn" onClick={onClose} aria-label={t("Close")}>
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
              <h3>{t("Type")}</h3>
              <dl>
                <dt>{t("Detected")}</dt>
                <dd>{r.detected ? r.detected.label : "—"}</dd>
                <dt>MIME</dt>
                <dd className="mono">{r.detected?.mime ?? "—"}</dd>
                <dt>{t("Extension")}</dt>
                <dd>
                  {r.ext ? `.${r.ext}` : t("none")}
                  {r.extMatches === true && <span className="muted"> · {t("matches content")}</span>}
                  {r.extMatches === false && <span className="attn"> · {t("does not match content")}</span>}
                </dd>
              </dl>
            </section>
          )}

          {r.findings.length > 0 && (
            <section>
              <h3>{t("Findings")}</h3>
              <ul className="findings">
                {r.findings.map((f, i) => (
                  <li key={i} className={`finding ${f.level}`}>
                    <div className="finding-head">
                      <span className="level">{t(LEVEL[f.level])}</span>
                      <span className="finding-title">{f.title}</span>
                    </div>
                    <p>{f.detail}</p>
                  </li>
                ))}
              </ul>
            </section>
          )}

          {r.kind === "file" && <MetadataSection id={id} onNotice={onNotice} />}

          {r.kind === "link" && (
            <section>
              <h3>{t("Symbolic link")}</h3>
              <dl>
                <dt>{t("Points to")}</dt>
                <dd className="mono wrap">{r.linkTarget || "—"}</dd>
                <dt>{t("Target")}</dt>
                <dd>{r.linkOutside ? t("Outside this folder · never followed by Mori") : t("Inside this folder · never followed by Mori")}</dd>
              </dl>
            </section>
          )}

          <section>
            <h3>{t("General")}</h3>
            <dl>
              <dt>{t("Kind")}</dt>
              <dd>{r.kind === "file" ? t("File") : r.kind === "folder" ? t("Folder") : t("Symbolic link")}</dd>
              {r.kind === "file" && (
                <>
                  <dt>{t("Size")}</dt>
                  <dd>
                    {formatSize(r.size)} <span className="muted">({tn(r.size, "{n} byte", "{n} bytes")})</span>
                  </dd>
                </>
              )}
              <dt>{t("Where")}</dt>
              <dd className="wrap">{r.path}</dd>
              <dt>{t("Created")}</dt>
              <dd>{formatDate(r.created)}</dd>
              <dt>{t("Modified")}</dt>
              <dd>{formatDate(r.modified)}</dd>
              <dt>{t("Accessed")}</dt>
              <dd>{formatDate(r.accessed)}</dd>
            </dl>
          </section>

          {r.permissions && (
            <section>
              <h3>{t("Permissions")}</h3>
              <dl>
                <dt>{t("Mode")}</dt>
                <dd className="mono">
                  {r.permissions.mode} <span className="muted">({r.permissions.octal})</span>
                </dd>
                <dt>{t("Owner")}</dt>
                <dd>{r.permissions.owner}</dd>
                <dt>{t("Group")}</dt>
                <dd>{r.permissions.group}</dd>
                {r.permissions.special.length > 0 && (
                  <>
                    <dt>{t("Special")}</dt>
                    <dd>{r.permissions.special.join(", ")}</dd>
                  </>
                )}
                {r.permissions.flags.length > 0 && (
                  <>
                    <dt>{t("Flags")}</dt>
                    <dd>{r.permissions.flags.join(", ")}</dd>
                  </>
                )}
                <dt>ACL</dt>
                <dd>{r.permissions.aclEntries ? tn(r.permissions.aclEntries, "{n} entry", "{n} entries") : t("none")}</dd>
                {r.permissions.hardLinks > 1 && (
                  <>
                    <dt>{t("Hard links")}</dt>
                    <dd>{fmtNum(r.permissions.hardLinks)}</dd>
                  </>
                )}
                {r.permissions.xattrs.length > 0 && (
                  <>
                    <dt>{t("Attributes")}</dt>
                    <dd className="mono wrap">{r.permissions.xattrs.join("\n")}</dd>
                  </>
                )}
              </dl>
            </section>
          )}

          {(r.marks.insidePrivate || r.marks.insideProtected) && (
            <section>
              <h3>{t("In Mori")}</h3>
              <dl>
                {r.marks.insidePrivate && (
                  <>
                    <dt>{t("Private")}</dt>
                    <dd>{r.marks.private ? t("This folder is private") : t("Inside a private folder")}</dd>
                  </>
                )}
                {r.marks.insideProtected && (
                  <>
                    <dt>{t("Never Modify")}</dt>
                    <dd>{r.marks.protected ? t("This folder is protected") : t("Inside a protected folder")}</dd>
                  </>
                )}
              </dl>
            </section>
          )}
          <p className="fineprint">{t("These are observations, not a malware verdict. Mori is not an antivirus.")}</p>
        </div>
      )}
    </aside>
  );
}

/** Embedded metadata, read by the sandboxed worker. */
function MetadataSection({ id, onNotice }: { id: string; onNotice?: (msg: string) => void }) {
  const [meta, setMeta] = useState<FileMeta | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const [all, setAll] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);

  useEffect(() => {
    let alive = true;
    setMeta(null);
    setFailed(null);
    setAll(false);
    setConfirm(false);
    setResult(null);
    api.fileMetadata(id).then(
      (m) => alive && setMeta(m),
      (e) => alive && setFailed(String(e)),
    );
    return () => {
      alive = false;
    };
  }, [id]);

  const createCopy = async () => {
    setBusy(true);
    const [o] = await api.sanitizeCopies([id]).catch((e) => [{ id, name: "", newName: null, error: String(e) }]);
    setBusy(false);
    setConfirm(false);
    if (o?.newName) {
      setResult({ ok: true, text: t("Created “{name}” next to the original. The original was not changed.", { name: o.newName }) });
      onNotice?.(t("Sanitized copy created: “{name}”", { name: o.newName }));
      review.success("sanitize");
    } else {
      setResult({ ok: false, text: t("No copy was made: {error}.", { error: o?.error ?? t("unknown error") }) });
      review.problem();
    }
  };

  if (failed) {
    return (
      <section>
        <h3>{t("Metadata")}</h3>
        <p className="muted small">{failed}</p>
      </section>
    );
  }
  if (!meta) {
    return (
      <section>
        <h3>{t("Metadata")}</h3>
        <div className="dot-spinner" />
      </section>
    );
  }
  const sensitive = meta.fields.filter((f) => f.sensitive);
  const groups = [...new Set(meta.fields.map((f) => f.group))];
  return (
    <section>
      <h3>{t("Metadata")}</h3>
      {meta.fields.length === 0 && !meta.gps ? (
        <p className="muted small">
          {meta.container !== "Unknown" ? t("No embedded metadata found in this {format} file.", { format: meta.container }) : t("No embedded metadata found.")}
        </p>
      ) : (
        <>
          {meta.categories.length > 0 ? (
            <p className="meta-contains">
              {t("Contains: {categories}", { categories: meta.categories.map((c) => CATEGORY_LABEL[c]).join(" · ") })}
            </p>
          ) : (
            <p className="muted small">{t("No location, people, device, software, comment or ID fields.")}</p>
          )}
          {meta.gps && (
            <dl>
              <dt>{t("Position")}</dt>
              <dd className="mono">
                {meta.gps[0].toFixed(5)}, {meta.gps[1].toFixed(5)}
              </dd>
            </dl>
          )}
          {!all && sensitive.length > 0 && <FieldList fields={sensitive.slice(0, 10)} />}
          {all &&
            groups.map((g) => (
              <div key={g} className="meta-group">
                <div className="meta-group-name">{g}</div>
                <FieldList fields={meta.fields.filter((f) => f.group === g)} />
              </div>
            ))}
          <button className="link-btn" onClick={() => setAll(!all)}>
            {all ? t("Show sensitive fields only") : tn(meta.fields.length, "Show all metadata ({n} field)", "Show all metadata ({n} fields)")}
          </button>
          {meta.partial && <p className="muted small">{t("Some metadata was damaged or past a size limit; the list may be incomplete.")}</p>}
        </>
      )}
      {meta.sanitizable && meta.categories.length > 0 && !confirm && (
        <button className="btn small meta-sanitize" onClick={() => setConfirm(true)} disabled={busy}>
          {t("Create Sanitized Copy…")}
        </button>
      )}
      {confirm && (
        <div className="confirm-box">
          <p>
            {tx("Mori will write a {newFile} next to this one with the same image and no metadata (orientation is kept). The original is not changed.", {
              newFile: <strong>{t("new file")}</strong>,
            })}
          </p>
          <div className="dialog-actions">
            <button className="btn small" onClick={() => setConfirm(false)} disabled={busy}>
              {t("Cancel")}
            </button>
            <button className="btn small primary" onClick={createCopy} disabled={busy}>
              {busy ? t("Creating…") : t("Create Copy")}
            </button>
          </div>
        </div>
      )}
      {result && <p className={`small ${result.ok ? "muted" : "attn"}`}>{result.text}</p>}
    </section>
  );
}

function FieldList({ fields }: { fields: MetaField[] }) {
  return (
    <dl className="meta-fields">
      {fields.map((f, i) => (
        <div key={i} className={f.sensitive ? "sensitive" : ""}>
          <dt title={f.sensitive ? CATEGORY_LABEL[f.sensitive] : undefined}>{f.name}</dt>
          <dd className="wrap">{f.value}</dd>
        </div>
      ))}
    </dl>
  );
}
