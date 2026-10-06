import { listen } from "@tauri-apps/api/event";
import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  formatDuration,
  formatShortDate,
  formatSize,
  isMac,
  parentOf,
  plural,
  previewUrl,
  setPreviewOpen,
  type AnalysisDone,
  type CleanupOutcome,
  type Entry,
  type LocationInfo,
  type PlanItem,
  type Sensitivity,
  type SimFile,
  type SimGroup,
  type SimMember,
  type SimProgress,
  type SimView,
} from "../api";
import { FinalReview, LocationList, ProgressBar, Summary, ToolSwitch } from "./Analyzer";
import { Icon } from "./Icon";
import { ModalFrame } from "./Modal";
import { Preview } from "./Preview";
import { Thumb } from "./Thumb";

/**
 * Similar Photos & Videos: SELECT FOLDER → PHOTOS / VIDEOS → ANALYZE →
 * CANDIDATES → REVIEW → KEEP / IGNORE / NOT DUPLICATES → FINAL REVIEW → TRASH.
 * Results are estimates; nothing changes on disk before the final button,
 * and the backend refuses any plan that would leave a group without a copy.
 */

type Phase = "setup" | "running" | "results" | "review" | "cleaning" | "summary";

const SENSITIVITY: { key: Sensitivity; label: string; text: string }[] = [
  { key: "strict", label: "Strict", text: "Only extremely similar media." },
  { key: "balanced", label: "Balanced", text: "Recommended." },
  { key: "broad", label: "Broad", text: "May find edited, cropped or recompressed versions, but can produce more false positives." },
];

const STAGE: Record<SimProgress["stage"], string> = {
  collecting: "Finding photos and videos",
  photos: "Generating visual fingerprints",
  videos: "Sampling video frames",
  comparing: "Comparing candidates",
  done: "Finishing",
};

const keyOf = (m: SimMember) => m.files[0].id;
/** A group is identified by its suggested copy (stable across refreshes). */
const groupKey = (g: SimGroup) => keyOf(g.members[0]);
const simEntry = (f: SimFile): Entry => ({ id: f.id, name: f.name, path: f.path, ext: f.ext, kind: f.kind, size: f.size, modified: f.modified, created: f.created });
const PAGE = 40;

