import { listen } from "@tauri-apps/api/event";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  formatSize,
  PRIVATE_HINT,
  type DestInfo,
  type Entry,
  type Resolution,
  type TransferOp,
  type TransferPlan,
  type TransferResult,
} from "../api";
import { t, tn } from "../i18n";
import { Icon } from "./Icon";
import { ModalFrame } from "./Modal";

/** A destination: a folder of the browsed drive ("" = its root) or one picked elsewhere. */
export interface Dest {
  key: string;
  label: string;
  /** Shown under the label (where it is). */
  where: string;
}

/** Destinations used this session (memory only, like Mori's other session state). */
let recent: Dest[] = [];
export const forgetRecentDestinations = () => {
  recent = [];
};

/** Dialog headings: one item is named, several are counted. */
function headings(op: TransferOp, entries: Entry[]) {
  const n = entries.length;
  const name = n === 1 ? entries[0].name : null;
  const move = op === "move";
  return {
    /** "Move “a.jpg” to…" */
    pick: () =>
      name !== null
        ? move
          ? t("Move “{name}” to…", { name })
          : t("Copy “{name}” to…", { name })
        : move
          ? tn(n, "Move {n} item to…", "Move {n} items to…")
          : tn(n, "Copy {n} item to…", "Copy {n} items to…"),
    /** "Move “a.jpg” to “Photos”" */
    into: (dest: string) =>
      name !== null
        ? move
          ? t("Move “{name}” to “{dest}”", { name, dest })
          : t("Copy “{name}” to “{dest}”", { name, dest })
        : move
          ? tn(n, "Move {n} item to “{dest}”", "Move {n} items to “{dest}”", { dest })
          : tn(n, "Copy {n} item to “{dest}”", "Copy {n} items to “{dest}”", { dest }),
    /** "Moving “a.jpg”…" */
    running: () =>
      name !== null
        ? move
          ? t("Moving “{name}”…", { name })
          : t("Copying “{name}”…", { name })
        : move
          ? tn(n, "Moving {n} item…", "Moving {n} items…")
          : tn(n, "Copying {n} item…", "Copying {n} items…"),
  };
}

/**
 * Move to… / Copy to…: pick a destination, see exactly what will happen
 * (conflicts, refusals) — computed by the backend without changing
 * anything — resolve conflicts, then run. Nothing is ever overwritten in
 * place: Replace moves the existing file to the Trash first.
 */
