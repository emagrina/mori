import { listen } from "@tauri-apps/api/event";
import { Fragment, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  formatDuration,
  formatShortDate,
  formatSize,
  isMac,
  parentOf,
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
import { FinalReview, LocationList, ProgressBar, Reasons, Summary, ToolSwitch } from "./Analyzer";
import { currentLocale, fmtNum, msg, t, tn, tx } from "../i18n";
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
  { key: "strict", label: msg("Strict"), text: msg("Only extremely similar media.") },
  { key: "balanced", label: msg("Balanced"), text: msg("Recommended.") },
  { key: "broad", label: msg("Broad"), text: msg("May find edited, cropped or recompressed versions, but can produce more false positives.") },
];

const STAGE: Record<SimProgress["stage"], string> = {
  collecting: msg("Finding photos and videos"),
  photos: msg("Generating visual fingerprints"),
  videos: msg("Sampling video frames"),
  comparing: msg("Comparing candidates"),
  done: msg("Finishing"),
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
          if (e.payload.status === "cancelled") onToast(t("Analysis cancelled — nothing was changed"));
          else setError(e.payload.message ?? t("The analysis failed."));
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
    if (marked(g) + 1 >= g.members.length) return onToast(t("At least one copy must be kept."), 3000);
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
    onToast(member === null ? t("Marked as not duplicates — Mori won't suggest these again") : t("Removed from this group — Mori won't suggest this match again"), 3500);
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
    onToast(t("Analysis cleared"));
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
    if (!text) return onToast(t("Path unavailable"));
    await navigator.clipboard.writeText(text).catch(() => {});
    onToast(t("Path copied"));
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
          <span className="crumb">{t("Analyze")}</span>
        </div>
        <div className="title-row">
          <h1 className="page-title">
            {t("Similar Media")}
            {phase !== "setup" && phase !== "running" && view && (
              <span className="scope-note">{t("in {places}", { places: view.locations.map((l) => l.label).join(", ") })}</span>
            )}
          </h1>
          {(phase === "results" || phase === "summary") && view && (
            <>
              <button className="btn" onClick={clear} title={t("Forget these results (they are only kept in memory)")}>
                {t("Clear Analysis")}
              </button>
              <button className="btn" onClick={() => setPhase("setup")}>
                {t("New Analysis")}
              </button>
            </>
          )}
          {phase === "results" && live.length > 0 && (
            <button className="btn primary" onClick={autoSelect} title={t("Keep the suggested copy in every group (except ignored ones) and review the rest")}>
              {t("Auto-select")}
            </button>
          )}
        </div>
      </div>

      <div className="analyzer-body">
        {phase === "setup" && (
          <div className="setup">
            <ToolSwitch current="similar" onSwitch={onSwitch} />
            <p className="lead">
              {tx(
                "Finds photos and videos that look like the same picture or clip even though their files differ — another format, size, compression or metadata. These are {estimates}: review every group before cleaning. For files that are identical byte for byte, use Exact Duplicates.",
                { estimates: <strong>{t("estimates")}</strong> },
              )}
            </p>
            <section>
              <h3>{t("Where to look")}</h3>
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
              <h3>{t("Media")}</h3>
              <div className="chips">
                <button className={`chip ${photos ? "on" : ""}`} onClick={() => setPhotos((v) => !v)} aria-pressed={photos}>
                  <Icon name="photo" size={14} /> {t("Photos")}
                </button>
                <button className={`chip ${videos ? "on" : ""}`} onClick={() => setVideos((v) => !v)} aria-pressed={videos}>
                  <Icon name="video" size={14} /> {t("Videos")}
                </button>
              </div>
              <label className="toggle">
                <input type="checkbox" checked={recursive} onChange={(e) => setRecursive(e.target.checked)} />
                <span className="switch" aria-hidden />
                {t("Include subfolders")}
              </label>
            </section>
            <section>
              <h3>{t("Similarity")}</h3>
              <div className="segmented sensitivity" role="radiogroup" aria-label={t("Similarity")}>
                {SENSITIVITY.map((s) => (
                  <button key={s.key} className={sensitivity === s.key ? "on" : ""} onClick={() => setSensitivity(s.key)} role="radio" aria-checked={sensitivity === s.key}>
                    {t(s.label)}
                  </button>
                ))}
              </div>
              <p className="hint-line">{t(SENSITIVITY.find((s) => s.key === sensitivity)!.text)}</p>
            </section>
            {error && <p className="field-error">{error}</p>}
            <div className="setup-actions">
              {view && (
                <button className="btn" onClick={() => setPhase("results")}>
                  {t("Back to Results")}
                </button>
              )}
              <button className="btn primary large" disabled={!locations.some((l) => chosen.has(l.key)) || (!photos && !videos)} onClick={start}>
                {t("Analyze")}
              </button>
            </div>
            <p className="fineprint">
              {t(
                "Everything happens on this computer: photos are decoded in Mori's sandboxed worker and videos are sampled at 12 points by the system video engine, then reduced to small grayscale fingerprints. Fingerprints are cached locally (removed by Clear Cache); nothing is uploaded. Private folders inside the chosen locations are skipped; choose a private folder directly to analyze it.",
              )}
              {dismissedCount > 0 && (
                <>
                  {" "}
                  {tn(dismissedCount, "{n} match you marked as not duplicates won't be suggested again.", "{n} matches you marked as not duplicates won't be suggested again.")}{" "}
                  <button
                    className="link"
                    onClick={() =>
                      api.similarForgetDecisions().then(() => {
                        setDismissedCount(0);
                        onToast(t("Not-duplicate decisions forgotten"));
                      })
                    }
                  >
                    {t("Forget these decisions")}
                  </button>
                </>
              )}
            </p>
          </div>
        )}

        {phase === "running" && (
          <div className="analyzer-center">
            <div className="progress-card">
              <h2>{progress ? t(STAGE[progress.stage]) : t("Starting")}…</h2>
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
                  <dt>{t("Photos")}</dt>
                  <dd>{progress && progress.stage !== "collecting" ? t("{done} of {total}", { done: progress.photosDone, total: progress.photosTotal }) : "—"}</dd>
                </div>
                <div>
                  <dt>{t("Videos")}</dt>
                  <dd>{progress && progress.stage !== "collecting" ? t("{done} of {total}", { done: progress.videosDone, total: progress.videosTotal }) : "—"}</dd>
                </div>
                <div>
                  <dt>{t("Comparing")}</dt>
                  <dd>{progress?.stage === "comparing" ? t("{pct}%", { pct: Math.round((100 * progress.compareDone) / Math.max(1, progress.compareTotal)) }) : "—"}</dd>
                </div>
                <div>
                  <dt>{t("Similar groups")}</dt>
                  <dd>{progress?.stage === "done" ? fmtNum(progress.groups) : "—"}</dd>
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
                  {cancelling ? t("Cancelling…") : t("Cancel")}
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
                  <strong>{tn(groups.length, "{n} similar group", "{n} similar groups")}</strong>
                  <span>{t("{size} potentially recoverable", { size: formatSize(recoverable) })}</span>
                  {ignored.size > 0 && <span>{tn(ignored.size, "{n} group ignored", "{n} groups ignored")}</span>}
                </>
              ) : (
                <strong>{t("No similar media found")}</strong>
              )}
              {st && (
                <span className="muted">
                  {t("{photos} and {videos} analyzed", { photos: tn(st.photos, "{n} photo", "{n} photos"), videos: tn(st.videos, "{n} video", "{n} videos") })}
                  {st.unanalyzable > 0 && ` · ${tn(st.unanalyzable, "{n} file could not be safely analyzed", "{n} files could not be safely analyzed")}`}
                  {st.dismissed > 0 && ` · ${tn(st.dismissed, "{n} match hidden (not duplicates)", "{n} matches hidden (not duplicates)")}`}
                </span>
              )}
            </div>
            {groups.length === 0 ? (
              <div className="empty">
                <Icon name="check" size={34} stroke={1.3} />
                <p>{t("Mori found no photos or videos that look like copies of each other here.")}</p>
              </div>
            ) : (
              <div className="dup-list">
                {groups.slice(0, shown).map((g, i, list) => (
                  <Fragment key={groupKey(g)}>
                    {(i === 0 || list[i - 1].video !== g.video) && (
                      <h3 className="list-heading">
                        {g.video ? t("Similar videos · {count}", { count: videoGroups.length }) : t("Similar photos · {count}", { count: photoGroups.length })}
                      </h3>
                    )}
                    {card(g)}
                  </Fragment>
                ))}
                {shown < groups.length && (
                  <button className="btn more" onClick={() => setShown((n) => n + PAGE)}>
                    {tn(groups.length - shown, "Show more ({n} group left)", "Show more ({n} groups left)")}
                  </button>
                )}
              </div>
            )}
            {plan.items.length > 0 && (
              <div className="selection-bar">
                <span>
                  {tx(tn(plan.trash.length, "{files} selected for Trash · {size}", "{files} selected for Trash · {size}"), {
                    files: <strong>{tn(plan.trash.length, "{n} file", "{n} files")}</strong>,
                    size: formatSize(plan.bytes),
                  })}
                </span>
                <div className="spacer" />
                <button className="btn" onClick={() => setMarks(new Set())}>
                  {t("Keep All")}
                </button>
                <button className="btn primary" onClick={() => setPhase("review")}>
                  {t("Review…")}
                </button>
              </div>
            )}
          </>
        )}

        {phase === "cleaning" && cleanup && (
          <div className="analyzer-center">
            <div className="progress-card">
              <h2>{t("Moving selected files to Trash…")}</h2>
              <ProgressBar value={cleanup.total ? cleanup.done / cleanup.total : 0} />
              <p className="muted">
                {tn(cleanup.total, "{done} of {n} file · each kept copy is re-checked first", "{done} of {n} files · each kept copy is re-checked first", { done: cleanup.done })}
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
                  {cancelling ? t("Stopping…") : t("Stop")}
                </button>
              </div>
            </div>
          </div>
        )}

        {phase === "summary" && outcome && <Summary outcome={outcome} remaining={groups.length} onDone={() => setPhase(groups.length ? "results" : "setup")} />}
      </div>

      {phase === "review" && (
        <FinalReview
          title={t("Similar media cleanup")}
          confirmLabel={t("Move Selected Files to Trash")}
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
            <Icon name="gallery" size={14} /> {t("Preview")}
          </button>
          <button onClick={() => api.revealFile(menu.group.members[menu.member].files[0].id).catch(() => onToast(t("File unavailable")))}>
            <Icon name="reveal" size={14} /> {isMac ? t("Show in Finder") : t("Show in Folder")}
          </button>
          <button onClick={() => copyPath(menu.group.members[menu.member].files[0].id)}>
            <Icon name="copy" size={14} /> {t("Copy Path")}
          </button>
          <div className="sep" />
          <button onClick={() => keepCopy(menu.group, menu.member)}>
            <Icon name="keep" size={14} /> {t("Keep This Copy")}
          </button>
          <button className="danger" onClick={() => selectForTrash(menu.group, menu.member)}>
            <Icon name="trash" size={14} /> {t("Select for Trash")}
          </button>
          {menu.member > 0 && (
            <>
              <div className="sep" />
              <button onClick={() => notDuplicates(menu.group, menu.member)}>
                <Icon name="close" size={14} /> {t("Not a Match")}
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
            <div className="muted">{t("Ignored — not part of any cleanup")}</div>
          </div>
          <button className="btn small" onClick={() => onIgnore(false)}>
            {t("Undo")}
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
            <button key={keyOf(m)} className="dup-thumb" onClick={() => onPreview(m.files[0])} title={t("Preview")}>
              <Thumb entry={simEntry(m.files[0])} fit="cover" iconSize={22} />
            </button>
          ))}
        </div>
        <div className="dup-title">
          <div className="truncate">
            {allExact ? t("Identical content (verified by hash)") : t("{pct}% estimated visual similarity", { pct: g.similarity })}
          </div>
          <div className="muted">
            {allExact ? (
              <span className="tag">{t("Exact duplicate")}</span>
            ) : (
              <span className="tag" title={t("Looks alike; the files are not byte-identical. Use Compare Integrity for an exact SHA-256 check.")}>
                {t("Visually similar")}
              </span>
            )}
            {g.members.some((m) => m.files.length > 1) && <span className="tag">{t("Live Photo")}</span>}
            {g.burst && (
              <span className="tag" title={t("Taken in quick succession by the same camera (from capture times)")}>
                {tn(g.burst.frames, "Burst · {n} frame in {secs} s", "Burst · {n} frames in {secs} s", {
                  secs: (g.burst.spanMs / 1000).toLocaleString(currentLocale(), { minimumFractionDigits: 1, maximumFractionDigits: 1 }),
                })}
              </span>
            )}
            {tn(g.members.length, "{n} copy", "{n} copies")}
          </div>
        </div>
        <div className="dup-saving">
          <span>{formatSize(trashCount ? g.members.filter((m) => marks.has(keyOf(m))).reduce((n, m) => n + m.files.reduce((k, f) => k + f.size, 0), 0) : g.recoverable)}</span>
          <span className="muted">{trashCount ? t("selected") : t("potential saving")}</span>
        </div>
        <div className="group-actions">
          {!g.video && g.members.length > 1 && (
            <button className="btn small" onClick={() => onCompare(1)} title={t("Compare side by side")}>
              {t("Compare")}
            </button>
          )}
          <button className="btn small" onClick={onKeepAll} disabled={!trashCount}>
            {t("Keep All")}
          </button>
          <button className="btn small" onClick={() => onIgnore(true)} title={t("Set this group aside for now; it won't be cleaned")}>
            {t("Ignore")}
          </button>
          <button className="btn small" onClick={onNotDuplicates} title={t("These are different photos or videos. Mori will remember and not suggest them again.")}>
            {t("Not Duplicates")}
          </button>
        </div>
      </header>
      <Reasons reasons={g.reasons} />
      <div className="dup-members">
        {g.members.map((m, i) => {
          const trash = marks.has(keyOf(m));
          const f = m.files[0];
          return (
            <div key={keyOf(m)} className={`dup-row sim ${trash ? "trash" : ""}`} onContextMenu={(ev) => onMenu(ev, i)}>
              <button className="mini-thumb" onClick={() => onPreview(f)} title={t("Preview")}>
                <Thumb entry={simEntry(f)} fit="cover" iconSize={16} />
              </button>
              <div className="dup-info">
                <div className="name-line name">
                  <span className="truncate" title={m.files.map((x) => x.name).join(" + ")}>
                    {m.files[0].name}
                    {m.files.length > 1 && ` + ${m.files[1].ext.toUpperCase()}`}
                  </span>
                  {i === 0 && <span className="tag suggested">{t("★ Suggested original")}</span>}
                  {i > 0 && (m.exact ? (
                      <span className="tag" title={t("Byte-for-byte identical, confirmed by a full content hash")}>
                        {t("Identical bytes (verified)")}
                      </span>
                    ) : (
                      <span className="tag">{t("{pct}% similar", { pct: m.similarity })}</span>
                    ))}
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
              <div className="dup-drive muted" title={t("Storage location")}>
                <Icon name="drive" size={13} /> <span className="truncate">{f.drive}</span>
              </div>
              <button className={`state-pill ${trash ? "trash" : "keep"}`} onClick={() => onToggle(i)} title={trash ? t("Selected for Trash — click to keep") : t("Kept — click to select for Trash")}>
                <Icon name={trash ? "trash" : "keep"} size={12} />
                {trash ? t("Trash") : t("Keep")}
              </button>
              <button className="btn small" onClick={() => onKeepOnly(i)} title={t("Keep this copy and select the others for Trash")}>
                {t("Keep This One")}
              </button>
            </div>
          );
        })}
      </div>
    </section>
  );
}