export function SimilarAnalyzer({
  active,
  version,
  onToast,
  onSwitch,
}: {
  active: boolean;
  version: number;
  onToast: (msg: string, ms?: number) => void;
  onSwitch: () => void;
}) {
  const [phase, setPhase] = useState<Phase>("setup");
  const [locations, setLocations] = useState<LocationInfo[]>([]);
  const [chosen, setChosen] = useState<Set<string>>(() => new Set(["library"]));
  const [photos, setPhotos] = useState(true);
  const [videos, setVideos] = useState(true);
  const [recursive, setRecursive] = useState(true);
  const [sensitivity, setSensitivity] = useState<Sensitivity>("balanced");
  const [dismissedCount, setDismissedCount] = useState(0);
  const [progress, setProgress] = useState<SimProgress | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [view, setView] = useState<SimView | null>(null);
  const [marks, setMarks] = useState<Set<string>>(() => new Set());
  /** Groups set aside for this session ("Ignore"): never part of a cleanup. */
  const [ignored, setIgnored] = useState<Set<string>>(() => new Set());
  const [shown, setShown] = useState(PAGE);
  const [cleanup, setCleanup] = useState<{ done: number; total: number } | null>(null);
  const [outcome, setOutcome] = useState<CleanupOutcome | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; group: SimGroup; member: number } | null>(null);
  const [preview, setPreview] = useState<{ items: Entry[]; index: number } | null>(null);
  const [compare, setCompare] = useState<{ group: SimGroup; a: number; b: number } | null>(null);

  const refresh = () =>
    api.similarResults().then((v) => {
      setView(v);
      if (v) {
        const keys = new Set(v.groups.flatMap((g) => g.members.map(keyOf)));
        setMarks((m) => new Set([...m].filter((k) => keys.has(k))));
      }
      return v;
    });

  useEffect(() => {
    const un = [
      listen<SimProgress>("similar-progress", (e) => setProgress(e.payload)),
      listen<AnalysisDone>("similar-done", (e) => {
        setCancelling(false);
        if (e.payload.status === "done") {
          setMarks(new Set());
          setIgnored(new Set());
          setShown(PAGE);
          refresh().then((v) => setPhase(v ? "results" : "setup"));
        } else {
          setPhase("setup");
          if (e.payload.status === "cancelled") onToast("Analysis cancelled — nothing was changed");
          else setError(e.payload.message ?? "The analysis failed.");
        }
      }),
      listen<{ done: number; total: number }>("cleanup-progress", (e) => setCleanup(e.payload)),
    ];
    refresh().then((v) => v && setPhase("results"));
    return () => un.forEach((p) => p.then((f) => f()));
  }, []);

  useEffect(() => {
    if (active && phase === "setup") {
      api.analysisLocations().then(setLocations);
      api.similarDismissedCount().then(setDismissedCount);
    }
  }, [active, phase]);

  useEffect(() => {
    if (version && view) refresh();
  }, [version]);

  useEffect(() => setPreviewOpen(preview !== null), [preview]);

  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    window.addEventListener("click", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("blur", close);
    };
  }, [menu]);

  const groups = view?.groups ?? [];
  const live = groups.filter((g) => !ignored.has(groupKey(g)));

  // ------------------------------------------------------------ selection

  const update = (fn: (n: Set<string>) => void) =>
    setMarks((m) => {
      const n = new Set(m);
      fn(n);
      return n;
    });
  const marked = (g: SimGroup) => g.members.filter((m) => marks.has(keyOf(m))).length;
  const keepAll = (g: SimGroup) => update((n) => g.members.forEach((m) => n.delete(keyOf(m))));
  const keepOnly = (g: SimGroup, keep: number) => update((n) => g.members.forEach((m, i) => (i === keep ? n.delete(keyOf(m)) : n.add(keyOf(m)))));
  const keepCopy = (g: SimGroup, i: number) => update((n) => n.delete(keyOf(g.members[i])));
  const selectForTrash = (g: SimGroup, i: number) => {
    if (marks.has(keyOf(g.members[i]))) return;
    if (marked(g) + 1 >= g.members.length) return onToast("At least one copy must be kept.", 3000);
    update((n) => n.add(keyOf(g.members[i])));
  };
  const toggle = (g: SimGroup, i: number) => (marks.has(keyOf(g.members[i])) ? keepCopy(g, i) : selectForTrash(g, i));
  const ignore = (g: SimGroup, on: boolean) => {
    keepAll(g);
    setIgnored((s) => {
      const n = new Set(s);
      if (on) n.add(groupKey(g));
      else n.delete(groupKey(g));
      return n;
    });
  };
  const notDuplicates = async (g: SimGroup, member: number | null) => {
    keepAll(g);
    await api.similarDismiss(g.index, member).catch((e) => onToast(String(e)));
    await refresh();
    onToast(member === null ? "Marked as not duplicates — Mori won't suggest these again" : "Removed from this group — Mori won't suggest this match again", 3500);
  };
  const autoSelect = () => {
    const next = new Set<string>();
    for (const g of live) g.members.forEach((m, i) => i > 0 && next.add(keyOf(m)));
    setMarks(next);
    setPhase("review");
  };

  const plan = useMemo(() => {
    const items: PlanItem[] = [];
    const trash: SimFile[] = [];
    let keep = 0;
    let bytes = 0;
    let invalid = false;
    for (const g of live) {
      const idx = g.members.flatMap((m, i) => (marks.has(keyOf(m)) ? [i] : []));
      if (!idx.length) continue;
      if (idx.length >= g.members.length) invalid = true;
      items.push({ group: g.index, trash: idx });
      g.members.forEach((m, i) => {
        if (idx.includes(i)) {
          trash.push(...m.files);
          bytes += m.files.reduce((n, f) => n + f.size, 0);
        } else keep += m.files.length;
      });
    }
    return { items, trash, keep, bytes, invalid };
  }, [groups, marks, ignored]);

  // -------------------------------------------------------------- actions

  const start = async () => {
    setError(null);
    setProgress(null);
    setOutcome(null);
    try {
      await api.similarStart([...chosen], photos, videos, recursive, sensitivity);
      setPhase("running");
    } catch (e) {
      setError(String(e));
    }
  };

  const clear = async () => {
    await api.similarClear();
    setView(null);
    setMarks(new Set());
    setIgnored(new Set());
    setOutcome(null);
    setPhase("setup");
    onToast("Analysis cleared");
  };

  const runCleanup = async () => {
    if (plan.invalid || !plan.items.length) return;
    setCleanup({ done: 0, total: plan.trash.length });
    setCancelling(false);
    setPhase("cleaning");
    try {
      setOutcome(await api.similarCleanup(plan.items));
    } catch (e) {
      setOutcome({ trashedFiles: 0, trashedBytes: 0, keptFiles: 0, groupsCleaned: 0, groupsSkipped: 0, failures: [{ path: "", reason: String(e) }], cancelled: false });
    }
    setMarks(new Set());
    await refresh();
    setPhase("summary");
  };

  const copyPath = async (id: string) => {
    const text = await api.copyPath(id).catch(() => null);
    if (!text) return onToast("Path unavailable");
    await navigator.clipboard.writeText(text).catch(() => {});
    onToast("Path copied");
  };

  const openPreview = (g: SimGroup, f: SimFile) => {
    const items = g.members.map((m) => simEntry(m.files[0]));
    setPreview({ items, index: Math.max(0, items.findIndex((e) => e.id === f.id)) });
  };

  if (!active) return null;

  const photoGroups = groups.filter((g) => !g.video);
  const videoGroups = groups.filter((g) => g.video);
  const recoverable = live.reduce((n, g) => n + g.recoverable, 0);
  const st = view?.stats;
  const card = (g: SimGroup) => (
    <SimGroupCard
      key={groupKey(g)}
      group={g}
      marks={marks}
      ignored={ignored.has(groupKey(g))}
      onKeepAll={() => keepAll(g)}
      onKeepOnly={(i) => keepOnly(g, i)}
      onToggle={(i) => toggle(g, i)}
      onIgnore={(on) => ignore(g, on)}
      onNotDuplicates={() => notDuplicates(g, null)}
      onPreview={(f) => openPreview(g, f)}
      onCompare={(b) => setCompare({ group: g, a: 0, b })}
      onMenu={(ev, i) => {
        ev.preventDefault();
        ev.stopPropagation();
        setMenu({ x: ev.clientX, y: ev.clientY, group: g, member: i });
      }}
    />
  );

  return (
    <main className="content analyzer">
      <header className="topbar" data-tauri-drag-region>
        <div className="spacer" data-tauri-drag-region />
      </header>
      <div className="page-head">
        <div className="crumbs">
          <span className="crumb">Analyze</span>
        </div>
        <div className="title-row">
          <h1 className="page-title">
            Similar Media
            {phase !== "setup" && phase !== "running" && view && <span className="scope-note">in {view.locations.map((l) => l.label).join(", ")}</span>}
          </h1>
          {(phase === "results" || phase === "summary") && view && (
            <>
              <button className="btn" onClick={clear} title="Forget these results (they are only kept in memory)">
                Clear Analysis
              </button>
              <button className="btn" onClick={() => setPhase("setup")}>
                New Analysis
              </button>
            </>
          )}
          {phase === "results" && live.length > 0 && (
            <button className="btn primary" onClick={autoSelect} title="Keep the suggested copy in every group (except ignored ones) and review the rest">
              Auto-select
            </button>
          )}
        </div>
      </div>

      <div className="analyzer-body">
        {phase === "setup" && (
          <div className="setup">
            <ToolSwitch current="similar" onSwitch={onSwitch} />
            <p className="lead">
              Finds photos and videos that look like the same picture or clip even though their files differ — another format, size, compression or
              metadata. These are <strong>estimates</strong>: review every group before cleaning. For files that are identical byte for byte, use Exact
              Duplicates.
            </p>
            <section>
              <h3>Where to look</h3>
              <LocationList
                locations={locations}
                chosen={chosen}
                setChosen={setChosen}
                onAddFolder={async () => {
                  const loc = await api.analysisChooseFolder().catch(() => null);
                  if (!loc) return;
                  setLocations((ls) => (ls.some((l) => l.key === loc.key) ? ls : [...ls, loc]));
                  setChosen((c) => new Set(c).add(loc.key));
                }}
              />
            </section>
            <section>
              <h3>Media</h3>
              <div className="chips">
                <button className={`chip ${photos ? "on" : ""}`} onClick={() => setPhotos((v) => !v)} aria-pressed={photos}>
                  <Icon name="photo" size={14} /> Photos
                </button>
                <button className={`chip ${videos ? "on" : ""}`} onClick={() => setVideos((v) => !v)} aria-pressed={videos}>
                  <Icon name="video" size={14} /> Videos
                </button>
              </div>
              <label className="toggle">
                <input type="checkbox" checked={recursive} onChange={(e) => setRecursive(e.target.checked)} />
                <span className="switch" aria-hidden />
                Include subfolders
              </label>
            </section>
            <section>
              <h3>Similarity</h3>
              <div className="segmented sensitivity" role="radiogroup" aria-label="Similarity">
                {SENSITIVITY.map((s) => (
                  <button key={s.key} className={sensitivity === s.key ? "on" : ""} onClick={() => setSensitivity(s.key)} role="radio" aria-checked={sensitivity === s.key}>
                    {s.label}
                  </button>
                ))}
              </div>
              <p className="hint-line">{SENSITIVITY.find((s) => s.key === sensitivity)!.text}</p>
            </section>
            {error && <p className="field-error">{error}</p>}
            <div className="setup-actions">
              {view && (
                <button className="btn" onClick={() => setPhase("results")}>
                  Back to Results
                </button>
              )}
              <button className="btn primary large" disabled={!locations.some((l) => chosen.has(l.key)) || (!photos && !videos)} onClick={start}>
                Analyze
              </button>
            </div>
            <p className="fineprint">
              Everything happens on this computer: photos are decoded in Mori's sandboxed worker and videos are sampled at 12 points by the system video
              engine, then reduced to small grayscale fingerprints. Fingerprints are cached locally (removed by Clear Cache); nothing is uploaded.
              {dismissedCount > 0 && (
                <>
                  {" "}
                  {plural(dismissedCount, "match", "matches")} you marked as not duplicates won't be suggested again.{" "}
                  <button
                    className="link"
                    onClick={() =>
                      api.similarForgetDecisions().then(() => {
                        setDismissedCount(0);
                        onToast("Not-duplicate decisions forgotten");
                      })
                    }
                  >
                    Forget these decisions
                  </button>
                </>
              )}
            </p>
          </div>
        )}

        {phase === "running" && (
          <div className="analyzer-center">
            <div className="progress-card">
              <h2>{progress ? STAGE[progress.stage] : "Starting"}…</h2>
              <ProgressBar
                value={
                  !progress
                    ? 0
                    : progress.stage === "photos"
                      ? progress.photosDone / Math.max(1, progress.photosTotal)
                      : progress.stage === "videos"
                        ? progress.videosDone / Math.max(1, progress.videosTotal)
                        : progress.stage === "comparing"
                          ? progress.compareDone / Math.max(1, progress.compareTotal)
                          : 0
                }
                indeterminate={!progress || progress.stage === "collecting"}
              />
              <dl className="facts">
                <div>
                  <dt>Photos</dt>
                  <dd>{progress && progress.stage !== "collecting" ? `${progress.photosDone.toLocaleString()} of ${progress.photosTotal.toLocaleString()}` : "—"}</dd>
                </div>
                <div>
                  <dt>Videos</dt>
                  <dd>{progress && progress.stage !== "collecting" ? `${progress.videosDone.toLocaleString()} of ${progress.videosTotal.toLocaleString()}` : "—"}</dd>
                </div>
                <div>
                  <dt>Comparing</dt>
                  <dd>{progress?.stage === "comparing" ? `${Math.round((100 * progress.compareDone) / Math.max(1, progress.compareTotal))}%` : "—"}</dd>
                </div>
                <div>
                  <dt>Similar groups</dt>
                  <dd>{progress?.stage === "done" ? progress.groups.toLocaleString() : "—"}</dd>
                </div>
              </dl>
              <div className="dialog-actions">
                <button
                  className="btn"
                  disabled={cancelling}
                  onClick={() => {
                    setCancelling(true);
                    api.similarCancel();
                  }}
                >
                  {cancelling ? "Cancelling…" : "Cancel"}
                </button>
              </div>
            </div>
          </div>
        )}

        {phase === "results" && view && (
          <>
            <div className="dup-summary">
              {groups.length ? (
                <>
                  <strong>{plural(groups.length, "similar group")}</strong>
                  <span>{formatSize(recoverable)} potentially recoverable</span>
                  {ignored.size > 0 && <span>{plural(ignored.size, "group")} ignored</span>}
                </>
              ) : (
                <strong>No similar media found</strong>
              )}
              {st && (
                <span className="muted">
                  {plural(st.photos, "photo")} and {plural(st.videos, "video")} analyzed
                  {st.unanalyzable > 0 && ` · ${plural(st.unanalyzable, "file")} could not be safely analyzed`}
                  {st.dismissed > 0 && ` · ${plural(st.dismissed, "match", "matches")} hidden (not duplicates)`}
                </span>
              )}
            </div>
            {groups.length === 0 ? (
              <div className="empty">
                <Icon name="check" size={34} stroke={1.3} />
                <p>Mori found no photos or videos that look like copies of each other here.</p>
              </div>
            ) : (
              <div className="dup-list">
                {groups.slice(0, shown).map((g, i, list) => (
                  <Fragment key={groupKey(g)}>
                    {(i === 0 || list[i - 1].video !== g.video) && (
                      <h3 className="list-heading">
                        {g.video ? `Similar videos · ${videoGroups.length}` : `Similar photos · ${photoGroups.length}`}
                      </h3>
                    )}
                    {card(g)}
                  </Fragment>
                ))}
                {shown < groups.length && (
                  <button className="btn more" onClick={() => setShown((n) => n + PAGE)}>
                    Show more ({(groups.length - shown).toLocaleString()} groups left)
                  </button>
                )}
              </div>
            )}
            {plan.items.length > 0 && (
              <div className="selection-bar">
                <span>
                  <strong>{plural(plan.trash.length, "file")}</strong> selected for Trash · {formatSize(plan.bytes)}
                </span>
                <div className="spacer" />
                <button className="btn" onClick={() => setMarks(new Set())}>
                  Keep All
                </button>
                <button className="btn primary" onClick={() => setPhase("review")}>
                  Review…
                </button>
              </div>
            )}
          </>
        )}

        {phase === "cleaning" && cleanup && (
          <div className="analyzer-center">
            <div className="progress-card">
              <h2>Moving selected files to Trash…</h2>
              <ProgressBar value={cleanup.total ? cleanup.done / cleanup.total : 0} />
              <p className="muted">
                {cleanup.done.toLocaleString()} of {plural(cleanup.total, "file")} · each kept copy is re-checked first
              </p>
              <div className="dialog-actions">
                <button
                  className="btn"
                  disabled={cancelling}
                  onClick={() => {
                    setCancelling(true);
                    api.analysisCleanupCancel();
                  }}
                >
                  {cancelling ? "Stopping…" : "Stop"}
                </button>
              </div>
            </div>
          </div>
        )}

        {phase === "summary" && outcome && <Summary outcome={outcome} remaining={groups.length} onDone={() => setPhase(groups.length ? "results" : "setup")} />}
      </div>

      {phase === "review" && (
        <FinalReview
          title="Similar media cleanup"
          confirmLabel="Move Selected Files to Trash"
          plan={plan}
          reviewed={groups.length}
          ignored={ignored.size}
          onCancel={() => {
            setMarks(new Set());
            setPhase("results");
          }}
          onReview={() => setPhase("results")}
          onConfirm={runCleanup}
        />
      )}

      {menu && (
        <div className="menu" style={{ left: Math.min(menu.x, window.innerWidth - 230), top: Math.min(menu.y, window.innerHeight - 250) }} onContextMenu={(e) => e.preventDefault()}>
          <button onClick={() => openPreview(menu.group, menu.group.members[menu.member].files[0])}>
            <Icon name="gallery" size={14} /> Preview
          </button>
          <button onClick={() => api.revealFile(menu.group.members[menu.member].files[0].id).catch(() => onToast("File unavailable"))}>
            <Icon name="reveal" size={14} /> {isMac ? "Show in Finder" : "Show in Folder"}
          </button>
          <button onClick={() => copyPath(menu.group.members[menu.member].files[0].id)}>
            <Icon name="copy" size={14} /> Copy Path
          </button>
          <div className="sep" />
          <button onClick={() => keepCopy(menu.group, menu.member)}>
            <Icon name="keep" size={14} /> Keep This Copy
          </button>
          <button className="danger" onClick={() => selectForTrash(menu.group, menu.member)}>
            <Icon name="trash" size={14} /> Select for Trash
          </button>
          {menu.member > 0 && (
            <>
              <div className="sep" />
              <button onClick={() => notDuplicates(menu.group, menu.member)}>
                <Icon name="close" size={14} /> Not a Match
              </button>
            </>
          )}
        </div>
      )}

      {preview && (
        <Preview
          items={preview.items}
          index={preview.index}
          onIndex={(i) => setPreview((p) => p && { ...p, index: i })}
          onClose={() => setPreview(null)}
          onCopyPath={(e) => copyPath(e.id)}
          onError={(m) => onToast(m)}
        />
      )}

      {compare && <Compare group={compare.group} a={compare.a} b={compare.b} onPick={(b) => setCompare({ ...compare, b })} onClose={() => setCompare(null)} />}
    </main>
  );
}

