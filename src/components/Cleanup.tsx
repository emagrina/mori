import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  formatDate,
  formatSize,
  isMac,
  parentOf,
  previewUrl,
  typeLabel,
  type Entry,
  type Inspection,
  type OperationPlan,
  type SortKey,
  type Status,
  type TrashResult,
} from "../api";
import {
  counts,
  createSession,
  current,
  decide,
  finished,
  fromSaved,
  go,
  jump,
  keep,
  markedItems,
  mayPersist,
  mayTrash,
  orderToSort,
  reconcile,
  toSaved,
  trashIds,
  undo,
  type CleanupKind,
  type CleanupOptions,
  type CleanupOrder,
  type CleanupState,
  type SavedSession,
} from "../cleanup";
import { click, EMPTY_SELECTION, selectAll, selectedItems, type Selection } from "../selection";
import { fmtNum, msg, t, tn, tx } from "../i18n";
import { Icon, type IconName } from "./Icon";
import { ModalFrame } from "./Modal";
import { PreviewBody } from "./Preview";
import { Thumb } from "./Thumb";

const KINDS: { kind: CleanupKind; label: string; icon: IconName }[] = [
  { kind: "all", label: msg("All"), icon: "all" },
  { kind: "photo", label: msg("Photos"), icon: "photo" },
  { kind: "video", label: msg("Videos"), icon: "video" },
  { kind: "gif", label: msg("GIFs"), icon: "gif" },
  { kind: "document", label: msg("Documents"), icon: "document" },
  { kind: "audio", label: msg("Audio"), icon: "audio" },
  { kind: "other", label: msg("Other"), icon: "other" },
];

const ORDERS: { order: CleanupOrder; label: string }[] = [
  { order: "browser", label: msg("Current sort order") },
  { order: "oldest", label: msg("Oldest first") },
  { order: "newest", label: msg("Newest first") },
  { order: "largest", label: msg("Largest first") },
  { order: "smallest", label: msg("Smallest first") },
];

/** Photos preloaded ahead (worker-rendered copies only; never during Safe Inspection). */
const PRELOAD = 2;

type Stage = "setup" | "review" | "marked" | "done";

interface Props {
  folder: { id: string; name: string };
  status: Status;
  /** The saved Read-only Mode setting. */
  readOnly: boolean;
  browserSort: { sort: SortKey; desc: boolean };
  recursiveDefault: boolean;
  /** Bumped when the index changes (files moved, trashed, rescanned). */
  indexVersion: number;
  onClose: () => void;
  onToast: (msg: string, ms?: number) => void;
  /** After the final, confirmed Move to Trash. */
  onTrashed: (r: TrashResult) => void;
  onUndo: () => void;
}

/**
 * Quick Cleanup: Keep / Mark for Trash, one file at a time, keyboard first.
 * Decisions are staged in memory (cleanup.ts) and nothing on disk changes
 * until the review screen's single confirmation, which moves the marked
 * files to the OS Trash through the existing `trash_items` command.
 */