// ------------------------------------------------------------- side by side

type CompareMode = "side" | "slider" | "diff";

/**
 * Two photos: side by side, overlaid with a slider, or as a difference image.
 * All modes use the worker-made previews (never the originals) and share
 * one zoom and pan.
 */
function Compare({ group, a, b, onPick, onClose }: { group: SimGroup; a: number; b: number; onPick: (b: number) => void; onClose: () => void }) {
  const [mode, setMode] = useState<CompareMode>("side");
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [split, setSplit] = useState(50);
  const drag = useRef<{ x: number; y: number } | null>(null);
  const files = [group.members[a].files[0], group.members[b].files[0]];
  const setZ = (z: number) => {
    setZoom(z);
    if (z === 1) setPan({ x: 0, y: 0 });
  };
  const transform = { transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})` };
  const panHandlers = {
    onPointerDown: (e: React.PointerEvent) => {
      if (zoom > 1 && !(e.target as HTMLElement).closest(".slider-handle")) drag.current = { x: e.clientX - pan.x, y: e.clientY - pan.y };
    },
    onPointerMove: (e: React.PointerEvent) => drag.current && setPan({ x: e.clientX - drag.current.x, y: e.clientY - drag.current.y }),
    onPointerUp: () => (drag.current = null),
    onPointerLeave: () => (drag.current = null),
  };
  const caption = (f: SimFile, k: number) => (
    <figcaption key={f.id}>
      <div className="truncate">
        {k === 0 ? <span className="tag suggested">{t("★ A · Suggested")}</span> : <span className="tag">B</span>} {f.name}
      </div>
      <div className="muted">
        {resolution(f)} · {f.ext.toUpperCase()} · {formatSize(f.size)} · {formatShortDate(f.created ?? f.modified)}
        {f.exif ? ` · ${t("camera metadata")}` : ""}
      </div>
      <div className="muted truncate">
        {f.location} / {f.path}
      </div>
    </figcaption>
  );
  return (
    <ModalFrame onCancel={onClose} wide>
      <div className="compare-head">
        <h2>{t("Compare")}</h2>
        <div className="segmented small" role="tablist" aria-label={t("Mode")}>
          {(
            [
              ["side", "A | B"],
              ["slider", msg("Slider")],
              ["diff", msg("Difference")],
            ] as const
          ).map(([m, label]) => (
            <button key={m} className={mode === m ? "on" : ""} onClick={() => setMode(m)} role="tab" aria-selected={mode === m}>
              {t(label)}
            </button>
          ))}
        </div>
        <div className="segmented small" role="group" aria-label={t("Zoom")}>
          {[1, 2, 4].map((z) => (
            <button key={z} className={zoom === z ? "on" : ""} onClick={() => setZ(z)}>
              {z === 1 ? t("Fit") : `${z}×`}
            </button>
          ))}
        </div>
        {group.members.length > 2 && (
          <select className="compare-pick" value={b} onChange={(e) => onPick(Number(e.target.value))} aria-label={t("Compare with")}>
            {group.members.map((m, i) =>
              i === a ? null : (
                <option key={i} value={i}>
                  {m.files[0].name}
                </option>
              ),
            )}
          </select>
        )}
        <button className="icon-btn" onClick={onClose} aria-label={t("Close")}>
          <Icon name="close" size={14} />
        </button>
      </div>
      {mode === "side" && (
        <div className="compare-panes" {...panHandlers}>
          {files.map((f, k) => (
            <figure key={f.id} className="compare-pane">
              <div className="compare-stage">
                <img src={previewUrl(f.id)} alt="" draggable={false} style={transform} />
              </div>
              {caption(f, k)}
            </figure>
          ))}
        </div>
      )}
      {mode === "slider" && (
        <>
          <div className="compare-single" {...panHandlers}>
            <div className="compare-stage">
              {/* B underneath, A on top clipped at the slider: both fill the same box. */}
              <div className="slider-box" style={transform}>
                <img src={previewUrl(files[1].id)} alt="" draggable={false} />
                <img src={previewUrl(files[0].id)} alt="" draggable={false} className="overlay" style={{ clipPath: `inset(0 ${100 - split}% 0 0)` }} />
                <div className="slider-line" style={{ left: `${split}%` }} />
              </div>
              <span className="slider-label left">A</span>
              <span className="slider-label right">B</span>
            </div>
            <input className="slider-handle" type="range" min={0} max={100} value={split} onChange={(e) => setSplit(Number(e.target.value))} aria-label={t("Slider position")} />
          </div>
          <div className="compare-captions">{files.map(caption)}</div>
        </>
      )}
      {mode === "diff" && (
        <>
          <div className="compare-single" {...panHandlers}>
            <DiffView a={files[0].id} b={files[1].id} style={transform} />
          </div>
          <div className="compare-captions">{files.map(caption)}</div>
        </>
      )}
      <p className="muted compare-note">
        {group.members[b].exact
          ? t("Identical bytes, verified by hash.")
          : t("{pct}% estimated similarity — an estimate, not a guarantee.", { pct: group.members[b].similarity })}
        {mode === "diff" && ` ${t("Bright areas differ; black is identical (after scaling both to the same size).")}`}
      </p>
    </ModalFrame>
  );
}

/** |A − B| per pixel, amplified, from the two sanitized previews. */
function DiffView({ a, b, style }: { a: string; b: string; style: React.CSSProperties }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [state, setState] = useState<{ changed: number } | "loading" | "failed">("loading");
  useEffect(() => {
    let alive = true;
    setState("loading");
    const load = (id: string) =>
      new Promise<HTMLImageElement>((ok, fail) => {
        const img = new Image();
        img.crossOrigin = "anonymous";
        img.onload = () => ok(img);
        img.onerror = fail;
        img.src = previewUrl(id);
      });
    Promise.all([load(a), load(b)]).then(
      ([ia, ib]) => {
        const c = canvas.current;
        if (!alive || !c) return;
        const scale = Math.min(1, 1600 / Math.max(ia.naturalWidth, ia.naturalHeight));
        const w = Math.max(1, Math.round(ia.naturalWidth * scale));
        const h = Math.max(1, Math.round(ia.naturalHeight * scale));
        const off = document.createElement("canvas");
        off.width = w;
        off.height = h;
        const octx = off.getContext("2d", { willReadFrequently: true })!;
        try {
          octx.drawImage(ia, 0, 0, w, h);
          const pa = octx.getImageData(0, 0, w, h).data;
          octx.drawImage(ib, 0, 0, w, h);
          const pb = octx.getImageData(0, 0, w, h);
          const out = pb.data;
          let changed = 0;
          for (let i = 0; i < out.length; i += 4) {
            const d = (Math.abs(pa[i] - out[i]) + Math.abs(pa[i + 1] - out[i + 1]) + Math.abs(pa[i + 2] - out[i + 2])) / 3;
            if (d > 24) changed++;
            const v = Math.min(255, d * 4);
            out[i] = out[i + 1] = out[i + 2] = v;
            out[i + 3] = 255;
          }
          c.width = w;
          c.height = h;
          c.getContext("2d")!.putImageData(pb, 0, 0);
          setState({ changed: changed / (w * h) });
        } catch {
          setState("failed");
        }
      },
      () => alive && setState("failed"),
    );
    return () => {
      alive = false;
    };
  }, [a, b]);
  return (
    <div className="compare-stage">
      <canvas ref={canvas} style={{ ...style, visibility: typeof state === "object" ? "visible" : "hidden" }} />
      {state === "loading" && <div className="spinner" />}
      {state === "failed" && <p className="muted">{t("The difference couldn't be computed for these previews.")}</p>}
      {typeof state === "object" && <span className="diff-stat">{t("{pct}% of pixels differ noticeably", { pct: (state.changed * 100).toLocaleString(currentLocale(), { minimumFractionDigits: 1, maximumFractionDigits: 1 }) })}</span>}
    </div>
  );
}