export function TransferDialog({
  op,
  entries,
  rootName,
  current,
  dest: preset,
  onClose,
  onDone,
}: {
  op: TransferOp;
  entries: Entry[];
  rootName: string;
  /** The folder being browsed (pre-selected as a starting point). */
  current: string;
  /** Dropped onto a folder: skip the picker, plan the move into it straight away. */
  dest?: Dest;
  onClose: () => void;
  onDone: (result: TransferResult, plan: TransferPlan) => void;
}) {
  const [dest, setDest] = useState<Dest | null>(null);
  const [plan, setPlan] = useState<TransferPlan | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [res, setRes] = useState<Record<string, Resolution>>({});
  const head = headings(op, entries);
  const sources = useMemo(() => new Set(entries.map((e) => e.id)), [entries]);

  const choose = async (d: Dest) => {
    setError(null);
    setBusy(true);
    try {
      const p = await api.planTransfer(
        op,
        entries.map((e) => e.id),
        d.key,
      );
      recent = [d, ...recent.filter((r) => r.key !== d.key)].slice(0, 5);
      setDest(d);
      // Conflicts start unresolved: nothing happens to them unless chosen.
      setRes({});
      setPlan(p);
      // Nothing to decide: go straight ahead.
      if (!p.conflicts && !p.blocked) await run(p, d, {});
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const run = async (p: TransferPlan, d: Dest, resolutions: Record<string, Resolution>) => {
    setBusy(true);
    setProgress({ done: 0, total: p.entries.length });
    const un = listen<{ done: number; total: number }>("transfer-progress", (e) => setProgress(e.payload));
    try {
      const r = await api.transferItems(
        op,
        entries.map((e) => e.id),
        d.key,
        resolutions,
      );
      onDone(r, p);
    } catch (e) {
      setError(String(e));
      setBusy(false);
      setProgress(null);
    } finally {
      un.then((f) => f());
    }
  };

  const other = async () => {
    try {
      const d: DestInfo = await api.transferChooseFolder();
      choose({ key: d.key, label: d.label, where: d.path });
    } catch (e) {
      if (String(e) !== "cancelled") setError(String(e));
    }
  };

  const conflicts = plan?.entries.filter((e) => !e.blocked && e.conflict) ?? [];
  const blocked = plan?.entries.filter((e) => e.blocked) ?? [];
  const ready = plan ? plan.entries.length - blocked.length : 0;
  const replacing = conflicts.filter((e) => res[e.id] === "replace").length;
  const applyAll = (r: Resolution) => setRes(Object.fromEntries(conflicts.map((e) => [e.id, r === "replace" && !e.replaceable ? "keepBoth" : r])));

  // A drop: plan at once (and run straight away if nothing needs a decision).
  // Exactly once, even when React mounts effects twice (StrictMode): this can move files.
  const started = useRef(false);
  useEffect(() => {
    if (!preset || started.current) return;
    started.current = true;
    choose(preset);
  }, []);

  if (progress) {
    return (
      <ModalFrame onCancel={() => {}}>
        <h2>{head.running()}</h2>
        <div className="progress transfer-progress">
          <div style={{ width: `${progress.total ? (progress.done / progress.total) * 100 : 0}%` }} />
        </div>
        <p className="muted small">
          {t("{done} of {total} · to “{dest}”", { done: progress.done, total: progress.total, dest: dest?.label ?? "" })}
        </p>
        <div className="dialog-actions">
          <button className="btn" onClick={() => api.transferCancel()} title={t("Stops after the current item; a partly copied item is removed again")}>
            {t("Stop")}
          </button>
        </div>
      </ModalFrame>
    );
  }

  if (preset && !plan) {
    return (
      <ModalFrame onCancel={() => !busy && onClose()}>
        <h2>{head.into(preset.label)}</h2>
        {error ? <p className="field-error">{error}</p> : <div className="dot-spinner" />}
        <div className="dialog-actions">
          <button className="btn" onClick={onClose} disabled={busy && !error}>
            {error ? t("Close") : t("Cancel")}
          </button>
        </div>
      </ModalFrame>
    );
  }

  if (plan && dest) {
    return (
      <ModalFrame onCancel={() => !busy && onClose()} wide>
        <div className="dialog-icon">
          <Icon name="folder" size={20} />
        </div>
        <h2>{head.into(dest.label)}</h2>
        <p className="dialog-facts">
          {tn(ready, "{n} item", "{n} items")} · {formatSize(plan.totalBytes)}
          {blocked.length > 0 && (
            <span className="attn">
              {" "}
              ·{" "}
              {op === "move"
                ? tn(blocked.length, "{n} item can't be moved", "{n} items can't be moved")
                : tn(blocked.length, "{n} item can't be copied", "{n} items can't be copied")}
            </span>
          )}
          {conflicts.length > 0 && <span className="attn"> · {tn(conflicts.length, "{n} name conflict", "{n} name conflicts")}</span>}
        </p>
        {plan.crossVolume && (
          <p className="dialog-note">{t("“{dest}” is on another drive: items are copied and checked, then the originals go to the Trash (never deleted).", { dest: dest.label })}</p>
        )}
        {conflicts.length > 1 && (
          <div className="conflict-all">
            <span className="muted small">{t("Apply to all:")}</span>
            <button className="btn small" onClick={() => applyAll("keepBoth")}>
              {t("Keep Both")}
            </button>
            {conflicts.some((e) => e.replaceable) && (
              <button className="btn small" onClick={() => applyAll("replace")} title={t("Only files over files; what's replaced goes to the Trash")}>
                {t("Replace")}
              </button>
            )}
            <button className="btn small" onClick={() => applyAll("skip")}>
              {t("Skip")}
            </button>
          </div>
        )}
        <div className="op-list">
          {conflicts.map((e) => (
            <div key={e.id} className="op-row conflict">
              <Icon name={e.kind === "folder" ? "folder" : e.kind === "link" ? "link" : "other"} size={13} />
              <span className="truncate op-path" title={e.path}>
                {e.name}
                <span className="muted small">
                  {" "}
                  —{" "}
                  {e.conflict === "batch"
                    ? t("another selected item has the same name")
                    : e.conflict === "folder"
                      ? t("a folder with this name is already there")
                      : e.conflict === "link"
                        ? t("a link with this name is already there")
                        : t("a file with this name is already there")}
                </span>
              </span>
              <div className="segmented small" role="radiogroup" aria-label={t("What to do with {name}", { name: e.name })}>
                {(["keepBoth", "replace", "skip"] as const)
                  .filter((r) => r !== "replace" || e.replaceable)
                  .map((r) => (
                    <button
                      key={r}
                      role="radio"
                      aria-checked={res[e.id] === r}
                      className={res[e.id] === r ? "on" : ""}
                      onClick={() => setRes((m) => ({ ...m, [e.id]: r }))}
                      title={
                        r === "replace"
                          ? t("The existing file goes to the Trash first (restorable)")
                          : r === "keepBoth"
                            ? t("Give the new one a free name (“name 2”)")
                            : t("Leave this item where it is")
                      }
                    >
                      {r === "keepBoth" ? t("Keep Both") : r === "replace" ? t("Replace") : t("Skip")}
                    </button>
                  ))}
              </div>
            </div>
          ))}
          {blocked.map((e) => (
            <div key={e.id} className="op-row blocked">
              <Icon name={e.kind === "folder" ? "folder" : "other"} size={13} />
              <span className="truncate op-path" title={e.path}>
                {e.name}
              </span>
              <span className="op-note attn">{e.blocked}</span>
            </div>
          ))}
        </div>
        {replacing > 0 && (
          <p className="op-warning">
            <strong>{tn(replacing, "{n} existing file will be replaced.", "{n} existing files will be replaced.")}</strong>{" "}
            {tn(replacing, "It goes to the Trash first, so you can restore it (or press Undo).", "They go to the Trash first, so you can restore them (or press Undo).")}
          </p>
        )}
        {conflicts.some((e) => !res[e.id]) && <p className="muted small">{t("Conflicts left undecided are skipped. Nothing is overwritten.")}</p>}
        {error && <p className="field-error">{error}</p>}
        <div className="dialog-actions">
          {!preset && (
            <button className="btn" onClick={() => (setPlan(null), setDest(null))} disabled={busy}>
              {t("Back")}
            </button>
          )}
          <button className="btn" onClick={onClose} disabled={busy}>
            {t("Cancel")}
          </button>
          <button className="btn primary" disabled={busy || !ready} onClick={() => run(plan, dest, res)} autoFocus>
            {replacing
              ? op === "move"
                ? t("Replace {n} and Move", { n: replacing })
                : t("Replace {n} and Copy", { n: replacing })
              : op === "move"
                ? tn(ready - conflicts.filter((e) => !res[e.id] || res[e.id] === "skip").length, "Move {n} item", "Move {n} items")
                : tn(ready - conflicts.filter((e) => !res[e.id] || res[e.id] === "skip").length, "Copy {n} item", "Copy {n} items")}
          </button>
        </div>
      </ModalFrame>
    );
  }

  return (
    <ModalFrame onCancel={() => !busy && onClose()} wide>
      <h2>{head.pick()}</h2>
      {recent.length > 0 && (
        <>
          <div className="menu-label">{t("Recent")}</div>
          <div className="dest-list">
            {recent.map((d) => (
              <button key={d.key} className="dest-row" onClick={() => choose(d)} disabled={busy}>
                <Icon name="clock" size={14} />
                <span className="truncate">{d.label}</span>
                <span className="muted small truncate">{d.where}</span>
              </button>
            ))}
          </div>
        </>
      )}
      <div className="menu-label">{t("Folders")}</div>
      <DestTree rootName={rootName} current={current} sources={sources} busy={busy} onChoose={choose} />
      {error && <p className="field-error">{error}</p>}
      <div className="dialog-actions">
        <button className="btn" onClick={other} disabled={busy} title={t("A folder outside the one you're browsing, chosen in the system's folder picker")}>
          {t("Other Folder…")}
        </button>
        <span className="spacer" />
        <button className="btn" onClick={onClose} disabled={busy}>
          {t("Cancel")}
        </button>
      </div>
    </ModalFrame>
  );
}

/** The drive's folders, expanded lazily. Click a folder to choose it. */
function DestTree({
  rootName,
  current,
  sources,
  busy,
  onChoose,
}: {
  rootName: string;
  current: string;
  sources: ReadonlySet<string>;
  busy: boolean;
  onChoose: (d: Dest) => void;
}) {
  const [open, setOpen] = useState<Set<string>>(() => new Set([""]));
  const [kids, setKids] = useState<Map<string, Entry[]>>(() => new Map());
  useEffect(() => {
    for (const id of open) {
      if (!kids.has(id)) api.subfolders(id).then((k) => setKids((m) => new Map(m).set(id, k)));
    }
  }, [open, kids]);
  const toggle = (id: string) =>
    setOpen((s) => {
      const n = new Set(s);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });
  const rows: React.ReactNode[] = [];
  const walk = (parent: string, path: string, depth: number, insideSource: boolean) => {
    for (const f of kids.get(parent) ?? []) {
      // A selected folder can't receive itself or anything inside it.
      const disabled = insideSource || sources.has(f.id);
      const loaded = kids.get(f.id);
      rows.push(
        <div key={f.id} className={`dest-row dest-node ${f.id === current ? "current" : ""}`} style={{ paddingLeft: 8 + depth * 16 }}>
          <button
            className={`twisty ${open.has(f.id) && loaded?.length !== 0 ? "open" : ""} ${loaded?.length === 0 ? "leaf" : ""}`}
            onClick={() => toggle(f.id)}
            aria-label={open.has(f.id) ? t("Collapse {name}", { name: f.name }) : t("Expand {name}", { name: f.name })}
          >
            <Icon name="chevron" size={12} />
          </button>
          <button
            className="dest-pick"
            disabled={busy || disabled}
            onClick={() => onChoose({ key: f.id, label: f.name, where: `${rootName}/${path}${f.name}` })}
            title={disabled ? t("A folder can't be moved or copied into itself") : f.private ? `${f.name} — ${t(PRIVATE_HINT)}` : f.name}
          >
            <Icon name="folder" size={14} />
            <span className="truncate">{f.name}</span>
            {f.private && <Icon name="lock" size={12} className="private-mark" />}
            {f.protected && <Icon name="shield" size={12} className="private-mark" />}
            {f.id === current && <span className="muted small">{t("current folder")}</span>}
          </button>
        </div>,
      );
      if (open.has(f.id)) walk(f.id, `${path}${f.name}/`, depth + 1, disabled);
    }
  };
  walk("", "", 1, false);
  return (
    <div className="dest-list tall">
      <div className={`dest-row dest-node ${current === "" ? "current" : ""}`} style={{ paddingLeft: 8 }}>
        <span className="twisty open" />
        <button className="dest-pick" disabled={busy} onClick={() => onChoose({ key: "", label: rootName, where: rootName })}>
          <Icon name="drive" size={14} />
          <span className="truncate">{rootName}</span>
          {current === "" && <span className="muted small">{t("current folder")}</span>}
        </button>
      </div>
      {rows}
      {!kids.has("") && <div className="dot-spinner" />}
    </div>
  );
}