export function QuickCleanup(p: Props) {
  const [stage, setStage] = useState<Stage>("setup");
  const [options, setOptions] = useState<CleanupOptions>({ recursive: p.recursiveDefault, kind: "all", order: "browser" });
  const [preview, setPreview] = useState<{ items: Entry[]; truncated: boolean } | null>(null);
  const [saved, setSaved] = useState<SavedSession | null>(null);
  const [s, setS] = useState<CleanupState<Entry> | null>(null);
  const [leaving, setLeaving] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [result, setResult] = useState<TrashResult | null>(null);
  const [pulse, setPulse] = useState<{ kind: "keep" | "trash"; n: number } | null>(null);
  const persist = mayPersist(p.status);
  const canTrash = mayTrash(p.status, p.readOnly);
  const optionsRef = useRef(options);
  optionsRef.current = options;

  const load = useCallback(
    async (o: CleanupOptions) => {
      const { sort, desc } = orderToSort(o.order, p.browserSort);
      // The browser's own query: same traversal, same private-folder boundaries.
      const r = await api.query({
        scope: "folder",
        folder: p.folder.id,
        kind: o.kind,
        search: "",
        sort: sort as SortKey,
        desc,
        recursive: o.recursive,
        global: false,
      });
      return { items: r.items.filter((e) => e.kind !== "folder" && e.kind !== "link"), truncated: r.truncated };
    },
    [p.folder.id, p.browserSort.sort, p.browserSort.desc],
  );

  // Setup: how many files the options cover.
  useEffect(() => {
    if (stage !== "setup") return;
    let alive = true;
    load(options).then(
      (r) => alive && setPreview(r),
      () => alive && setPreview({ items: [], truncated: false }),
    );
    return () => {
      alive = false;
    };
  }, [stage, options, load, p.indexVersion]);

  // An unfinished session for this folder (normal mode only).
  useEffect(() => {
    if (persist) api.cleanupSaved(p.folder.id).then(setSaved, () => setSaved(null));
  }, [persist, p.folder.id]);

  // The folder changed underneath: keep decisions for what's still there.
  const firstVersion = useRef(p.indexVersion);
  useEffect(() => {
    if (p.indexVersion === firstVersion.current || stage === "setup" || stage === "done") return;
    let alive = true;
    load(optionsRef.current).then((r) => alive && setS((cur) => cur && reconcile(cur, r.items)));
    return () => {
      alive = false;
    };
  }, [p.indexVersion]);

  // Save decisions to resume later — ids only, never in a temporary session.
  useEffect(() => {
    if (!persist || !s || !s.decisions.size || (stage !== "review" && stage !== "marked")) return;
    const timer = window.setTimeout(() => api.cleanupSave(toSaved(s, p.folder.id, optionsRef.current)).catch(() => {}), 500);
    return () => window.clearTimeout(timer);
  }, [persist, s?.decisions, s?.cursor, stage]);

  const start = async (resume: SavedSession | null) => {
    const o = resume ? resume.options : options;
    const r = await load(o);
    setOptions(o);
    if (!resume && saved) {
      api.cleanupDiscard(p.folder.id).catch(() => {});
      setSaved(null);
    }
    setS(resume ? fromSaved(r.items, resume) : createSession(r.items));
    setStage("review");
  };

  const discardSaved = () => {
    api.cleanupDiscard(p.folder.id).catch(() => {});
    setSaved(null);
  };

  const close = () => {
    // In a temporary session, decisions only live here: confirm before losing them.
    if (!persist && s?.decisions.size && stage !== "done") setLeaving(true);
    else p.onClose();
  };

  // The latest state, updated synchronously: two key presses arriving before
  // React re-renders still decide two different items, never one twice.
  const latest = useRef(s);
  latest.current = s;
  const act = (d: "keep" | "trash", repeat = false) => {
    const cur = latest.current;
    if (!cur) return;
    const r = decide(cur, d, performance.now(), { repeat });
    if (r.refused === "protected") p.onToast(t("Protected (Never Modify): Mori won't move this file to the Trash."), 3000);
    if (r.refused) return;
    latest.current = r.state;
    setS(r.state);
    setPulse((x) => ({ kind: d, n: (x?.n ?? 0) + 1 }));
  };

  // ------------------------------------------------------------ keyboard

  const videoRef = useRef<HTMLVideoElement>(null);
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  useEffect(() => {
    setZoom(1);
    setPan({ x: 0, y: 0 });
  }, [s?.cursor]);
  const zoomBy = (f: number) =>
    setZoom((z) => {
      const nz = Math.min(8, Math.max(1, z * f));
      if (nz === 1) setPan({ x: 0, y: 0 });
      return nz;
    });

  const reviewing = stage === "review" && s && !finished(s);
  useEffect(() => {
    if (!reviewing || confirm || leaving) return;
    const onKey = (e: KeyboardEvent) => {
      const el = e.target as HTMLElement;
      if (el.tagName === "INPUT" || el.tagName === "SELECT" || el.tagName === "TEXTAREA") return;
      const mod = isMac ? e.metaKey : e.ctrlKey;
      const k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
      if (mod && k === "z" && !e.shiftKey) {
        e.preventDefault();
        if (!e.repeat) setS((cur) => cur && undo(cur));
        return;
      }
      if (mod && (k === "=" || k === "+")) return (e.preventDefault(), zoomBy(1.25));
      if (mod && k === "-") return (e.preventDefault(), zoomBy(0.8));
      if (mod && k === "0") return (e.preventDefault(), setZoom(1), setPan({ x: 0, y: 0 }));
      // Never act on a key combined with a modifier (⌘⌫ must not mark anything).
      if (mod || e.altKey) return;
      switch (k) {
        case "ArrowRight":
        case "k":
          e.preventDefault();
          return act("keep", e.repeat);
        case "ArrowLeft":
        case "d":
        case "Backspace":
        case "Delete":
          e.preventDefault();
          return act("trash", e.repeat);
        case "ArrowUp":
        case "p":
          e.preventDefault();
          return setS((cur) => cur && go(cur, -1));
        case "ArrowDown":
        case "s":
          e.preventDefault();
          return setS((cur) => cur && go(cur, 1));
        case "u":
          e.preventDefault();
          if (!e.repeat) setS((cur) => cur && undo(cur));
          return;
        case " ": {
          e.preventDefault();
          const v = videoRef.current;
          if (v) v.paused ? v.play().catch(() => {}) : v.pause();
          return;
        }
        case "z":
          e.preventDefault();
          setZoom((z) => (z > 1 ? 1 : 2));
          setPan({ x: 0, y: 0 });
          return;
        case "r":
          e.preventDefault();
          return setStage("marked");
        case "Escape":
          e.preventDefault();
          return close();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  // Setup and summary keys.
  useEffect(() => {
    if (reviewing || confirm || leaving) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        if (stage === "marked" && s && !finished(s)) setStage("review");
        else close();
      } else if (e.key === "Enter" && stage === "setup" && preview?.items.length) {
        e.preventDefault();
        start(null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  // Preload the next few photos (worker-rendered copies), cancelling ones that are no longer ahead.
  const preloaded = useRef(new Map<string, HTMLImageElement>());
  useEffect(() => {
    const keepIds = new Set<string>();
    if (s && reviewing && !p.status.safeMode) {
      for (const e of s.items.slice(s.cursor + 1, s.cursor + 1 + PRELOAD)) {
        if (e.kind !== "photo" && e.kind !== "gif") continue;
        keepIds.add(e.id);
        if (!preloaded.current.has(e.id)) {
          const img = new Image();
          img.decoding = "async";
          img.src = previewUrl(e.id);
          preloaded.current.set(e.id, img);
        }
      }
    }
    for (const [id, img] of preloaded.current) {
      if (!keepIds.has(id)) {
        img.src = "";
        preloaded.current.delete(id);
      }
    }
  }, [s?.cursor, s?.items, reviewing, p.status.safeMode]);
  useEffect(
    () => () => {
      for (const img of preloaded.current.values()) img.src = "";
      preloaded.current.clear();
    },
    [],
  );

  useEffect(() => {
    if (!pulse) return;
    const timer = window.setTimeout(() => setPulse(null), 420);
    return () => window.clearTimeout(timer);
  }, [pulse]);

  // ------------------------------------------------------------- trash

  const trash = async () => {
    if (!s) return;
    setConfirm(false);
    const ids = trashIds(s);
    const r = await api.trashItems(ids).catch((e) => ({ trashed: [] as string[], bytes: 0, failed: [{ path: "", reason: String(e) }] }));
    setResult(r);
    setStage("done");
    if (persist) api.cleanupDiscard(p.folder.id).catch(() => {});
    p.onTrashed(r);
  };

  const c = s ? counts(s) : null;

  // -------------------------------------------------------------- render

  const header = (
    <header className="cleanup-bar" data-tauri-drag-region>
      <button className="icon-btn" onClick={close} title={t("Close (Esc)")} aria-label={t("Close Quick Cleanup")}>
        <Icon name="close" size={16} />
      </button>
      <div className="cleanup-title" data-tauri-drag-region>
        <div className="truncate">{t("Quick Cleanup · {folder}", { folder: p.folder.name })}</div>
        <div className="sub">
          {p.status.privateInspection
            ? t("Private Inspection: read-only, decisions stay in memory")
            : p.status.temporary
              ? t("Temporary session: decisions aren't saved")
              : t("Nothing moves until you confirm at the end")}
        </div>
      </div>
      {s && c && stage !== "setup" && stage !== "done" && (
        <>
          <div className="cleanup-progress" aria-label={t("Progress")}>
            <span className="count">
              {fmtNum(Math.min(s.cursor + 1, c.total))} / {fmtNum(c.total)}
            </span>
            <span className="bar">
              <span style={{ width: `${c.total ? ((c.kept + c.marked) / c.total) * 100 : 0}%` }} />
            </span>
          </div>
          <div className="cleanup-counts">
            <span>{t("Kept {n}", { n: c.kept })}</span>
            <span className={c.marked ? "marked" : ""}>{t("Marked {n}", { n: c.marked })}</span>
          </div>
          {stage === "review" && !finished(s) && (
            <button className="btn small" onClick={() => setStage("marked")} disabled={!c.marked} title={t("Review the items marked for Trash (R)")}>
              {t("Review Marked")} <kbd>R</kbd>
            </button>
          )}
        </>
      )}
    </header>
  );

  let body: React.ReactNode;
  if (stage === "setup") {
    const n = preview?.items.length ?? 0;
    body = (
      <div className="cleanup-setup">
        <Icon name="gallery" size={34} stroke={1.3} />
        <h1>{t("Quick Cleanup")}</h1>
        <p className="lead">
          {tx(
            "Go through {folder} one file at a time and decide {keep} or {mark}. Marking only records your decision: files stay where they are until you review the marked items and confirm moving them to the Trash.",
            { folder: <strong>{p.folder.name}</strong>, keep: <strong>{t("Keep")}</strong>, mark: <strong>{t("Mark for Trash")}</strong> },
          )}
        </p>
        {saved && (
          <div className="cleanup-resume">
            <div>
              <strong>{t("Unfinished cleanup")}</strong>
              <span className="muted small">
                {" "}
                · {tn(saved.kept.length + saved.marked.length, "{n} decision", "{n} decisions")} · {t("{n} marked", { n: saved.marked.length })}
              </span>
            </div>
            <button className="btn small" onClick={discardSaved}>
              {t("Discard")}
            </button>
            <button className="btn small primary" onClick={() => start(saved)} autoFocus>
              {t("Resume")}
            </button>
          </div>
        )}
        <div className="cleanup-options">
          <label className="toggle" title={t("Also review files in subfolders (private folders inside stay out)")}>
            <input type="checkbox" checked={options.recursive} onChange={(e) => setOptions({ ...options, recursive: e.target.checked })} />
            <span className="switch" aria-hidden />
            {t("Include subfolders")}
          </label>
          <div className="segmented small" role="radiogroup" aria-label={t("File type")}>
            {KINDS.map((k) => (
              <button
                key={k.kind}
                role="radio"
                aria-checked={options.kind === k.kind}
                className={options.kind === k.kind ? "on" : ""}
                onClick={() => setOptions({ ...options, kind: k.kind })}
                title={t(k.label)}
              >
                <Icon name={k.icon} size={13} />
                <span className="label">{t(k.label)}</span>
              </button>
            ))}
          </div>
          <label className="select-label">
            {t("Order")}
            <select value={options.order} onChange={(e) => setOptions({ ...options, order: e.target.value as CleanupOrder })}>
              {ORDERS.map((o) => (
                <option key={o.order} value={o.order}>
                  {t(o.label)}
                </option>
              ))}
            </select>
          </label>
        </div>
        <p className="muted small">
          {preview === null ? t("Counting…") : n ? tn(n, "{n} file to review", "{n} files to review") : t("No files to review with these options.")}
          {preview?.truncated && <> {t("(only the first {n})", { n: 50000 })}</>}
        </p>
        {!canTrash && (
          <p className="dialog-note">
            {p.status.privateInspection
              ? t("Private Inspection is read-only: you can review and mark files, but moving them to the Trash is unavailable in this session.")
              : t("Read-only Mode is on: you can review and mark files, but moving them to the Trash is unavailable until it's turned off.")}
          </p>
        )}
        <div className="dialog-actions center">
          <button className="btn" onClick={p.onClose}>
            {t("Cancel")}
          </button>
          <button className={`btn ${saved ? "" : "primary"}`} disabled={!n} onClick={() => start(null)} autoFocus={!saved}>
            {saved ? t("Start Over") : t("Start")} <kbd>↩</kbd>
          </button>
        </div>
        <Keys />
      </div>
    );
  } else if (stage === "done" && result) {
    const failed = result.failed.length;
    body = (
      <div className="cleanup-setup">
        <Icon name={result.trashed.length ? "trash" : "warning"} size={34} stroke={1.3} />
        <h1>{result.trashed.length ? tn(result.trashed.length, "Moved {n} item to the Trash", "Moved {n} items to the Trash") : t("Nothing was moved to the Trash")}</h1>
        <p className="lead">
          {result.trashed.length > 0 && <>{t("{size} · they can be restored from the Trash.", { size: formatSize(result.bytes) })} </>}
          {t("Kept and undecided files were left untouched.")}
        </p>
        {failed > 0 && (
          <>
            <p className="attn">{tn(failed, "{n} item couldn't be moved:", "{n} items couldn't be moved:")}</p>
            <div className="op-list">
              {result.failed.slice(0, 200).map((f, i) => (
                <div key={i} className="op-row blocked">
                  <span className="truncate op-path">{f.path || "—"}</span>
                  <span className="op-note attn">{f.reason}</span>
                </div>
              ))}
            </div>
          </>
        )}
        <div className="dialog-actions center">
          {result.trashed.length > 0 && (
            <button className="btn" onClick={p.onUndo} title={t("Put the items back from the Trash")}>
              {t("Undo")} <kbd>{isMac ? "⌘Z" : "Ctrl+Z"}</kbd>
            </button>
          )}
          <button className="btn primary" onClick={p.onClose} autoFocus>
            {t("Done")}
          </button>
        </div>
      </div>
    );
  } else if (s && (stage === "marked" || finished(s))) {
    body = (
      <MarkedReview
        state={s}
        finishedAll={finished(s)}
        canTrash={canTrash}
        readOnlyNote={p.status.privateInspection ? t("Private Inspection is read-only.") : t("Read-only Mode is on.")}
        onKeep={(ids) => setS((cur) => cur && keep(cur, ids))}
        onOpen={(id) => {
          setS(
            (cur) =>
              cur &&
              jump(
                cur,
                cur.items.findIndex((e) => e.id === id),
              ),
          );
          setStage("review");
        }}
        onBack={() => {
          setS((cur) => cur && (finished(cur) ? jump(cur, cur.items.length - 1) : cur));
          setStage("review");
        }}
        onTrash={() => setConfirm(true)}
      />
    );
  } else if (s) {
    const e = current(s)!;
    const d = s.decisions.get(e.id);
    body = (
      <div className={`cleanup-review ${pulse ? `pulse-${pulse.kind}` : ""}`}>
        {/* The normal viewer's own stage (same sizing: fit the whole media), never a separate layout. */}
        <div className="preview-stage cleanup-stage">
          {(d || e.guarded) && (
            <div className="cleanup-badges">
              {d && <span className={`chip ${d === "trash" ? "danger" : ""}`}>{d === "trash" ? t("Marked for Trash") : t("Kept")}</span>}
              {e.guarded && (
                <span className="chip" title={t("Inside a protected folder: Mori won't change it")}>
                  <Icon name="shield" size={11} /> {t("Never Modify")}
                </span>
              )}
            </div>
          )}
          <MediaStage
            key={e.id}
            entry={e}
            iso={p.status.safeMode}
            zoom={zoom}
            pan={pan}
            setPan={setPan}
            zoomBy={zoomBy}
            setZoom={setZoom}
            videoRef={videoRef}
          />
          {pulse && (
            <div className={`cleanup-pulse ${pulse.kind}`} key={pulse.n}>
              {pulse.kind === "trash" ? t("Marked for Trash") : t("Kept")}
            </div>
          )}
        </div>
        <footer className="cleanup-info">
          <span className="truncate name">{e.name}</span>
          <span>{typeLabel(e)}</span>
          <span>{formatSize(e.size)}</span>
          <span>{formatDate(e.modified)}</span>
          <span className="truncate">{parentOf(e.path) || "/"}</span>
        </footer>
        <div className="cleanup-actions">
          <button className="decide trash" onClick={() => act("trash")} disabled={!!e.guarded} title={t("Mark for Trash (← or D). Nothing is moved yet.")}>
            <Icon name="left" size={15} /> {t("Mark for Trash")} <kbd>D</kbd>
          </button>
          <div className="cleanup-minor">
            <button
              className="btn small ghost"
              onClick={() => setS((cur) => cur && undo(cur))}
              disabled={!s.steps.length}
              title={t("Undo last decision (U or {key})", { key: isMac ? "⌘Z" : "Ctrl+Z" })}
            >
              {t("Undo")} <kbd>U</kbd>
            </button>
            <button className="btn small ghost" onClick={() => setS((cur) => cur && go(cur, -1))} disabled={s.cursor === 0} title={t("Previous item (↑ or P)")}>
              {t("Back")} <kbd>↑</kbd>
            </button>
            <button className="btn small ghost" onClick={() => setS((cur) => cur && go(cur, 1))} title={t("Next item without deciding (↓ or S)")}>
              {t("Skip")} <kbd>↓</kbd>
            </button>
          </div>
          <button className="decide keep" onClick={() => act("keep")} title={t("Keep (→ or K)")}>
            {t("Keep")} <kbd>K</kbd> <Icon name="right" size={15} />
          </button>
        </div>
      </div>
    );
  }

  return (
    <div className="cleanup" role="dialog" aria-label={t("Quick Cleanup of {folder}", { folder: p.folder.name })}>
      {header}
      {body}
      {confirm && s && <ConfirmTrash ids={trashIds(s)} bytes={counts(s).markedBytes} onCancel={() => setConfirm(false)} onConfirm={trash} />}
      {leaving && (
        <ModalFrame onCancel={() => setLeaving(false)}>
          <h2>{t("Leave Quick Cleanup?")}</h2>
          <p>
            {tn(
              s?.decisions.size ?? 0,
              "This is a temporary session, so your {n} decision isn't saved and will be lost. No files were changed.",
              "This is a temporary session, so your {n} decisions aren't saved and will be lost. No files were changed.",
            )}
          </p>
          <div className="dialog-actions">
            <button className="btn" onClick={() => setLeaving(false)} autoFocus>
              {t("Stay")}
            </button>
            <button className="btn primary" onClick={p.onClose}>
              {t("Leave")}
            </button>
          </div>
        </ModalFrame>
      )}
    </div>
  );
}

/** One file, through the same safe preview paths as Mori's preview. */
function MediaStage({
  entry,
  iso,
  zoom,
  pan,
  setPan,
  zoomBy,
  setZoom,
  videoRef,
}: {
  entry: Entry;
  iso: boolean;
  zoom: number;
  pan: { x: number; y: number };
  setPan: (p: { x: number; y: number }) => void;
  zoomBy: (f: number) => void;
  setZoom: (z: number) => void;
  videoRef: React.RefObject<HTMLVideoElement | null>;
}) {
  const [info, setInfo] = useState<Inspection | null>(null);
  useEffect(() => {
    let alive = true;
    api.inspect(entry.id).then(
      (i) => alive && setInfo(i),
      () => alive && setInfo({ detected: "unknown", preview: "none", canOpen: false, mismatch: false, video: null, previewsOff: false }),
    );
    return () => {
      alive = false;
    };
  }, [entry.id]);
  if (!info) return <div className="spinner" />;
  return (
    <PreviewBody
      entry={entry}
      info={info}
      zoom={zoom}
      pan={pan}
      setPan={setPan}
      zoomBy={zoomBy}
      resetZoom={() => {
        setZoom(zoom > 1 ? 1 : 2);
        setPan({ x: 0, y: 0 });
      }}
      onDims={() => {}}
      videoRef={videoRef}
      iso={iso || info.previewsOff}
    />
  );
}

/** Every item marked for Trash, before anything happens to it. */
function MarkedReview({
  state,
  finishedAll,
  canTrash,
  readOnlyNote,
  onKeep,
  onOpen,
  onBack,
  onTrash,
}: {
  state: CleanupState<Entry>;
  finishedAll: boolean;
  canTrash: boolean;
  readOnlyNote: string;
  onKeep: (ids?: string[]) => void;
  onOpen: (id: string) => void;
  onBack: () => void;
  onTrash: () => void;
}) {
  const marked = useMemo(() => markedItems(state), [state]);
  const order = useMemo(() => marked.map((e) => e.id), [marked]);
  const [sel, setSel] = useState<Selection>(EMPTY_SELECTION);
  const c = counts(state);
  const chosen = selectedItems(sel, marked);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((isMac ? e.metaKey : e.ctrlKey) && e.key.toLowerCase() === "a") {
        e.preventDefault();
        setSel((cur) => selectAll(cur, order));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [order]);
  return (
    <div className="cleanup-marked">
      <div className="marked-head">
        <div>
          <h1>{finishedAll ? t("All files reviewed") : t("Marked for Trash")}</h1>
          <p className="muted">
            {finishedAll && `${t("Kept {n}", { n: c.kept })} · ${c.undecided ? `${t("{n} undecided", { n: c.undecided })} · ` : ""}`}
            <strong>
              {tn(c.marked, "{n} item marked", "{n} items marked")} · {formatSize(c.markedBytes)}
            </strong>
            {" · "}
            {t("Nothing has been moved yet.")}
          </p>
        </div>
        <div className="marked-actions">
          <button
            className="btn"
            onClick={() => (onKeep(chosen.map((e) => e.id)), setSel(EMPTY_SELECTION))}
            disabled={!chosen.length}
            title={t("Restore the selected items to Keep")}
          >
            {chosen.length ? t("Keep Selected ({n})", { n: chosen.length }) : t("Keep Selected")}
          </button>
          <button className="btn" onClick={() => onKeep()} disabled={!c.marked}>
            {t("Keep All")}
          </button>
          <button className="btn" onClick={onBack}>
            {finishedAll ? t("Back to Last File") : t("Back to Cleanup")}
          </button>
          <button
            className="btn primary danger-fill"
            onClick={onTrash}
            disabled={!c.marked || !canTrash}
            title={canTrash ? t("Move the marked items to the system Trash (one confirmation first)") : t("{note} Files can't be moved to the Trash.", { note: readOnlyNote })}
          >
            {tn(c.marked, "Move {n} Item to Trash", "Move {n} Items to Trash")}
          </button>
        </div>
      </div>
      {!canTrash && <p className="dialog-note">{t("{note} You can review your decisions, but nothing can be moved to the Trash right now.", { note: readOnlyNote })}</p>}
      {marked.length ? (
        <div className="marked-grid" role="listbox" aria-multiselectable aria-label={t("Marked items")}>
          {marked.map((e) => (
            <div
              key={e.id}
              role="option"
              aria-selected={sel.ids.has(e.id)}
              className="marked-tile"
              data-selected={sel.ids.has(e.id) || undefined}
              title={t("{name} — double-click to look at it again", { name: e.name })}
              onClick={(ev) => setSel((cur) => click(cur, order, e.id, { toggle: isMac ? ev.metaKey : ev.ctrlKey, range: ev.shiftKey }))}
              onDoubleClick={() => onOpen(e.id)}
              onMouseDown={(ev) => ev.shiftKey && ev.preventDefault()}
            >
              <div className="frame">
                <Thumb entry={e} fit="cover" iconSize={30} />
              </div>
              <div className="name truncate">{e.name}</div>
              <div className="meta">{formatSize(e.size)}</div>
            </div>
          ))}
        </div>
      ) : (
        <p className="muted marked-empty">{t("No items are marked for Trash.")}</p>
      )}
    </div>
  );
}

/** The one confirmation before anything changes on disk. */
function ConfirmTrash({ ids, bytes, onCancel, onConfirm }: { ids: string[]; bytes: number; onCancel: () => void; onConfirm: () => void }) {
  const [plan, setPlan] = useState<OperationPlan | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    // The backend's own dry run: what its policy will refuse (protected, gone…).
    api.planOperation("trash", ids).then(setPlan, (e) => setError(String(e)));
  }, []);
  const blocked = plan?.entries.filter((e) => e.blocked) ?? [];
  const n = ids.length - blocked.length;
  return (
    <ModalFrame onCancel={onCancel}>
      <div className="dialog-icon">
        <Icon name="trash" size={20} />
      </div>
      <h2>{tn(ids.length, "Move {n} item to the Trash?", "Move {n} items to the Trash?")}</h2>
      <p>
        {t("{size}. They go to the system Trash and can be restored from there (or with Undo, {key}). Nothing is deleted permanently.", {
          size: formatSize(bytes),
          key: isMac ? "⌘Z" : "Ctrl+Z",
        })}
      </p>
      {blocked.length > 0 && (
        <>
          <p className="attn">{tn(blocked.length, "{n} item will stay where it is:", "{n} items will stay where they are:")}</p>
          <div className="op-list">
            {blocked.slice(0, 100).map((e) => (
              <div key={e.id} className="op-row blocked">
                <span className="truncate op-path">{e.path}</span>
                <span className="op-note attn">{e.blocked}</span>
              </div>
            ))}
          </div>
        </>
      )}
      {error && <p className="field-error">{error}</p>}
      <div className="dialog-actions">
        <button className="btn" onClick={onCancel} autoFocus>
          {t("Cancel")}
        </button>
        <button className="btn primary danger-fill" onClick={onConfirm} disabled={!plan || n <= 0}>
          {tn(Math.max(0, n), "Move {n} Item to Trash", "Move {n} Items to Trash")}
        </button>
      </div>
    </ModalFrame>
  );
}

function Keys() {
  const rows: [string, string][] = [
    ["→  K", t("Keep, next file")],
    ["←  D  ⌫", t("Mark for Trash, next file")],
    [`U  ${isMac ? "⌘Z" : "Ctrl+Z"}`, t("Undo last decision")],
    ["↑  ↓", t("Previous / next without deciding")],
    [t("Space"), t("Play / pause video")],
    ["Z", t("Zoom photo")],
    ["R", t("Review marked items")],
    [t("Esc"), t("Close (in normal sessions you can resume later)")],
  ];
  return (
    <dl className="cleanup-keys">
      {rows.map(([k, v]) => (
        <div key={k}>
          <dt>
            <kbd>{k}</kbd>
          </dt>
          <dd>{v}</dd>
        </div>
      ))}
    </dl>
  );
}
