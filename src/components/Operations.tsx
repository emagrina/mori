import { useEffect, useState } from "react";
import { api, formatDate, formatSize, type Entry, type HistoryRecord, type OperationPlan } from "../api";
import { t, tn, tx } from "../i18n";
import { Icon } from "./Icon";
import { ModalFrame } from "./Modal";

/**
 * Operation Preview: before a mass action, exactly what will happen to each
 * item — and what Mori's mutation policy refuses — computed by the backend
 * without changing anything.
 */
export function OperationPreview({
  op,
  entries,
  onCancel,
  onTrash,
  onDeleted,
}: {
  op: "trash" | "delete";
  entries: Entry[];
  onCancel: () => void;
  onTrash: (ids: string[]) => void;
  onDeleted: (msg: string, ids: string[]) => void;
}) {
  const [plan, setPlan] = useState<OperationPlan | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [overwrite, setOverwrite] = useState(false);
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    api.planOperation(op, entries.map((e) => e.id)).then(setPlan, (e) => setError(String(e)));
  }, []);
  const ok = plan?.entries.filter((e) => !e.blocked) ?? [];
  const confirmed = !plan?.needsTypedConfirm || typed === "DELETE";
  const run = async () => {
    if (!plan) return;
    const ids = ok.map((e) => e.id);
    if (op === "trash") return onTrash(ids);
    setBusy(true);
    try {
      const r = await api.deleteItems(ids, overwrite, typed);
      const n = r.deleted.length;
      const size = formatSize(r.bytes);
      onDeleted(
        r.failed.length
          ? `${overwrite ? tn(n, "Overwrote and deleted {n} item", "Overwrote and deleted {n} items") : tn(n, "Deleted {n} item", "Deleted {n} items")} · ${tn(r.failed.length, "{n} item not deleted ({reason})", "{n} items not deleted ({reason})", { reason: r.failed[0].reason })}`
          : overwrite
            ? tn(n, "Overwrote and deleted {n} item ({size})", "Overwrote and deleted {n} items ({size})", { size })
            : tn(n, "Permanently deleted {n} item ({size})", "Permanently deleted {n} items ({size})", { size }),
        r.deleted,
      );
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };
  const danger = op === "delete";
  return (
    <ModalFrame onCancel={() => !busy && onCancel()} wide>
      <div className={`dialog-icon ${danger ? "danger" : ""}`}>
        <Icon name="trash" size={20} />
      </div>
      <h2>
        {danger ? tn(entries.length, "Delete permanently: {n} item", "Delete permanently: {n} items") : tn(entries.length, "Move to Trash: {n} item", "Move to Trash: {n} items")}
      </h2>
      {!plan && !error && <div className="dot-spinner" />}
      {plan && (
        <>
          <p className="dialog-facts">
            {tn(ok.length, "{n} item", "{n} items")} · {tn(plan.totalFiles, "{n} file", "{n} files")} · {formatSize(plan.totalBytes)}
            {plan.blocked > 0 && <span className="attn"> · {tn(plan.blocked, "{n} item refused (see below)", "{n} items refused (see below)")}</span>}
          </p>
          <div className="op-list">
            {plan.entries.map((e) => (
              <div key={e.id} className={`op-row ${e.blocked ? "blocked" : ""}`}>
                <Icon name={e.kind === "folder" ? "folder" : e.kind === "link" ? "link" : "other"} size={13} />
                <span className="truncate op-path" title={e.path}>
                  {e.path}
                </span>
                <span className="muted op-size">{e.kind === "folder" ? `${tn(e.files, "{n} file", "{n} files")} · ${formatSize(e.bytes)}` : e.kind === "link" ? t("link") : formatSize(e.bytes)}</span>
                {(e.blocked || e.note) && <span className={`op-note ${e.blocked ? "attn" : "muted"}`}>{e.blocked ?? e.note}</span>}
              </div>
            ))}
          </div>
          {danger ? (
            <>
              <p className="op-warning">
                <strong>{t("This bypasses the Trash.")}</strong> {t("Mori can't undo it. Make sure you have what you need from these items.")}
              </p>
              <label className={`op-option ${plan.overwriteUnavailable ? "disabled" : ""}`} title={plan.overwriteUnavailable ?? undefined}>
                <input type="checkbox" disabled={!!plan.overwriteUnavailable} checked={overwrite} onChange={(e) => setOverwrite(e.target.checked)} />
                <span>
                  {t("Overwrite file contents first (single pass)")}
                  <span className="muted small">
                    {plan.overwriteUnavailable
                      ? ` — ${t("not offered here: {reason}", { reason: plan.overwriteUnavailable })}`
                      : ` — ${t("on this spinning disk the old bytes are replaced before deletion. Backups and other copies are not affected, and this is not forensic erasure.")}`}
                  </span>
                </span>
              </label>
              {plan.needsTypedConfirm && (
                <label className="op-typed">
                  {tx("Type {word} to confirm", { word: <strong>DELETE</strong> })}
                  <input className="text-input slim" value={typed} onChange={(e) => setTyped(e.target.value)} autoFocus spellCheck={false} autoComplete="off" />
                </label>
              )}
            </>
          ) : (
            <p className="dialog-note">{t("Items go to the system Trash and can be restored (⌘Z in Mori, or from the Trash).")}</p>
          )}
        </>
      )}
      {error && <p className="field-error">{error}</p>}
      <div className="dialog-actions">
        <button className="btn" onClick={onCancel} disabled={busy} autoFocus={!danger}>
          {t("Cancel")}
        </button>
        <button className={`btn primary ${danger ? "danger-fill" : ""}`} disabled={!plan || !ok.length || !confirmed || busy} onClick={run}>
          {busy ? t("Deleting…") : danger ? (overwrite ? t("Overwrite and Delete") : t("Delete Permanently")) : t("Move to Trash")}
        </button>
      </div>
    </ModalFrame>
  );
}

/** What Mori changed this session, with Undo where it's genuinely possible. */
export function HistoryPanel({ onClose, onUndone }: { onClose: () => void; onUndone: (msg: string) => void }) {
  const [list, setList] = useState<HistoryRecord[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = () => api.historyList().then(setList, (e) => setError(String(e)));
  useEffect(() => {
    load();
  }, []);
  const undo = async (id: number) => {
    setError(null);
    try {
      const r = await api.historyUndo(id);
      const undid = tn(r.restored, "Undid {n} change", "Undid {n} changes");
      onUndone(r.failed.length ? `${undid} · ${r.failed[0]}` : undid);
      load();
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <ModalFrame onCancel={onClose} wide>
      <h2>{t("Undo History")}</h2>
      <p className="muted small">{t("Changes Mori made to your files in this session.")}</p>
      {list && !list.length && <p className="muted">{t("Nothing yet.")}</p>}
      <div className="op-list">
        {list?.map((r) => (
          <div key={r.id} className={`op-row history ${r.undone ? "blocked" : ""}`}>
            <span className="truncate op-path">{r.label}</span>
            <span className="muted op-size">{formatDate(r.at)}</span>
            {r.undone ? (
              <span className="muted small">{t("Undone")}</span>
            ) : r.undoable ? (
              <button className="btn small" onClick={() => undo(r.id)}>
                {t("Undo")}
              </button>
            ) : (
              <span className="muted small op-note">{r.note}</span>
            )}
          </div>
        ))}
      </div>
      {error && <p className="field-error">{error}</p>}
      <div className="dialog-actions">
        <button className="btn" onClick={onClose} autoFocus>
          {t("Done")}
        </button>
      </div>
    </ModalFrame>
  );
}
