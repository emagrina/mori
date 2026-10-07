import { listen } from "@tauri-apps/api/event";
import { useEffect, useMemo, useState } from "react";
import {
  api,
  formatSize,
  plural,
  PRIVATE_HINT,
  type DestInfo,
  type Entry,
  type Resolution,
  type TransferOp,
  type TransferPlan,
  type TransferResult,
} from "../api";
import { Icon } from "./Icon";
import { ModalFrame } from "./Modal";

/** A destination: a folder of the browsed drive ("" = its root) or one picked elsewhere. */
interface Dest {
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

const verb = (op: TransferOp) => (op === "move" ? "Move" : "Copy");

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
  onClose,
  onDone,
}: {
  op: TransferOp;
  entries: Entry[];
  rootName: string;
  /** The folder being browsed (pre-selected as a starting point). */
  current: string;
  onClose: () => void;
  onDone: (result: TransferResult, plan: TransferPlan) => void;
}) {
  const [dest, setDest] = useState<Dest | null>(null);
  const [plan, setPlan] = useState<TransferPlan | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<{ done: number; total: number } | null>(null);
  const [res, setRes] = useState<Record<string, Resolution>>({});
  const what = entries.length === 1 ? `“${entries[0].name}”` : plural(entries.length, "item");
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

  if (progress) {
    return (
      <ModalFrame onCancel={() => {}}>
        <h2>
          {op === "move" ? "Moving" : "Copying"} {what}…
        </h2>
        <div className="progress transfer-progress">
          <div style={{ width: `${progress.total ? (progress.done / progress.total) * 100 : 0}%` }} />
        </div>
        <p className="muted small">
          {progress.done.toLocaleString()} of {progress.total.toLocaleString()} · to “{dest?.label}”
        </p>
        <div className="dialog-actions">
          <button className="btn" onClick={() => api.transferCancel()} title="Stops after the current item; a partly copied item is removed again">
            Stop
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
        <h2>
          {verb(op)} {what} to “{dest.label}”
        </h2>
        <p className="dialog-facts">
          {plural(ready, "item")} · {formatSize(plan.totalBytes)}
          {blocked.length > 0 && (
            <span className="attn">
              {" "}
              · {plural(blocked.length, "item")} can't be {op === "move" ? "moved" : "copied"}
            </span>
          )}
          {conflicts.length > 0 && <span className="attn"> · {plural(conflicts.length, "name conflict")}</span>}
        </p>
        {plan.crossVolume && (
          <p className="dialog-note">“{dest.label}” is on another drive: items are copied and checked, then the originals go to the Trash (never deleted).</p>
        )}
        {conflicts.length > 1 && (
          <div className="conflict-all">
            <span className="muted small">Apply to all:</span>
            <button className="btn small" onClick={() => applyAll("keepBoth")}>
              Keep Both
            </button>
            {conflicts.some((e) => e.replaceable) && (
              <button className="btn small" onClick={() => applyAll("replace")} title="Only files over files; what's replaced goes to the Trash">
                Replace
              </button>
            )}
            <button className="btn small" onClick={() => applyAll("skip")}>
              Skip
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
                  — {e.conflict === "batch" ? "another selected item has the same name" : `a ${e.conflict} with this name is already there`}
                </span>
              </span>
              <div className="segmented small" role="radiogroup" aria-label={`What to do with ${e.name}`}>
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
                          ? "The existing file goes to the Trash first (restorable)"
                          : r === "keepBoth"
                            ? "Give the new one a free name (“name 2”)"
                            : "Leave this item where it is"
                      }
                    >
                      {r === "keepBoth" ? "Keep Both" : r === "replace" ? "Replace" : "Skip"}
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
            <strong>{plural(replacing, "existing file")} will be replaced.</strong> {replacing === 1 ? "It goes" : "They go"} to the Trash first, so you can
            restore {replacing === 1 ? "it" : "them"} (or press Undo).
          </p>
        )}
        {conflicts.some((e) => !res[e.id]) && <p className="muted small">Conflicts left undecided are skipped. Nothing is overwritten.</p>}
        {error && <p className="field-error">{error}</p>}
        <div className="dialog-actions">
          <button className="btn" onClick={() => (setPlan(null), setDest(null))} disabled={busy}>
            Back
          </button>
          <button className="btn" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button className="btn primary" disabled={busy || !ready} onClick={() => run(plan, dest, res)} autoFocus>
            {replacing
              ? `Replace ${replacing} and ${verb(op)}`
              : `${verb(op)} ${plural(ready - conflicts.filter((e) => !res[e.id] || res[e.id] === "skip").length, "item")}`}
          </button>
        </div>
      </ModalFrame>
    );
  }

  return (
    <ModalFrame onCancel={() => !busy && onClose()} wide>
      <h2>
        {verb(op)} {what} to…
      </h2>
      {recent.length > 0 && (
        <>
          <div className="menu-label">Recent</div>
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
      <div className="menu-label">Folders</div>
      <DestTree rootName={rootName} current={current} sources={sources} busy={busy} onChoose={choose} />
      {error && <p className="field-error">{error}</p>}
      <div className="dialog-actions">
        <button className="btn" onClick={other} disabled={busy} title="A folder outside the one you're browsing, chosen in the system's folder picker">
          Other Folder…
        </button>
        <span className="spacer" />
        <button className="btn" onClick={onClose} disabled={busy}>
          Cancel
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
            aria-label={open.has(f.id) ? `Collapse ${f.name}` : `Expand ${f.name}`}
          >
            <Icon name="chevron" size={12} />
          </button>
          <button
            className="dest-pick"
            disabled={busy || disabled}
            onClick={() => onChoose({ key: f.id, label: f.name, where: `${rootName}/${path}${f.name}` })}
            title={disabled ? "A folder can't be moved or copied into itself" : f.private ? `${f.name} — ${PRIVATE_HINT}` : f.name}
          >
            <Icon name="folder" size={14} />
            <span className="truncate">{f.name}</span>
            {f.private && <Icon name="lock" size={12} className="private-mark" />}
            {f.protected && <Icon name="shield" size={12} className="private-mark" />}
            {f.id === current && <span className="muted small">current folder</span>}
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
          {current === "" && <span className="muted small">current folder</span>}
        </button>
      </div>
      {rows}
      {!kids.has("") && <div className="dot-spinner" />}
    </div>
  );
}