// ---------------------------------------------------------------- group card

const resolution = (f: SimFile) => (f.width && f.height ? `${f.width} × ${f.height}` : "—");

function SimGroupCard({
  group: g,
  marks,
  ignored,
  onKeepAll,
  onKeepOnly,
  onToggle,
  onIgnore,
  onNotDuplicates,
  onPreview,
  onCompare,
  onMenu,
}: {
  group: SimGroup;
  marks: Set<string>;
  ignored: boolean;
  onKeepAll: () => void;
  onKeepOnly: (i: number) => void;
  onToggle: (i: number) => void;
  onIgnore: (on: boolean) => void;
  onNotDuplicates: () => void;
  onPreview: (f: SimFile) => void;
  onCompare: (b: number) => void;
  onMenu: (ev: React.MouseEvent, i: number) => void;
}) {
  const trashCount = g.members.filter((m) => marks.has(keyOf(m))).length;
  const allExact = g.members.slice(1).every((m) => m.exact);
  if (ignored) {
    return (
      <section className="dup-group ignored">
        <header>
          <div className="dup-title">
            <div className="truncate">{g.members[0].files[0].name}</div>
            <div className="muted">Ignored — not part of any cleanup</div>
          </div>
          <button className="btn small" onClick={() => onIgnore(false)}>
            Undo
          </button>
        </header>
      </section>
    );
  }
  return (
    <section className="dup-group">
      <header>
        <div className="pair-thumbs">
          {g.members.slice(0, 2).map((m) => (
            <button key={keyOf(m)} className="dup-thumb" onClick={() => onPreview(m.files[0])} title="Preview">
              <Thumb entry={simEntry(m.files[0])} fit="cover" iconSize={22} />
            </button>
          ))}
        </div>
        <div className="dup-title">
          <div className="truncate">
            {allExact ? "100% identical" : `${g.similarity}% estimated similarity`}
          </div>
          <div className="muted">
            {allExact ? <span className="tag">Exact duplicate</span> : <span className="tag">Visually similar</span>}
            {g.members.some((m) => m.files.length > 1) && <span className="tag">Live Photo</span>}
            {plural(g.members.length, "copy", "copies")}
          </div>
        </div>
        <div className="dup-saving">
          <span>{formatSize(trashCount ? g.members.filter((m) => marks.has(keyOf(m))).reduce((n, m) => n + m.files.reduce((k, f) => k + f.size, 0), 0) : g.recoverable)}</span>
          <span className="muted">{trashCount ? "selected" : "potential saving"}</span>
        </div>
        <div className="group-actions">
          {!g.video && g.members.length > 1 && (
            <button className="btn small" onClick={() => onCompare(1)} title="Compare side by side">
              Compare
            </button>
          )}
          <button className="btn small" onClick={onKeepAll} disabled={!trashCount}>
            Keep All
          </button>
          <button className="btn small" onClick={() => onIgnore(true)} title="Set this group aside for now; it won't be cleaned">
            Ignore
          </button>
          <button className="btn small" onClick={onNotDuplicates} title="These are different photos or videos. Mori will remember and not suggest them again.">
            Not Duplicates
          </button>
        </div>
      </header>
      <div className="dup-members">
        {g.members.map((m, i) => {
          const trash = marks.has(keyOf(m));
          const f = m.files[0];
          return (
            <div key={keyOf(m)} className={`dup-row sim ${trash ? "trash" : ""}`} onContextMenu={(ev) => onMenu(ev, i)}>
              <button className="mini-thumb" onClick={() => onPreview(f)} title="Preview">
                <Thumb entry={simEntry(f)} fit="cover" iconSize={16} />
              </button>
              <div className="dup-info">
                <div className="name-line name">
                  <span className="truncate" title={m.files.map((x) => x.name).join(" + ")}>
                    {m.files[0].name}
                    {m.files.length > 1 && ` + ${m.files[1].ext.toUpperCase()}`}
                  </span>
                  {i === 0 && <span className="tag suggested">★ Suggested original</span>}
                  {i > 0 && (m.exact ? <span className="tag">Identical (verified)</span> : <span className="tag">{m.similarity}% similar</span>)}
                </div>
                <div className="truncate muted" title={`${f.location}/${f.path}`}>
                  {[f.location, ...parentOf(f.path).split("/").filter(Boolean)].join(" / ")}
                </div>
              </div>
              <div className="sim-facts muted">
                <span>{resolution(f)}</span>
                <span>{f.durationMs !== null ? formatDuration(f.durationMs) : f.ext.toUpperCase()}</span>
                <span>{g.video ? [f.container, f.codec].filter(Boolean).join(" · ") || f.ext.toUpperCase() : formatSize(f.size)}</span>
                <span>{g.video ? formatSize(f.size) : formatShortDate(f.created ?? f.modified)}</span>
              </div>
              <div className="dup-drive muted" title="Storage location">
                <Icon name="drive" size={13} /> <span className="truncate">{f.drive}</span>
              </div>
              <button className={`state-pill ${trash ? "trash" : "keep"}`} onClick={() => onToggle(i)} title={trash ? "Selected for Trash — click to keep" : "Kept — click to select for Trash"}>
                <Icon name={trash ? "trash" : "keep"} size={12} />
                {trash ? "Trash" : "Keep"}
              </button>
              <button className="btn small" onClick={() => onKeepOnly(i)} title="Keep this copy and select the others for Trash">
                Keep This One
              </button>
            </div>
          );
        })}
      </div>
    </section>
  );
}

// ------------------------------------------------------------- side by side

/** Two photos side by side, with shared zoom and pan (sanitized previews). */
function Compare({ group, a, b, onPick, onClose }: { group: SimGroup; a: number; b: number; onPick: (b: number) => void; onClose: () => void }) {
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const drag = useRef<{ x: number; y: number } | null>(null);
  const files = [group.members[a].files[0], group.members[b].files[0]];
  const setZ = (z: number) => {
    setZoom(z);
    if (z === 1) setPan({ x: 0, y: 0 });
  };
  return (
    <ModalFrame onCancel={onClose} wide>
      <div className="compare-head">
        <h2>Compare</h2>
        <div className="segmented small" role="group" aria-label="Zoom">
          {[1, 2, 4].map((z) => (
            <button key={z} className={zoom === z ? "on" : ""} onClick={() => setZ(z)}>
              {z === 1 ? "Fit" : `${z}×`}
            </button>
          ))}
        </div>
        {group.members.length > 2 && (
          <select className="compare-pick" value={b} onChange={(e) => onPick(Number(e.target.value))} aria-label="Compare with">
            {group.members.map((m, i) =>
              i === a ? null : (
                <option key={i} value={i}>
                  {m.files[0].name}
                </option>
              ),
            )}
          </select>
        )}
        <button className="icon-btn" onClick={onClose} aria-label="Close">
          <Icon name="close" size={14} />
        </button>
      </div>
      <div
        className="compare-panes"
        onPointerDown={(e) => {
          if (zoom > 1) drag.current = { x: e.clientX - pan.x, y: e.clientY - pan.y };
        }}
        onPointerMove={(e) => drag.current && setPan({ x: e.clientX - drag.current.x, y: e.clientY - drag.current.y })}
        onPointerUp={() => (drag.current = null)}
        onPointerLeave={() => (drag.current = null)}
      >
        {files.map((f, k) => (
          <figure key={f.id} className="compare-pane">
            <div className="compare-stage">
              <img src={previewUrl(f.id)} alt="" draggable={false} style={{ transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})` }} />
            </div>
            <figcaption>
              <div className="truncate">
                {k === 0 && <span className="tag suggested">★ Suggested</span>} {f.name}
              </div>
              <div className="muted">
                {resolution(f)} · {f.ext.toUpperCase()} · {formatSize(f.size)} · {formatShortDate(f.created ?? f.modified)}
                {f.exif ? " · camera metadata" : ""}
              </div>
              <div className="muted truncate">
                {f.location} / {f.path}
              </div>
            </figcaption>
          </figure>
        ))}
      </div>
      <p className="muted compare-note">
        {group.members[b].exact ? "Identical bytes, verified by hash." : `${group.members[b].similarity}% estimated similarity — an estimate, not a guarantee.`}
      </p>
    </ModalFrame>
  );
}
