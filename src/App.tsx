import { listen } from "@tauri-apps/api/event";
import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  isMac,
  PRIVATE_HINT,
  resetThumbs,
  setPreviewOpen,
  startSimilarCaptureService,
  type ConnectedDrive,
  type Entry,
  type InitInfo,
  type KindFilter,
  type TagInfo,
  type ViewKind,
  type QueryResult,
  type Scope,
  type SortKey,
  type Stats,
  type Status,
  type ViewMode,
  type TransferOp,
  type TransferPlan,
  type TransferResult,
  type TrashResult,
} from "./api";
import { click as clickSel, EMPTY_SELECTION, focusIndex, moveTo, neighborAfterRemoval, reconcile as reconcileSel, selectAll, selectedItems, selectOnly as only, type Selection } from "./selection";
import { forgetRecentDestinations, TransferDialog, type Dest } from "./components/Transfer";
import { beginDrag, canDropInto, dragged, dragSource, DRAG_TYPE, edgeScroll, endDrag, SPRING_MS } from "./dnd";
import { Analyzer } from "./components/Analyzer";
import { FileView } from "./components/FileView";
import { Inspector } from "./components/Inspector";
import { Icon, Logo, type IconName } from "./components/Icon";
import { ModalFrame } from "./components/Modal";
import { AnalysisCenter } from "./components/AnalysisCenter";
import { HealthView } from "./components/HealthView";
import { MetadataAnalyzer } from "./components/MetadataAnalyzer";
import { TagDeleteDialog, TagDialog, TagList, TagManager, TagRenameDialog } from "./components/Organize";
import { HistoryPanel, OperationPreview } from "./components/Operations";
import { ChecksumDialog, CompareDialog, IntegrityPanel } from "./components/Integrity";
import { Diagnostics } from "./components/Diagnostics";
import { PrivacyData } from "./components/PrivacyData";
import { CommandPalette, ShortcutsHelp, type Command } from "./components/CommandPalette";
import { StorageView } from "./components/StorageView";
import { Preview } from "./components/Preview";
import { SimilarAnalyzer } from "./components/SimilarAnalyzer";
import { LanguageDialog } from "./components/Language";
import { currentLocale, fmtNum, msg, t, tn, tx } from "./i18n";

interface Location {
  scope: Scope;
  folder: string;
}

const FILTERS: { kind: KindFilter; label: string; icon: IconName }[] = [
  { kind: "all", label: msg("All"), icon: "all" },
  { kind: "photo", label: msg("Photos"), icon: "photo" },
  { kind: "video", label: msg("Videos"), icon: "video" },
  { kind: "gif", label: msg("GIFs"), icon: "gif" },
  { kind: "document", label: msg("Documents"), icon: "document" },
  { kind: "audio", label: msg("Audio"), icon: "audio" },
  { kind: "other", label: msg("Other"), icon: "other" },
];

/** Guessed from names and the macOS screen-capture attribute; correctable per file. */
const CAPTURE_FILTERS: { kind: KindFilter; label: string; icon: IconName }[] = [
  { kind: "screenshot", label: msg("Screenshots"), icon: "screenshot" },
  { kind: "recording", label: msg("Screen Recordings"), icon: "recording" },
];

const filterIcon = (k: KindFilter) => [...FILTERS, ...CAPTURE_FILTERS].find((f) => f.kind === k)?.icon ?? "all";

/** Translated title of a library view (tag names are the user's own). */
const viewTitle = (k: ViewKind, tags: TagInfo[]) =>
  k === "favorites" ? t("Favorites") : k.startsWith("tag:") ? tags.find((x) => `tag:${x.id}` === k)?.name ?? t("Tag") : t(LIBRARY_TITLE[k as KindFilter]);

/** A view title inside a sentence: lower case, except where nouns keep their capital. */
const inSentence = (title: string) => (currentLocale() === "de" ? title : title.toLocaleLowerCase(currentLocale()));

const LIBRARY_TITLE: Record<KindFilter, string> = {
  screenshot: msg("Screenshots"),
  recording: msg("Screen Recordings"),
  all: msg("All Files"),
  photo: msg("Photos"),
  video: msg("Videos"),
  gif: msg("GIFs"),
  document: msg("Documents"),
  audio: msg("Audio"),
  other: msg("Other Files"),
};

const SORTS: { key: SortKey; label: string }[] = [
  { key: "name", label: msg("Name") },
  { key: "modified", label: msg("Date Modified") },
  { key: "created", label: msg("Date Created") },
  { key: "size", label: msg("Size") },
  { key: "type", label: msg("Type") },
];

type Dialog =
  | { kind: "rename"; entry: Entry }
  | { kind: "private"; entry: Entry }
  | { kind: "unprotect"; entry: Entry }
  | { kind: "tags"; entries: Entry[] }
  | { kind: "manageTags" }
  | { kind: "renameTag"; tag: TagInfo }
  | { kind: "deleteTag"; tag: TagInfo }
  | { kind: "forget" }
  | { kind: "clearSession" }
  | { kind: "op"; op: "trash" | "delete"; entries: Entry[] }
  | { kind: "transfer"; op: TransferOp; entries: Entry[]; dest?: Dest }
  | { kind: "history" }
  | { kind: "checksum"; entry: Entry }
  | { kind: "compare"; a: Entry; b: Entry }
  | { kind: "integrity" }
  | { kind: "diagnostics" }
  | { kind: "privacy" }
  | { kind: "language" };

/** Loaded when first opened. */
const QuickCleanup = lazy(() => import("./components/Cleanup").then((m) => ({ default: m.QuickCleanup })));

const EMPTY: QueryResult = { items: [], total: 0, truncated: false, crumbs: [] };

export default function App() {
  const [info, setInfo] = useState<InitInfo | null>(null);
  const [status, setStatus] = useState<Status | null>(null);
  const [stats, setStats] = useState<Stats | null>(null);
  const [topFolders, setTopFolders] = useState<Entry[]>([]);
  const [view, setView] = useState<ViewMode>("grid");
  const [sort, setSort] = useState<SortKey>("name");
  const [desc, setDesc] = useState(false);
  const [recursive, setRecursive] = useState(false);
  const [searchGlobal, setSearchGlobal] = useState(false);
  const [loc, setLoc] = useState<Location>({ scope: "folder", folder: "" });
  const [history, setHistory] = useState<Location[]>([]);
  const [kind, setKind] = useState<ViewKind>("all");
  const [tags, setTags] = useState<TagInfo[]>([]);
  const [search, setSearch] = useState("");
  const [debounced, setDebounced] = useState("");
  const [result, setResult] = useState<QueryResult>(EMPTY);
  /** Selected items (by id) and the focused one: see selection.ts. */
  const [sel, setSel] = useState<Selection>(EMPTY_SELECTION);
  const [previewId, setPreviewId] = useState<string | null>(null);
  /** The preview was opened with "Open in Isolation". */
  const [previewIso, setPreviewIso] = useState(false);
  /** The preview is Mori Quick Look (Space). */
  const [previewQuick, setPreviewQuick] = useState(false);
  const [palette, setPalette] = useState(false);
  const [shortcuts, setShortcuts] = useState(false);
  /** A drive connected while Mori runs, offered for safe inspection. */
  const [newDrive, setNewDrive] = useState<ConnectedDrive | null>(null);
  /** The Safe Inspection banner was collapsed ("Browse metadata only"). */
  const [safeCollapsed, setSafeCollapsed] = useState(false);
  /** Quick Cleanup of a folder (staged decisions only, until the final confirmation). */
  const [cleanup, setCleanup] = useState<{ id: string; name: string } | null>(null);
  /** `targets`: what the menu's actions apply to (the whole selection when right-clicking inside it). */
  const [menu, setMenu] = useState<{ x: number; y: number; entry: Entry; targets: Entry[] } | null>(null);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  /** Item shown in the inspector panel. */
  const [inspectId, setInspectId] = useState<string | null>(null);
  const [readOnly, setReadOnlyState] = useState(false);
  const [mode, setMode] = useState<"browse" | "center" | "analyzer" | "similar" | "metadata" | "places" | "storage" | "health">("browse");
  /** Files opened from Storage or Media Health (Media Health opens them isolated). */
  const [extPreview, setExtPreview] = useState<{ items: Entry[]; index: number; isolated: boolean } | null>(null);
  /** Bumped when files are trashed from the browser, so analyzer results refresh. */
  const [analysisVersion, setAnalysisVersion] = useState(0);
  // Small anchored menus: sort options and the sidebar overflow ("more") menu.
  const [pop, setPop] = useState<{ kind: "sort" | "more"; x: number; y: number } | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [indexVersion, setIndexVersion] = useState(0);
  const searchRef = useRef<HTMLInputElement>(null);
  const settingsLoaded = useRef(false);

  // ------------------------------------------------------------- startup

  // The Similar Media analyzer asks the webview for sampled video frames.
  useEffect(() => startSimilarCaptureService(), []);

  useEffect(() => {
    api.init().then((i) => {
      setInfo(i);
      setStatus(i.status);
      if (i.view) setView(i.view);
      if (i.sort) setSort(i.sort);
      if (i.desc !== null) setDesc(i.desc);
      if (i.recursive !== null) setRecursive(i.recursive);
      if (i.searchGlobal !== null) setSearchGlobal(i.searchGlobal);
      setReadOnlyState(i.readOnly);
      // If the UI was reloaded (e.g. after the video engine failed), come back
      // to the same place instead of the drive root.
      const saved = readSession(i.launchId);
      if (saved) {
        setLoc(saved.loc);
        setKind(saved.kind);
        setSearch(saved.search);
      }
      settingsLoaded.current = true;
      api.takeRecovered().then((names) => {
        if (names.length) {
          setToast(t("Mori recovered after the video engine stopped responding on “{name}”. That file won't be previewed again.", { name: names[names.length - 1] }));
          setTimeout(() => setToast(null), 7000);
        }
      });
    });
    let timer: number | undefined;
    const unlisten = [
      listen<Status>("status", (e) => setStatus(e.payload)),
      listen<ConnectedDrive>("drive-connected", (e) => setNewDrive(e.payload)),
      // Batch bursts of index updates during a scan.
      listen("index-changed", () => {
        if (timer) return;
        timer = window.setTimeout(() => {
          timer = undefined;
          resetThumbs();
          setIndexVersion((v) => v + 1);
        }, 400);
      }),
    ];
    return () => unlisten.forEach((p) => p.then((f) => f()));
  }, []);

  useEffect(() => {
    if (settingsLoaded.current) api.updateSettings({ view, sort, desc, recursive, searchGlobal });
  }, [view, sort, desc, recursive, searchGlobal]);

  useEffect(() => {
    // A temporary session keeps no record of where you were or what you searched.
    if (settingsLoaded.current && info && !status?.temporary) writeSession({ launchId: info.launchId, loc, kind, search });
  }, [loc, kind, search, info, status?.temporary]);

  // Background video decoding (thumbnail frames) pauses while previewing.
  useEffect(() => setPreviewOpen(previewId !== null || extPreview !== null), [previewId, extPreview]);
  useEffect(() => {
    if (!previewId) {
      setPreviewIso(false);
      setPreviewQuick(false);
    }
  }, [previewId]);

  // ⌘K / Ctrl+K works everywhere except over other dialogs.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((isMac ? e.metaKey : e.ctrlKey) && e.key.toLowerCase() === "k" && !e.altKey && !e.shiftKey) {
        e.preventDefault();
        if (!dialog && !cleanup) setPalette((p) => !p);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [dialog, cleanup]);

  // Safe Inspection Mode: keep the "Media decoded" counter current.
  useEffect(() => {
    if (!status?.safeMode) return;
    const t = window.setInterval(() => api.status().then(setStatus, () => {}), 2000);
    return () => window.clearInterval(t);
  }, [status?.safeMode]);

  useEffect(() => {
    if (!status?.hasRoot) return;
    api.stats().then(setStats);
    api.subfolders("").then(setTopFolders);
    api.tagsList().then(setTags, () => {});
  }, [indexVersion, status?.hasRoot, status?.rootName]);

  // --------------------------------------------------------------- query

  useEffect(() => {
    const t = setTimeout(() => setDebounced(search.trim()), search ? 90 : 0);
    return () => clearTimeout(t);
  }, [search]);

  useEffect(() => {
    if (!status?.hasRoot) return;
    let alive = true;
    api
      .query({ ...loc, kind, search: debounced, sort, desc, recursive: recursive && loc.scope === "folder", global: searchGlobal })
      .then((r) => alive && setResult(r));
    return () => {
      alive = false;
    };
  }, [loc, kind, debounced, sort, desc, recursive, searchGlobal, indexVersion, status?.hasRoot, status?.rootName]);

  const items = result.items;
  const files = useMemo(() => items.filter((e) => e.kind !== "folder"), [items]);
  const order = useMemo(() => items.map((e) => e.id), [items]);
  const selected = useMemo(() => focusIndex(sel, order), [sel, order]);
  const marked = sel.ids;
  const previewIndex = useMemo(() => (previewId ? files.findIndex((e) => e.id === previewId) : -1), [files, previewId]);
  const crumbs = result.crumbs;

  const selectOnly = useCallback((id: string | null) => setSel(only(id)), []);

  // Forget selected items that are no longer listed (moved, trashed, filtered out).
  useEffect(() => setSel((cur) => reconcileSel(cur, order)), [order]);
  const parentFolder = crumbs.length > 1 ? crumbs[crumbs.length - 2].id : "";

  // -------------------------------------------------------------- actions

  const navigate = useCallback(
    (next: Location, opts: { keepKind?: boolean } = {}) => {
      setHistory((h) => [...h.slice(-49), loc]);
      setLoc(next);
      setSearch("");
      selectOnly(null);
      setMode("browse");
      if (!opts.keepKind && next.scope === "folder") setKind("all");
    },
    [loc, selectOnly],
  );

  const goBack = useCallback(() => {
    if (search) return setSearch("");
    const prev = history[history.length - 1];
    if (prev) {
      setHistory((h) => h.slice(0, -1));
      setLoc(prev);
      selectOnly(null);
    } else if (loc.scope === "folder" && loc.folder) {
      setLoc({ scope: "folder", folder: parentFolder });
    }
  }, [history, loc, search, parentFolder]);

  const activate = useCallback(
    (i: number) => {
      const e = items[i];
      if (!e) return;
      if (e.kind === "folder") navigate({ scope: "folder", folder: e.id }, { keepKind: true });
      // Links are never followed: show what they are instead.
      else if (e.kind === "link") setInspectId(e.id);
      else setPreviewId(e.id);
    },
    [items, navigate],
  );

  const flash = (msg: string, ms = 1800) => {
    setToast(msg);
    setTimeout(() => setToast((t) => (t === msg ? null : t)), ms);
  };

  // ------------------------------------------------- trash & rename

  /** A single file goes straight to the Trash; several items or a folder ask first. */
  const requestTrash = async (targets: Entry[]) => {
    if (!targets.length) return;
    if (targets.length === 1 && targets[0].kind !== "folder") return moveToTrash(targets);
    // Several items or a folder: Operation Preview first.
    setDialog({ kind: "op", op: "trash", entries: targets });
  };

  const undoLast = async () => {
    try {
      const r = await api.historyUndo(null);
      flash(
        r.failed.length
          ? tn(r.restored, "Undo: {n} change restored · {reason}", "Undo: {n} changes restored · {reason}", { reason: r.failed[0] })
          : tn(r.restored, "Undone ({n} change)", "Undone ({n} changes)"),
        3500,
      );
      setAnalysisVersion((v) => v + 1);
    } catch (e) {
      flash(String(e), 3500);
    }
  };

  /**
   * Items are about to leave the view (Trash, Move): a preview showing one
   * moves on to the next sensible file (releasing the old one), and the
   * keyboard focus continues from the same place.
   */
  const leaving = (targets: Entry[]) => {
    const folders = targets.filter((t) => t.kind === "folder").map((t) => `${t.path}/`);
    const ids = new Set(targets.map((t) => t.id));
    const inside = (e: { id: string; path: string }) => ids.has(e.id) || folders.some((f) => e.path.startsWith(f));
    const previewed = previewId ? files.find((f) => f.id === previewId) : undefined;
    if (previewed && inside(previewed)) {
      const next = neighborAfterRemoval(
        files.map((f) => f.id),
        new Set(files.filter(inside).map((f) => f.id)),
        previewed.id,
      );
      setPreviewId(next);
    }
    return inside;
  };

  /** Items are gone from here: drop them from the visible results now (the index refresh follows). */
  const removeFromView = (removed: (e: Entry) => boolean) => {
    const goneIds = new Set(items.filter(removed).map((e) => e.id));
    const next = neighborAfterRemoval(order, goneIds, sel.focus);
    setResult((res) => {
      const kept = res.items.filter((e) => !removed(e));
      return { ...res, items: kept, total: Math.max(0, res.total - (res.items.length - kept.length)) };
    });
    setSel(sel.focus && goneIds.has(sel.focus) ? only(previewId ? null : next) : (cur) => reconcileSel(cur, order.filter((id) => !goneIds.has(id))));
    setAnalysisVersion((v) => v + 1);
  };

  const moveToTrash = async (targets: Entry[]) => {
    setDialog(null);
    leaving(targets);
    const r = await api.trashItems(targets.map((t) => t.id)).catch((e) => ({ trashed: [] as string[], bytes: 0, failed: [{ path: "", reason: String(e) }] }));
    const gone = targets.filter((t) => r.trashed.includes(t.id));
    if (gone.length) {
      const goneFolders = gone.filter((t) => t.kind === "folder").map((t) => `${t.path}/`);
      removeFromView((e: Entry) => r.trashed.includes(e.id) || goneFolders.some((f) => e.path.startsWith(f)));
      // The folder being viewed (or one of its parents) went to the Trash.
      if (loc.scope === "folder" && crumbs.some((c) => r.trashed.includes(c.id))) {
        setLoc({ scope: "folder", folder: "" });
        setHistory([]);
      }
    }
    const undoKey = isMac ? "⌘Z" : "Ctrl+Z";
    const moved = gone.length === 1 ? t("Moved “{name}” to Trash", { name: gone[0].name }) : tn(gone.length, "Moved {n} item to Trash", "Moved {n} items to Trash");
    if (!r.failed.length) flash(`${moved} · ${t("{key} to undo", { key: undoKey })}`, 3000);
    else if (!gone.length)
      flash(
        targets.length === 1 ? t("Couldn't move to Trash: {reason}", { reason: r.failed[0].reason }) : t("Nothing was moved to Trash ({reason})", { reason: r.failed[0].reason }),
        5000,
      );
    else flash(`${moved} · ${tn(r.failed.length, "{n} item couldn't be moved ({reason})", "{n} items couldn't be moved ({reason})", { reason: r.failed[0].reason })}`, 6000);
  };

  const renamed = (entry: Entry, newId: string, name: string) => {
    setDialog(null);
    const path = entry.path.includes("/") ? `${entry.path.slice(0, entry.path.lastIndexOf("/"))}/${name}` : name;
    setResult((res) => ({ ...res, items: res.items.map((e) => (e.id === entry.id ? { ...e, id: newId, name, path } : e)) }));
    selectOnly(newId);
    if (previewId === entry.id) setPreviewId(newId);
    if (loc.scope === "folder" && loc.folder === entry.id) setLoc({ scope: "folder", folder: newId });
    flash(t("Renamed"));
  };

  const requestTransfer = (op: TransferOp, targets: Entry[]) => {
    setMenu(null);
    if (targets.length) setDialog({ kind: "transfer", op, entries: targets });
  };

  /** Move / Copy finished (the backend already updated its index). */
  const transferred = (op: TransferOp, entries: Entry[], r: TransferResult, plan: TransferPlan) => {
    setDialog(null);
    if (op === "move" && r.done.length) {
      const moved = new Set(r.done.map((d) => d.id));
      const goneFolders = entries.filter((t) => t.kind === "folder" && moved.has(t.id)).map((t) => `${t.path}/`);
      const out = (e: Entry) => moved.has(e.id) || goneFolders.some((f) => e.path.startsWith(f));
      leaving(entries.filter((e) => moved.has(e.id)));
      removeFromView(out);
      if (loc.scope === "folder" && crumbs.some((c) => moved.has(c.id))) {
        setLoc({ scope: "folder", folder: "" });
        setHistory([]);
      }
    } else if (r.done.length) setAnalysisVersion((v) => v + 1);
    const first = entries.find((e) => e.id === r.done[0]?.id);
    const dest = plan.destName;
    const one = r.done.length === 1 && first;
    const head = !r.done.length
      ? op === "move"
        ? t("Nothing was moved")
        : t("Nothing was copied")
      : op === "move"
        ? one
          ? t("Moved “{name}” to “{dest}”", { name: first.name, dest })
          : tn(r.done.length, "Moved {n} item to “{dest}”", "Moved {n} items to “{dest}”", { dest })
        : one
          ? t("Copied “{name}” to “{dest}”", { name: first.name, dest })
          : tn(r.done.length, "Copied {n} item to “{dest}”", "Copied {n} items to “{dest}”", { dest });
    const parts = [head];
    if (r.skipped) parts.push(tn(r.skipped, "{n} item skipped", "{n} items skipped"));
    if (r.failed.length) parts.push(tn(r.failed.length, "{n} item failed ({reason})", "{n} items failed ({reason})", { reason: r.failed[0].reason }));
    if (r.originalsKept.length)
      parts.push(tn(r.originalsKept.length, "{n} original kept ({reason})", "{n} originals kept ({reason})", { reason: r.originalsKept[0].reason }));
    if (r.cancelled) parts.push(t("stopped"));
    if (r.done.length) parts.push(t("{key} to undo", { key: isMac ? "⌘Z" : "Ctrl+Z" }));
    flash(parts.join(" · "), r.failed.length || r.originalsKept.length ? 7000 : 3500);
  };

  const cleanupTrashed = (r: TrashResult) => {
    setAnalysisVersion((v) => v + 1);
    if (r.trashed.length) {
      const gone = new Set(r.trashed);
      removeFromView((e) => gone.has(e.id));
    }
  };

  const startCleanup = (folder: Entry | null) => {
    setMenu(null);
    setPreviewId(null);
    if (folder) setCleanup({ id: folder.id, name: folder.name });
    else if (loc.scope === "folder") setCleanup({ id: loc.folder, name: crumbs.length ? crumbs[crumbs.length - 1].name : status?.rootName ?? "" });
  };

  // ------------------------------------------------- drag and drop

  /** A drag starts on `entry`: the selection if it's part of it, else just it (and it becomes the selection). */
  const dragFrom = (entry: Entry): Entry[] => {
    const { items: what, reselect } = dragSource(entry, selectedItems(sel, items));
    if (reselect) selectOnly(entry.id);
    return what;
  };

  const canDrop = (target: { id: string; path: string; kind?: string; guarded?: boolean }) =>
    canDropInto(target, dragged() ?? [], { readOnly: !canModify });

  /** Dropped on a folder: the same plan → (conflicts) → Move as "Move to…", run by the backend. */
  const dropInto = (target: { id: string; name: string; path: string }, copy: boolean) => {
    const what = dragged() as Entry[] | null;
    endDrag();
    if (!what?.length) return;
    setMenu(null);
    setDialog({
      kind: "transfer",
      op: copy ? "copy" : "move",
      entries: what,
      dest: { key: target.id, label: target.name, where: target.path ? `${status?.rootName ?? ""}/${target.path}` : status?.rootName ?? "" },
    });
  };

  /**
   * A tag was renamed or deleted (the backend already re-published the
   * index, so results and their tags refresh). Counts update now; a deleted
   * tag that was being viewed gives way to the drive, without ghost results.
   */
  const tagsChanged = (msg: string, deleted?: number) => {
    flash(msg, 2600);
    api.tagsList().then(setTags, () => {});
    if (deleted !== undefined && kind === `tag:${deleted}`) {
      setResult(EMPTY);
      navigate({ scope: "folder", folder: "" });
    }
  };

  const setPrivate = async (entry: Entry, isPrivate: boolean) => {
    setDialog(null);
    try {
      await api.setFolderPrivate(entry.id, isPrivate);
      // The index is re-published with the new boundaries; refresh now.
      setIndexVersion((v) => v + 1);
      setAnalysisVersion((v) => v + 1);
      flash(isPrivate ? t("“{name}” is now private", { name: entry.name }) : t("“{name}” is public again", { name: entry.name }), 2500);
    } catch (e) {
      flash(String(e), 4000);
    }
  };

  const setProtected = async (entry: Entry, on: boolean) => {
    setDialog(null);
    try {
      await api.setFolderProtected(entry.id, on);
      setIndexVersion((v) => v + 1);
      flash(on ? t("“{name}” is protected: Mori won't change anything inside it", { name: entry.name }) : t("Protection removed from “{name}”", { name: entry.name }), 3000);
    } catch (e) {
      flash(String(e), 4000);
    }
  };

  const setFavorite = async (targets: Entry[], on: boolean) => {
    try {
      await api.setFavorite(
        targets.map((t) => t.id),
        on,
      );
      // Show it right away; the index refresh that follows confirms it (and updates Favorites and its count).
      const ids = new Set(targets.map((t) => t.id));
      setResult((res) => ({ ...res, items: res.items.map((e) => (ids.has(e.id) ? { ...e, favorite: on } : e)) }));
      flash(
        !on
          ? t("Removed from Favorites")
          : targets.length === 1
            ? t("Added “{name}” to Favorites", { name: targets[0].name })
            : tn(targets.length, "Added {n} item to Favorites", "Added {n} items to Favorites"),
        2200,
      );
    } catch (e) {
      flash(String(e), 4000);
    }
  };

  const startTemporary = async () => {
    const s = await api.openTemporary().catch((e) => (String(e) !== "cancelled" && flash(String(e), 4000), null));
    if (s) rootChanged(s);
  };

  const endTemporary = async () => {
    const s = await api.endTemporary().catch(() => null);
    if (!s) return;
    try {
      sessionStorage.clear();
    } catch {
      // nothing stored
    }
    setHistory([]);
    setSearch("");
    setInspectId(null);
    setPreviewId(null);
    setExtPreview(null);
    // The session's cleanup decisions only ever lived in memory: they end here.
    setCleanup(null);
    forgetRecentDestinations();
    setAnalysisVersion((v) => v + 1);
    rootChanged(s);
    flash(t("Session cleared"), 3000);
  };

  const startPrivate = async (key: string | null) => {
    setNewDrive(null);
    const s = await api.startPrivateInspection(key).catch((e) => (String(e) !== "cancelled" && flash(String(e), 4000), null));
    if (s) rootChanged(s);
  };

  const forgetDrive = async () => {
    setDialog(null);
    try {
      const r = await api.forgetDrive();
      try {
        sessionStorage.clear();
      } catch {
        // nothing stored
      }
      resetThumbs();
      flash(
        tn(
          r.indexes,
          "Mori forgot “{drive}” ({n} index, thumbnails and records). Nothing on the drive was changed.",
          "Mori forgot “{drive}” ({n} indexes, thumbnails and records). Nothing on the drive was changed.",
          { drive: r.drive },
        ),
        6000,
      );
      setStatus(await api.status());
    } catch (e) {
      flash(String(e), 4000);
    }
  };

  const clearSessionData = async () => {
    setDialog(null);
    await api.clearSessionData().catch(() => {});
    try {
      sessionStorage.clear();
    } catch {
      // nothing stored
    }
    setHistory([]);
    setSearch("");
    setAnalysisVersion((v) => v + 1);
    flash(t("Session data cleared: analysis results and folders picked this session are forgotten"), 3500);
  };

  const toggleReadOnly = async () => {
    const next = !readOnly;
    try {
      await api.setReadOnly(next);
      setReadOnlyState(next);
      flash(next ? t("Read-only Mode on: Mori won't change any files") : t("Read-only Mode off"), 2500);
    } catch (e) {
      flash(String(e), 4000);
    }
  };

  /** A click selects (Cmd/Ctrl toggles, Shift ranges); opening is a double-click or Return. */
  const clickItem = (i: number, mods: { toggle: boolean; range: boolean }) => {
    const e = items[i];
    if (e) setSel((cur) => clickSel(cur, order, e.id, mods));
  };

  const selectionTargets = () => (marked.size ? selectedItems(sel, items) : selected >= 0 ? [items[selected]] : []);
  /** What file actions apply to: the previewed file while previewing, else the selection. */
  const actionTargets = () => {
    const previewed = previewId ? files.find((f) => f.id === previewId) : undefined;
    return previewed ? [previewed] : selectionTargets();
  };
  const canModify = !(readOnly || status?.readOnly);

  const copyPath = async (e: Entry) => {
    const text = await api.copyPath(e.id).catch(() => null);
    if (!text) return flash(t("Path unavailable"));
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const ta = document.createElement("textarea");
      ta.value = text;
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      ta.remove();
    }
    flash(t("Path copied"));
  };

  const chooseRoot = async () => {
    // The native picker runs in Rust; the UI never handles a filesystem path.
    const s = await api.chooseRoot().catch(() => null);
    if (s) rootChanged(s);
  };

  const inspectDriveSafely = async (d: ConnectedDrive) => {
    setNewDrive(null);
    const s = await api.openDriveSafely(d.key).catch((e) => (flash(String(e), 4000), null));
    if (s) rootChanged(s);
  };

  const setDrivePreviews = async (on: boolean) => {
    try {
      await api.setDrivePreviews(on);
      resetThumbs();
      setSafeCollapsed(false);
      setStatus(await api.status());
      setIndexVersion((v) => v + 1);
    } catch (e) {
      flash(String(e), 4000);
    }
  };

  const rootChanged = (s: Status) => {
    setCleanup(null);
    selectOnly(null);
    resetThumbs();
    setSafeCollapsed(false);
    setStatus(s);
    setLoc({ scope: "folder", folder: "" });
    setHistory([]);
    setKind("all");
    setSearch("");
    setResult(EMPTY);
    setIndexVersion((v) => v + 1);
  };

  // ------------------------------------------------------------ shortcuts

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (dialog || palette || shortcuts || cleanup || menu || mode !== "browse") return; // dialogs, menus and other views handle their own keys
      const mod = isMac ? e.metaKey : e.ctrlKey;
      const t = e.target as HTMLElement;
      // Never steal keys from text fields.
      const inInput = t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT" || t.isContentEditable;
      // Move to… (M) / Copy to… (Shift+M), Rename (F2): the previewed file or the selection.
      if (!inInput && !mod && !e.altKey && e.key.toLowerCase() === "m" && !e.repeat) {
        const targets = actionTargets();
        if (targets.length) {
          e.preventDefault();
          requestTransfer(e.shiftKey ? "copy" : "move", targets);
        }
        return;
      }
      if (!inInput && e.key === "F2") {
        const targets = actionTargets();
        if (targets.length === 1) {
          e.preventDefault();
          setDialog({ kind: "rename", entry: targets[0] });
        }
        return;
      }
      // Move to Trash: ⌘⌫ on macOS, Delete on Windows/Linux. Never a permanent delete.
      if (!inInput && (isMac ? e.metaKey && e.key === "Backspace" : e.key === "Delete") && !e.repeat) {
        e.preventDefault();
        requestTrash(actionTargets());
        return;
      }
      if (!inInput && (e.key === "i" || (mod && e.key.toLowerCase() === "i")) && !e.altKey) {
        const target = previewId ? files.find((f) => f.id === previewId) : selected >= 0 ? items[selected] : null;
        if (target) {
          e.preventDefault();
          setInspectId((cur) => (cur === target.id ? null : target.id));
        }
        return;
      }
      if (!inInput && !previewId && !mod && !e.altKey && e.key === "f") {
        const targets = selectionTargets().filter((t) => t.kind !== "link");
        if (targets.length) {
          e.preventDefault();
          setFavorite(targets, !targets.every((t) => t.favorite));
        }
        return;
      }
      if (mod && !e.shiftKey && e.key.toLowerCase() === "z" && !inInput) {
        e.preventDefault();
        undoLast();
        return;
      }
      if (mod && e.key.toLowerCase() === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      if (previewId) return; // the preview owns the keyboard
      if (e.key === "Escape") {
        if (pop) setPop(null);
        else if (inInput) {
          setSearch("");
          searchRef.current?.blur();
        } else selectOnly(null);
      } else if (mod && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "a" && !inInput) {
        // Select All in the current view (folder, search results, flattened subfolders).
        e.preventDefault();
        setSel((cur) => selectAll(cur, order));
      } else if (mod && ["1", "2", "3"].includes(e.key)) {
        e.preventDefault();
        setView((["list", "grid", "gallery"] as const)[Number(e.key) - 1]);
      } else if (!inInput && (e.key === "Backspace" || (mod && e.key === "[") || (mod && e.key === "ArrowUp"))) {
        e.preventDefault();
        if (mod && e.key === "ArrowUp" && loc.scope === "folder" && loc.folder) {
          navigate({ scope: "folder", folder: parentFolder });
        } else goBack();
      } else if (inInput && (e.key === "ArrowDown" || e.key === "Enter") && items.length) {
        // Jump from the search field into the results.
        e.preventDefault();
        searchRef.current?.blur();
        selectOnly(items[0].id);
        if (e.key === "Enter") activate(0);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  useEffect(() => {
    if (!menu && !pop) return;
    const close = () => {
      setMenu(null);
      setPop(null);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        close();
      }
    };
    window.addEventListener("click", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKey);
    };
  }, [menu, pop]);

  // --------------------------------------------------------------- render

  const focused = selected >= 0 ? items[selected] : null;
  // Section names are headings in the palette: translated here, where they're built.
  const sView = t("View"),
    sGo = t("Go"),
    sLibrary = t("Library"),
    sTag = t("Tag"),
    sAnalyze = t("Analyze"),
    sSelection = t("Selection"),
    sMori = t("Mori"),
    sEdit = t("Edit"),
    sHelp = t("Help");
  const commands: Command[] = [
    { id: "v-list", title: t("List View"), section: sView, icon: "list", keys: `${isMac ? "⌘" : "Ctrl+"}1`, run: () => setView("list") },
    { id: "v-grid", title: t("Grid View"), section: sView, icon: "grid", keys: `${isMac ? "⌘" : "Ctrl+"}2`, run: () => setView("grid") },
    { id: "v-gallery", title: t("Gallery View"), section: sView, icon: "gallery", keys: `${isMac ? "⌘" : "Ctrl+"}3`, run: () => setView("gallery") },
    { id: "v-sub", title: recursive ? t("Hide Subfolder Contents") : t("Include Subfolders"), section: sView, icon: "folder", run: () => setRecursive(!recursive) },
    { id: "g-search", title: t("Search"), section: sGo, icon: "search", keys: `${isMac ? "⌘" : "Ctrl+"}F`, run: () => setTimeout(() => searchRef.current?.focus(), 0) },
    { id: "g-drive", title: t("Go to {name}", { name: status?.rootName ?? t("Drive") }), section: sGo, icon: "drive", run: () => navigate({ scope: "folder", folder: "" }) },
    ...[...FILTERS, ...CAPTURE_FILTERS].map((f) => ({
      id: `l-${f.kind}`,
      title: t(LIBRARY_TITLE[f.kind]),
      section: sLibrary,
      icon: f.icon,
      run: () => {
        navigate({ scope: "library", folder: "" }, { keepKind: true });
        setKind(f.kind);
      },
    })),
    { id: "l-fav", title: t("Favorites"), section: sLibrary, icon: "star", run: () => (navigate({ scope: "library", folder: "" }, { keepKind: true }), setKind("favorites")) },
    ...tags.map((tag) => ({ id: `t-${tag.id}`, title: tag.name, section: sTag, icon: "tag" as IconName, run: () => (navigate({ scope: "library", folder: "" }, { keepKind: true }), setKind(`tag:${tag.id}`)) })),
    { id: "a-center", title: t("Analysis Center"), section: sAnalyze, icon: "all", run: () => setMode("center") },
    { id: "a-storage", title: t("Storage"), section: sAnalyze, icon: "drive", words: "space size treemap empty folders", run: () => setMode("storage") },
    { id: "a-dupes", title: t("Exact Duplicates"), section: sAnalyze, icon: "duplicate", run: () => setMode("analyzer") },
    { id: "a-similar", title: t("Similar Media"), section: sAnalyze, icon: "gallery", words: "bursts compare", run: () => setMode("similar") },
    { id: "a-meta", title: t("Sensitive Metadata"), section: sAnalyze, icon: "tag", words: "exif gps location privacy", run: () => setMode("metadata") },
    { id: "a-places", title: t("Places"), section: sAnalyze, icon: "pin", words: "map gps", run: () => setMode("places") },
    { id: "a-health", title: t("Media Health"), section: sAnalyze, icon: "warning", words: "broken unsupported", run: () => setMode("health") },
    ...(focused && focused.kind !== "link"
      ? [
          ...(focused.kind !== "folder"
            ? [
                { id: "s-ql", title: t("Quick Look “{name}”", { name: focused.name }), section: sSelection, icon: "gallery" as IconName, keys: t("Space"), run: () => (setPreviewQuick(true), setPreviewId(focused.id)) },
                { id: "s-iso", title: t("Open “{name}” in Isolation", { name: focused.name }), section: sSelection, icon: "shield" as IconName, run: () => (setPreviewIso(true), setPreviewId(focused.id)) },
              ]
            : []),
          { id: "s-info", title: t("Get Info for “{name}”", { name: focused.name }), section: sSelection, icon: "info" as IconName, keys: "I", run: () => setInspectId(focused.id) },
          { id: "s-fav", title: focused.favorite ? t("Remove “{name}” from Favorites", { name: focused.name }) : t("Add “{name}” to Favorites", { name: focused.name }), section: sSelection, icon: "star" as IconName, keys: "F", run: () => setFavorite([focused], !focused.favorite) },
          { id: "s-tags", title: t("Tags for “{name}”…", { name: focused.name }), section: sSelection, icon: "tag" as IconName, run: () => setDialog({ kind: "tags", entries: selectionTargets() }) },
          { id: "s-copy", title: t("Copy Path"), section: sSelection, icon: "copy" as IconName, run: () => copyPath(focused) },
          { id: "s-reveal", title: isMac ? t("Show in Finder") : t("Show in Folder"), section: sSelection, icon: "reveal" as IconName, run: () => api.revealFile(focused.id) },
        ]
      : []),
    { id: "m-ro", title: readOnly ? t("Turn Off Read-only Mode") : t("Turn On Read-only Mode"), section: sMori, icon: "shield", run: toggleReadOnly },
    { id: "m-change", title: t("Change Folder…"), section: sMori, icon: "folder", run: chooseRoot },
    { id: "m-temp", title: t("Temporary Session…"), section: sMori, icon: "clock", words: "browse without indexing ephemeral", run: startTemporary },
    { id: "m-private", title: t("Private Inspection…"), section: sMori, icon: "shield", words: "usb drive unknown read-only temporary safe", run: () => startPrivate(null) },
    { id: "m-integrity", title: t("Integrity Snapshots…"), section: sMori, icon: "check", words: "sha-256 checksum verify", run: () => setDialog({ kind: "integrity" }) },
    { id: "m-privacy", title: t("Privacy & Local Data…"), section: sMori, icon: "lock", words: "clear data reset storage", run: () => setDialog({ kind: "privacy" }) },
    { id: "m-diag", title: t("Diagnostics…"), section: sMori, icon: "info", words: "self-test capabilities offline security", run: () => setDialog({ kind: "diagnostics" }) },
    { id: "m-language", title: t("Language…"), section: sMori, icon: "globe", words: "language idioma langue sprache translate locale", run: () => setDialog({ kind: "language" }) },
    ...(focused && focused.kind !== "folder" && focused.kind !== "link" ? [{ id: "s-sum", title: t("Calculate Checksum of “{name}”", { name: focused.name }), section: sSelection, icon: "check" as IconName, words: "sha-256 hash integrity", run: () => setDialog({ kind: "checksum", entry: focused }) }] : []),
    { id: "m-tags", title: t("Manage Tags…"), section: sMori, icon: "tag", run: () => setDialog({ kind: "manageTags" }) },
    { id: "m-session", title: t("Clear Session Data…"), section: sMori, icon: "close", run: () => setDialog({ kind: "clearSession" }) },
    { id: "m-rescan", title: t("Rescan"), section: sMori, icon: "refresh", run: () => api.rescan() },
    { id: "m-undo", title: t("Undo"), section: sEdit, icon: "refresh", keys: `${isMac ? "⌘" : "Ctrl+"}Z`, run: undoLast },
    { id: "m-history", title: t("Undo History…"), section: sEdit, icon: "clock", run: () => setDialog({ kind: "history" }) },
    ...(focused && focused.kind !== "link" ? [{ id: "s-del", title: t("Delete “{name}” Permanently…", { name: focused.name }), section: sSelection, icon: "close" as IconName, words: "remove erase", run: () => setDialog({ kind: "op", op: "delete", entries: selectionTargets() }) }] : []),
    ...(items.length ? [{ id: "s-all", title: t("Select All"), section: sEdit, icon: "check" as IconName, keys: `${isMac ? "⌘" : "Ctrl+"}A`, run: () => setSel((cur) => selectAll(cur, order)) }] : []),
    ...(focused && canModify
      ? [
          { id: "s-move", title: marked.size > 1 ? tn(marked.size, "Move {n} Item to…", "Move {n} Items to…") : t("Move “{name}” to…", { name: focused.name }), section: sSelection, icon: "folder" as IconName, keys: "M", run: () => requestTransfer("move", selectionTargets()) },
          { id: "s-copyto", title: marked.size > 1 ? tn(marked.size, "Copy {n} Item to…", "Copy {n} Items to…") : t("Copy “{name}” to…", { name: focused.name }), section: sSelection, icon: "copy" as IconName, keys: "⇧M", run: () => requestTransfer("copy", selectionTargets()) },
          { id: "s-rename", title: t("Rename “{name}”…", { name: focused.name }), section: sSelection, icon: "rename" as IconName, keys: "F2", run: () => setDialog({ kind: "rename", entry: focused }) },
        ]
      : []),
    ...(focused?.kind === "folder" ? [{ id: "s-clean", title: t("Quick Cleanup of “{name}”…", { name: focused.name }), section: sSelection, icon: "gallery" as IconName, words: "keep mark trash review photos", run: () => startCleanup(focused) }] : []),
    ...(loc.scope === "folder" ? [{ id: "m-clean", title: t("Quick Cleanup of This Folder…"), section: sMori, icon: "gallery" as IconName, words: "keep mark trash review photos", run: () => startCleanup(null) }] : []),
    { id: "m-keys", title: t("Keyboard Shortcuts"), section: sHelp, icon: "command", words: "keys help", run: () => setShortcuts(true) },
  ];

  if (!info) return <div className="app loading" />;
  const drivePrompt = newDrive && (
    <ModalFrame onCancel={() => setNewDrive(null)}>
          <div className="dialog-icon">
            <Icon name="drive" size={20} />
          </div>
          <h2>{t("“{name}” connected", { name: newDrive.label })}</h2>
          <p>
            {t(
              "Inspect it safely with Mori: only names, sizes and dates are indexed. Nothing on the drive is opened or decoded until you choose to generate previews.",
            )}
          </p>
          <p className="dialog-note">
            {tx("{strong} also makes the session read-only and keeps no Mori index, previews, history or drive record after you end it.", {
              strong: <strong>{t("Temporary inspection")}</strong>,
            })}
          </p>
          <div className="dialog-actions">
            <button className="btn" onClick={() => setNewDrive(null)}>
              {t("Not Now")}
            </button>
            <button className="btn" onClick={() => inspectDriveSafely(newDrive)}>
              {t("Inspect Safely")}
            </button>
            <button className="btn primary" onClick={() => startPrivate(newDrive.key)} autoFocus>
              {t("Temporary Inspection")}
            </button>
          </div>
        </ModalFrame>
  );
  if (!status?.hasRoot)
    return (
      <>
        <Welcome info={info} onChoose={chooseRoot} onPrivate={() => startPrivate(null)} />
        {drivePrompt}
        {toast && <div className="toast">{toast}</div>}
      </>
    );

  const browsing = mode === "browse";
  const searching = debounced.length > 0;
  const folderScope = loc.scope === "folder";
  const folderName = crumbs.length ? crumbs[crumbs.length - 1].name : status.rootName;
  const driveWide = !folderScope || (searching && searchGlobal);
  const recursiveView = folderScope && recursive;
  const searchPlaceholder = !folderScope
    ? t("Search {what} in {drive}…", { what: inSentence(viewTitle(kind, tags)), drive: status.rootName })
    : searchGlobal
      ? t("Search all of {drive}…", { drive: status.rootName })
      : recursive
        ? t("Search in {folder} and subfolders…", { folder: folderName })
        : t("Search in {folder}…", { folder: folderName });
  const showLocation = recursiveView || searching || !folderScope;
  const count = (k: ViewKind) => (stats && !k.startsWith("tag:") && k !== "favorites" ? (k === "all" ? stats.files : stats[k as Exclude<KindFilter, "all">]) : undefined);

  const openPop = (kind: "sort" | "more") => (ev: React.MouseEvent) => {
    ev.stopPropagation();
    const r = (ev.currentTarget as HTMLElement).getBoundingClientRect();
    setMenu(null);
    setPop((p) => (p?.kind === kind ? null : { kind, x: kind === "sort" ? r.right : r.left, y: kind === "sort" ? r.bottom + 6 : r.top - 6 }));
  };
  const clearCache = () =>
    api.clearCache().then(() => {
      resetThumbs();
      flash(t("Thumbnails and index cleared"));
    });
  const pageTitle = searching && driveWide ? t("Results for “{query}”", { query: debounced }) : !folderScope ? viewTitle(kind, tags) : folderName;
  const pageNote =
    searching && driveWide
      ? t("in all of {drive}", { drive: status.rootName })
      : !folderScope
        ? t("across {drive}", { drive: status.rootName })
        : searching
          ? t("results for “{query}”", { query: debounced })
          : "";
  const canGoBack = history.length > 0 || !!search || (loc.scope === "folder" && !!loc.folder);
  // Parent trail shown above the title (the current folder is the title itself).
  const trail = folderScope && !(searching && driveWide) ? crumbs.slice(0, -1) : [];

  return (
    <div className={`app ${isMac ? "mac" : ""}`}>
      <aside className="sidebar">
        <div className="sidebar-head" data-tauri-drag-region>
          <div className="brand" data-tauri-drag-region>
            <Logo size={20} />
            <span data-tauri-drag-region>Mori</span>
          </div>
        </div>
        <nav onDragOver={(e) => dragged() && edgeScroll(e.currentTarget, e.clientY)}>
          <button
            className={`side-item ${browsing && loc.scope === "folder" && !loc.folder && !searching ? "on" : ""}`}
            onClick={() => navigate({ scope: "folder", folder: "" })}
            title={status.rootName}
            {...dropTarget({ id: "", name: status.rootName, path: "", kind: "folder" }, canDrop, dropInto)}
          >
            <Icon name="drive" />
            <span className="truncate">{status.rootName}</span>
          </button>
          <div className="side-heading">{t("Library")}</div>
          {FILTERS.filter((f) => f.kind !== "other" || (stats?.other ?? 0) > 0).map((f) => (
            <button
              key={f.kind}
              className={`side-item ${browsing && loc.scope === "library" && kind === f.kind && !searching ? "on" : ""}`}
              onClick={() => {
                navigate({ scope: "library", folder: "" }, { keepKind: true });
                setKind(f.kind);
              }}
            >
              <Icon name={f.icon} />
              <span>{t(LIBRARY_TITLE[f.kind])}</span>
              <span className="count">{count(f.kind) !== undefined ? fmtNum(count(f.kind)!) : undefined}</span>
            </button>
          ))}
          {CAPTURE_FILTERS.filter((f) => (count(f.kind) ?? 0) > 0).map((f) => (
            <button
              key={f.kind}
              className={`side-item ${browsing && loc.scope === "library" && kind === f.kind && !searching ? "on" : ""}`}
              onClick={() => {
                navigate({ scope: "library", folder: "" }, { keepKind: true });
                setKind(f.kind);
              }}
              title={t("Recognised from file names and the system's screen-capture mark. Right-click a file to correct it.")}
            >
              <Icon name={f.icon} />
              <span>{t(f.label)}</span>
              <span className="count">{count(f.kind) !== undefined ? fmtNum(count(f.kind)!) : undefined}</span>
            </button>
          ))}
          <button
            className={`side-item ${browsing && loc.scope === "library" && kind === "favorites" && !searching ? "on" : ""}`}
            onClick={() => {
              navigate({ scope: "library", folder: "" }, { keepKind: true });
              setKind("favorites");
            }}
            title={t("Files and folders you marked as favorites")}
          >
            <Icon name="star" />
            <span>{t("Favorites")}</span>
            {(stats?.favorites ?? 0) > 0 && <span className="count">{fmtNum(stats!.favorites)}</span>}
          </button>
          {tags.length > 0 && (
            <div className="side-heading with-action">
              {t("Tags")}
              <button className="icon-btn tiny" onClick={() => setDialog({ kind: "manageTags" })} title={t("Manage tags")} aria-label={t("Manage tags")}>
                <Icon name="more" size={12} />
              </button>
            </div>
          )}
          <TagList
            tags={tags}
            active={browsing && loc.scope === "library" && kind.startsWith("tag:") && !searching ? Number(kind.slice(4)) : null}
            canEdit={!status.temporary}
            onOpen={(tag) => {
              navigate({ scope: "library", folder: "" }, { keepKind: true });
              setKind(`tag:${tag.id}`);
            }}
            onRename={(tag) => setDialog({ kind: "renameTag", tag })}
            onDelete={(tag) => setDialog({ kind: "deleteTag", tag })}
          />
          <button className={`side-heading as-button ${mode === "center" ? "on" : ""}`} onClick={() => setMode("center")} title={t("All analyses")}>
            {t("Analyze")}
          </button>
          <button className={`side-item ${mode === "storage" ? "on" : ""}`} onClick={() => setMode("storage")} title={t("Where the space goes, largest items, empty folders")}>
            <Icon name="drive" />
            <span>{t("Storage")}</span>
          </button>
          <button className={`side-item ${mode === "analyzer" ? "on" : ""}`} onClick={() => setMode("analyzer")} title={t("Exact byte-identical files")}>
            <Icon name="duplicate" />
            <span>{t("Duplicates")}</span>
          </button>
          <button className={`side-item ${mode === "similar" ? "on" : ""}`} onClick={() => setMode("similar")} title={t("Visually similar photos and videos")}>
            <Icon name="gallery" />
            <span>{t("Similar Media")}</span>
          </button>
          <button className={`side-item ${mode === "metadata" ? "on" : ""}`} onClick={() => setMode("metadata")} title={t("Location, people, device and other revealing metadata")}>
            <Icon name="tag" />
            <span>{t("Sensitive Metadata")}</span>
          </button>
          <button className={`side-item ${mode === "places" ? "on" : ""}`} onClick={() => setMode("places")} title={t("Where photos and videos were taken (offline map)")}>
            <Icon name="pin" />
            <span>{t("Places")}</span>
          </button>
          <button className={`side-item ${mode === "health" ? "on" : ""}`} onClick={() => setMode("health")} title={t("Broken, unsupported and risk-flagged media")}>
            <Icon name="warning" />
            <span>{t("Media Health")}</span>
          </button>
          {topFolders.length > 0 && <div className="side-heading">{t("Folders")}</div>}
          <FolderTree
            roots={topFolders}
            activeId={browsing && folderScope && !searching ? loc.folder : null}
            ancestors={folderScope ? crumbs.slice(0, -1).map((c) => c.id) : []}
            version={indexVersion}
            canDrop={canDrop}
            onDrop={(t, copy) => dropInto(t, copy)}
            onOpen={(id) => navigate({ scope: "folder", folder: id })}
            onContextMenu={(e, entry) => {
              e.preventDefault();
              setPop(null);
              setMenu({ x: e.clientX, y: e.clientY, entry, targets: [entry] });
            }}
          />
        </nav>
        {(readOnly || status.readOnly) && (
          <button
            className="readonly-chip"
            // During Private Inspection read-only is part of the session: the chip
            // never changes the saved setting unless that setting is what's on.
            onClick={readOnly ? toggleReadOnly : undefined}
            disabled={!readOnly}
            title={readOnly ? t("Read-only Mode is on: Mori won't change any files. Click to turn it off.") : t("Read-only for this Private Inspection. It ends with the session.")}
          >
            <Icon name="shield" size={12} /> {t("Read-only Mode")}
          </button>
        )}
        <div className="side-footer">
          {status.scanning ? (
            <div className="scan-status">
              <span className="dot-spinner" />
              <span className="truncate">{tn(status.scanCount, "Scanning… {n} file", "Scanning… {n} files")}</span>
            </div>
          ) : (
            <div className="scan-status">
              <span className="truncate">{tn(status.fileCount, "{n} file indexed", "{n} files indexed")}</span>
            </div>
          )}
          <button className="icon-btn" onClick={() => api.rescan()} disabled={status.scanning} title={t("Rescan")} aria-label={t("Rescan")}>
            <Icon name="refresh" size={15} />
          </button>
          <button className={`icon-btn ${pop?.kind === "more" ? "on" : ""}`} onClick={openPop("more")} title={t("More")} aria-label={t("More")}>
            <Icon name="more" size={16} />
          </button>
        </div>
      </aside>

      <main className="content" style={browsing ? undefined : { display: "none" }}>
        <header className="topbar" data-tauri-drag-region>
          <label className="search">
            <Icon name="search" size={15} />
            <input
              ref={searchRef}
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              placeholder={searchPlaceholder}
              title={searchPlaceholder}
              spellCheck={false}
              autoCorrect="off"
              autoCapitalize="off"
            />
            {search ? (
              <button className="clear" onClick={() => setSearch("")} title={t("Clear")} aria-label={t("Clear search")}>
                <Icon name="close" size={11} />
              </button>
            ) : (
              <kbd>{isMac ? "⌘F" : "Ctrl F"}</kbd>
            )}
          </label>
          <div className="segmented filters" role="tablist" aria-label={t("File type")}>
            {FILTERS.map((f) => (
              <button key={f.kind} className={kind === f.kind ? "on" : ""} onClick={() => setKind(f.kind)} title={t(f.label)} role="tab" aria-selected={kind === f.kind}>
                <Icon name={f.icon} size={14} />
                <span className="label">{t(f.label)}</span>
              </button>
            ))}
          </div>
          <div className="spacer" data-tauri-drag-region />
          <div className="icon-group" aria-label={t("View")}>
            {([
              ["list", msg("List")],
              ["grid", msg("Grid")],
              ["gallery", msg("Gallery")],
            ] as const).map(([v, label], i) => (
              <button
                key={v}
                className={`icon-btn ${view === v ? "on" : ""}`}
                onClick={() => setView(v)}
                title={`${t(label)} (${isMac ? "⌘" : "Ctrl+"}${i + 1})`}
                aria-label={t(label)}
                aria-pressed={view === v}
              >
                <Icon name={v} size={15} />
              </button>
            ))}
          </div>
          <button className={`dropdown ${pop?.kind === "sort" ? "open" : ""}`} onClick={openPop("sort")} title={t("Sort")} aria-haspopup="menu">
            <Icon name={desc ? "down" : "up"} size={13} />
            <span className="sort-label">{t(SORTS.find((s) => s.key === sort)?.label ?? "")}</span>
            <Icon name="chevronDown" size={13} />
          </button>
        </header>

        {status.temporary && (
          <div className="session-banner" role="status">
            <Icon name={status.privateInspection ? "shield" : "clock"} size={13} />
            <span className="safe-title">{status.privateInspection ? t("PRIVATE INSPECTION") : t("TEMPORARY SESSION")}</span>
            {status.privateInspection ? (
              <>
                <span className="safe-stat">
                  {t("Read-only")} {status.readOnly ? "✓" : "—"}
                </span>
                <span className="safe-stat">{t("Temporary session")} ✓</span>
                <span className="safe-stat">{status.safeMode ? t("Automatic decoding Off") : t("Automatic decoding On")}</span>
                <span className="safe-stat">{t("Persistent indexing Off")}</span>
                <span className="safe-stat">{t("Network required No")}</span>
              </>
            ) : (
              <span className="safe-stat">{t("No Mori history, index or previews are kept after the session ends")}</span>
            )}
            <span className="spacer" />
            <button className="btn small" onClick={endTemporary}>
              {t("End Session")}
            </button>
          </div>
        )}
        {status.safeMode &&
          (safeCollapsed ? (
            <button className="safe-banner collapsed" onClick={() => setSafeCollapsed(false)} title={t("Safe Inspection Mode")}>
              <Icon name="shield" size={12} /> {t("Safe Inspection Mode")}
            </button>
          ) : (
            <div className="safe-banner" role="status">
              <Icon name="shield" size={13} />
              <span className="safe-title">{t("SAFE INSPECTION MODE")}</span>
              <span className="safe-stat">{t("Files indexed {count}", { count: status.fileCount })}</span>
              <span className="safe-stat">{t("Media decoded {count}", { count: status.decoded })}</span>
              <span className="spacer" />
              <button className="btn small" onClick={() => setDrivePreviews(true)} title={t("Allow thumbnails and previews for this drive")}>
                {t("Generate previews")}
              </button>
              <button className="btn small ghost" onClick={() => setSafeCollapsed(true)} title={t("Keep browsing names, sizes and dates only")}>
                {t("Browse metadata only")}
              </button>
            </div>
          ))}

        <div className="page-head">
          <div className="crumbs">
            <button className="icon-btn back" onClick={goBack} disabled={!canGoBack} title={t("Back")} aria-label={t("Back")}>
              <Icon name="left" size={14} />
            </button>
            {trail.length > 0 || (folderScope && crumbs.length > 0) ? (
              <>
                <button className="crumb" onClick={() => navigate({ scope: "folder", folder: "" }, { keepKind: true })}>
                  {status.rootName}
                </button>
                {trail.map((c) => (
                  <span key={c.id} className="crumb-wrap">
                    <Icon name="chevron" size={11} className="crumb-sep" />
                    <button className="crumb" onClick={() => navigate({ scope: "folder", folder: c.id }, { keepKind: true })}>
                      {c.name}
                    </button>
                  </span>
                ))}
              </>
            ) : (
              <span className="crumb">{folderScope && !searching ? t("Drive") : status.rootName}</span>
            )}
          </div>
          <div className="title-row">
            <h1 className="page-title" title={pageTitle}>
              {pageTitle}
              {pageNote && <span className="scope-note">{pageNote}</span>}
              {folderScope && !(searching && searchGlobal) && result.protectedScope && (
                <span className="private-chip" title={t("Never Modify: Mori won't rename, move or delete anything here.")}>
                  <Icon name="shield" size={11} /> {t("Never Modify")}
                </span>
              )}
              {folderScope && !(searching && searchGlobal) && result.privateScope && (
                <span className="private-chip" title={t(PRIVATE_HINT)}>
                  <Icon name="lock" size={11} /> {t("Private")}
                </span>
              )}
            </h1>
            {searching && folderScope && (
              <div className="segmented small" role="radiogroup" aria-label={t("Search scope")}>
                <button className={!searchGlobal ? "on" : ""} onClick={() => setSearchGlobal(false)} title={t("Search only in {folder}", { folder: folderName })}>
                  {t("In {folder}", { folder: folderName })}
                </button>
                <button className={searchGlobal ? "on" : ""} onClick={() => setSearchGlobal(true)} title={t("Search the whole drive")}>
                  {t("Entire drive")}
                </button>
              </div>
            )}
            {folderScope && !(searching && searchGlobal) && (
              <label className="toggle" title={t("Show every file in this folder and all of its subfolders together")}>
                <input type="checkbox" checked={recursive} onChange={(e) => setRecursive(e.target.checked)} />
                <span className="switch" aria-hidden />
                {t("Include subfolders")}
              </label>
            )}
            <div className="item-count" aria-live="polite">
              {marked.size > 0 && <span className="selected-count">{t("{count} selected", { count: marked.size })} · </span>}
              {tn(result.total, "{n} item", "{n} items")}
              {result.truncated && ` · ${t("first {count}", { count: items.length })}`}
            </div>
            {folderScope && !searching && files.length > 0 && (
              <button className="btn small ghost" onClick={() => startCleanup(null)} title={t("Go through these files one by one: Keep or Mark for Trash. Nothing moves until you confirm.")}>
                <Icon name="gallery" size={13} /> {t("Quick Cleanup")}
              </button>
            )}
          </div>
        </div>

        {items.length ? (
          <FileView
            items={items}
            view={view}
            focus={selected}
            selectedIds={marked}
            locationKey={`${loc.scope}|${loc.folder}|${kind}|${debounced}|${recursiveView}|${searchGlobal}`}
            keyboardActive={!previewId && browsing && !dialog && !cleanup && !menu}
            sort={sort}
            desc={desc}
            showLocation={showLocation}
            baseLabel={driveWide ? status.rootName : folderName}
            onSort={(k) => (k === sort ? setDesc((d) => !d) : (setSort(k), setDesc(false)))}
            onMove={(i, extend) => setSel((cur) => moveTo(cur, order, i, extend))}
            onClear={() => selectOnly(null)}
            onActivate={(i) => {
              selectOnly(items[i]?.id ?? null);
              activate(i);
            }}
            onQuickLook={(i) => {
              const e = items[i];
              if (!e || e.kind === "folder" || e.kind === "link") return;
              setPreviewQuick(true);
              setPreviewId(e.id);
            }}
            onClickItem={clickItem}
            onFavorite={status.temporary ? undefined : (i) => items[i] && items[i].kind !== "link" && setFavorite([items[i]], !items[i].favorite)}
            onDragStartItem={(i) => (items[i] ? dragFrom(items[i]) : [])}
            canDrop={canDrop}
            onDrop={(t, copy) => dropInto(t, copy)}
            onContextMenu={(e, i) => {
              const entry = items[i];
              let targets = [entry];
              if (marked.has(entry.id) && marked.size > 1) targets = items.filter((x) => marked.has(x.id));
              else selectOnly(entry.id);
              setPop(null);
              setMenu({ x: e.clientX, y: e.clientY, entry, targets });
            }}
          />
        ) : status.scanning && !status.fileCount ? (
          <div className="empty">
            <span className="dot-spinner large" />
            <p>{t("Scanning {name}…", { name: status.rootName })}</p>
          </div>
        ) : searching ? (
          <div className="empty">
            <Logo size={64} className="mark" />
            <h2>{t("No files found")}</h2>
            <p>
              {driveWide
                ? t("Nothing matches “{query}”.", { query: debounced })
                : recursive
                  ? t("Nothing matches “{query}” in {folder} or its subfolders.", { query: debounced, folder: folderName })
                  : t("Nothing matches “{query}” in {folder}.", { query: debounced, folder: folderName })}
            </p>
          </div>
        ) : kind === "all" ? (
          <div className="empty">
            <Logo size={64} className="mark" />
            <h2>{recursiveView ? t("No files here") : t("This folder is empty")}</h2>
            {recursiveView && <p>{t("No files in this folder or its subfolders.")}</p>}
          </div>
        ) : (
          <div className="empty">
            <Icon name={kind === "favorites" ? "star" : kind.startsWith("tag:") ? "tag" : filterIcon(kind as KindFilter)} size={34} stroke={1.3} />
            <p>
              {recursiveView
                ? t("No {what} here or in subfolders.", { what: inSentence(viewTitle(kind, tags)) })
                : t("No {what} here.", { what: inSentence(viewTitle(kind, tags)) })}
            </p>
          </div>
        )}
        {marked.size > 1 && !previewId && (
          <SelectionBar
            count={marked.size}
            targets={selectionTargets()}
            canModify={canModify}
            onMove={() => requestTransfer("move", selectionTargets())}
            onCopy={() => requestTransfer("copy", selectionTargets())}
            onTags={() => setDialog({ kind: "tags", entries: selectionTargets() })}
            onFavorite={(on) => setFavorite(selectionTargets().filter((t) => t.kind !== "link"), on)}
            onTrash={() => requestTrash(selectionTargets())}
            onClear={() => selectOnly(null)}
          />
        )}
      </main>

      <Analyzer active={mode === "analyzer"} version={analysisVersion} onToast={flash} onSwitch={() => setMode("similar")} />
      <AnalysisCenter active={mode === "center"} onOpen={(t) => setMode(t)} />
      <StorageView
        active={mode === "storage"}
        version={indexVersion}
        rootName={status.rootName}
        onOpenFile={(e) => setExtPreview({ items: [e], index: 0, isolated: false })}
        onOpenFolder={(id) => navigate({ scope: "folder", folder: id })}
        onToast={flash}
      />
      <HealthView
        active={mode === "health"}
        rootName={status.rootName}
        onOpen={(items, index) => setExtPreview({ items, index, isolated: true })}
        onInfo={setInspectId}
        onToast={flash}
      />
      <MetadataAnalyzer
        active={mode === "metadata" || mode === "places"}
        view={mode === "places" ? "map" : "list"}
        onToast={flash}
        onInfo={setInspectId}
        onSwitch={(to) => setMode(to === "map" ? "places" : "metadata")}
      />
      <SimilarAnalyzer active={mode === "similar"} version={analysisVersion} onToast={flash} onSwitch={() => setMode("analyzer")} />

      {previewId && previewIndex >= 0 && (
        <Preview
          items={files}
          index={previewIndex}
          onIndex={(i) => {
            setPreviewId(files[i].id);
            selectOnly(files[i].id);
          }}
          onClose={() => setPreviewId(null)}
          onCopyPath={copyPath}
          onError={flash}
          isolated={previewIso}
          quick={previewQuick}
          active={!dialog && !menu && !palette && !inspectId}
          actions={{
            canModify,
            onTrash: (e) => requestTrash([e]),
            onMove: (e) => requestTransfer("move", [e]),
            onCopy: (e) => requestTransfer("copy", [e]),
            onRename: (e) => setDialog({ kind: "rename", entry: e }),
            onInfo: (e) => setInspectId(e.id),
            onChecksum: (e) => setDialog({ kind: "checksum", entry: e }),
            onFavorite: (e) => setFavorite([e], !e.favorite),
          }}
        />
      )}

      {extPreview && (
        <Preview
          items={extPreview.items}
          index={extPreview.index}
          onIndex={(i) => setExtPreview((p) => p && { ...p, index: i })}
          onClose={() => setExtPreview(null)}
          onCopyPath={copyPath}
          onError={flash}
          isolated={extPreview.isolated}
        />
      )}

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          entry={menu.entry}
          count={menu.targets.length}
          onTrash={() => requestTrash(menu.targets)}
          onMoveTo={() => requestTransfer("move", menu.targets)}
          onCopyTo={() => requestTransfer("copy", menu.targets)}
          onCleanup={() => startCleanup(menu.entry)}
          canModify={canModify}
          onDelete={() => setDialog({ kind: "op", op: "delete", entries: menu.targets })}
          onRename={() => setDialog({ kind: "rename", entry: menu.entry })}
          onPrivacy={() => (menu.entry.private ? setPrivate(menu.entry, false) : setDialog({ kind: "private", entry: menu.entry }))}
          onInfo={() => setInspectId(menu.entry.id)}
          onProtect={() => (menu.entry.protected ? setDialog({ kind: "unprotect", entry: menu.entry }) : setProtected(menu.entry, true))}
          onPreview={() => (menu.entry.kind === "folder" ? navigate({ scope: "folder", folder: menu.entry.id }) : setPreviewId(menu.entry.id))}
          onFavorite={() => setFavorite(menu.targets, !menu.entry.favorite)}
          onChecksum={() => setDialog({ kind: "checksum", entry: menu.entry })}
          onCompare={() => setDialog({ kind: "compare", a: menu.targets[0], b: menu.targets[1] })}
          compareOk={menu.targets.length === 2 && menu.targets.every((t) => t.kind !== "folder" && t.kind !== "link")}
          onSnapshot={() =>
            api.integritySave(menu.entry.id).then(
              () => (flash(t("Creating integrity snapshot… (progress in Integrity Snapshots)"), 3500), setDialog({ kind: "integrity" })),
              (e) => flash(String(e), 4000),
            )
          }
          onTags={() => setDialog({ kind: "tags", entries: menu.targets })}
          onCapture={async () => {
            const e = menu.entry;
            try {
              await api.setCaptureOverride(e.id, e.capture ? "not" : "yes");
              flash(
                e.capture
                  ? e.capture === 2
                    ? t("“{name}” is no longer listed as a screen recording", { name: e.name })
                    : t("“{name}” is no longer listed as a screenshot", { name: e.name })
                  : e.kind === "video"
                    ? t("“{name}” is now listed as a screen recording", { name: e.name })
                    : t("“{name}” is now listed as a screenshot", { name: e.name }),
                2500,
              );
            } catch (err) {
              flash(String(err), 4000);
            }
          }}
          onIsolate={() => {
            setPreviewIso(true);
            setPreviewId(menu.entry.id);
          }}
          onCopy={() => copyPath(menu.entry)}
          onError={flash}
        />
      )}

      {pop?.kind === "sort" && (
        <div className="menu" style={{ top: pop.y, left: Math.max(8, pop.x - 210) }} role="menu" onClick={(e) => e.stopPropagation()}>
          <div className="menu-label">{t("Sort by")}</div>
          {SORTS.map((o) => (
            <button
              key={o.key}
              role="menuitemradio"
              aria-checked={sort === o.key}
              onClick={() => {
                setSort(o.key);
                setPop(null);
              }}
            >
              {t(o.label)}
              {sort === o.key && <Icon name="check" size={14} className="check" />}
            </button>
          ))}
          <div className="sep" />
          {[false, true].map((d) => (
            <button
              key={String(d)}
              role="menuitemradio"
              aria-checked={desc === d}
              onClick={() => {
                setDesc(d);
                setPop(null);
              }}
            >
              <Icon name={d ? "down" : "up"} size={14} />
              {d ? t("Descending") : t("Ascending")}
              {desc === d && <Icon name="check" size={14} className="check" />}
            </button>
          ))}
        </div>
      )}

      {pop?.kind === "more" && (
        <div className="menu" style={{ bottom: window.innerHeight - pop.y, left: pop.x - 8 }} role="menu" onClick={(e) => e.stopPropagation()}>
          <button
            onClick={() => {
              setPop(null);
              chooseRoot();
            }}
          >
            <Icon name="folder" size={14} /> {t("Change Folder…")}
          </button>
          <button
            role="menuitemcheckbox"
            aria-checked={readOnly}
            onClick={() => {
              setPop(null);
              toggleReadOnly();
            }}
            title={t("When on, Mori refuses every change to your files (enforced by the backend).")}
          >
            <Icon name="shield" size={14} /> {t("Read-only Mode")}
            {readOnly && <Icon name="check" size={14} className="check" />}
          </button>
          <button
            disabled={status.scanning}
            onClick={() => {
              setPop(null);
              clearCache();
            }}
            title={t("Delete Mori's thumbnails and index (your files are not touched)")}
          >
            <Icon name="refresh" size={14} /> {t("Clear Cache")}
          </button>
          <div className="sep" />
          <button
            onClick={() => {
              setPop(null);
              startPrivate(null);
            }}
            title={t("Read-only, temporary, no automatic decoding: for drives and folders you don't trust")}
          >
            <Icon name="shield" size={14} /> {t("Private Inspection…")}
          </button>
          <div className="sep" />
          <button
            onClick={() => {
              setPop(null);
              setDialog({ kind: "integrity" });
            }}
          >
            <Icon name="check" size={14} /> {t("Integrity Snapshots…")}
          </button>
          <button
            onClick={() => {
              setPop(null);
              setDialog({ kind: "privacy" });
            }}
          >
            <Icon name="lock" size={14} /> {t("Privacy & Local Data…")}
          </button>
          <button
            onClick={() => {
              setPop(null);
              setDialog({ kind: "diagnostics" });
            }}
          >
            <Icon name="info" size={14} /> {t("Diagnostics…")}
          </button>
          <button
            onClick={() => {
              setPop(null);
              setDialog({ kind: "language" });
            }}
          >
            <Icon name="globe" size={14} /> {t("Language…")}
          </button>
          <button
            onClick={() => {
              setPop(null);
              setDialog({ kind: "history" });
            }}
            title={t("Changes Mori made to your files in this session, with Undo where possible")}
          >
            <Icon name="clock" size={14} /> {t("Undo History…")}
          </button>
          <button
            onClick={() => {
              setPop(null);
              startTemporary();
            }}
            title={t("Open a folder without saving an index, thumbnails or anything else about it")}
          >
            <Icon name="clock" size={14} /> {t("Temporary Session…")}
          </button>
          <button
            onClick={() => {
              setPop(null);
              setDialog({ kind: "clearSession" });
            }}
            title={t("Forget analysis results and folders picked in this session (files, caches and records are kept)")}
          >
            <Icon name="close" size={14} /> {t("Clear Session Data…")}
          </button>
          <button
            disabled={status.temporary}
            onClick={() => {
              setPop(null);
              setDialog({ kind: "forget" });
            }}
            title={t("Remove everything Mori stores about this drive (nothing on the drive is changed)")}
          >
            <Icon name="drive" size={14} /> {t("Forget This Drive…")}
          </button>
        </div>
      )}

      {dialog?.kind === "private" && (
        <PrivateDialog entry={dialog.entry} onCancel={() => setDialog(null)} onConfirm={() => setPrivate(dialog.entry, true)} />
      )}
      {dialog?.kind === "unprotect" && (
        <ModalFrame onCancel={() => setDialog(null)}>
          <div className="dialog-icon">
            <Icon name="shield" size={20} />
          </div>
          <h2>{t("Remove protection from “{name}”?", { name: dialog.entry.name })}</h2>
          <p>{t("Mori will again allow renaming, moving and deleting items inside this folder when you ask it to.")}</p>
          <div className="dialog-actions">
            <button className="btn" onClick={() => setDialog(null)} autoFocus>
              {t("Cancel")}
            </button>
            <button className="btn primary" onClick={() => setProtected(dialog.entry, false)}>
              {t("Remove Protection")}
            </button>
          </div>
        </ModalFrame>
      )}
      {dialog?.kind === "tags" && (
        <TagDialog
          entries={dialog.entries.map((e) => items.find((x) => x.id === e.id) ?? e)}
          tags={tags}
          onClose={() => setDialog(null)}
          onDone={(m) => {
            flash(m, 2200);
            api.tagsList().then(setTags);
          }}
        />
      )}
      {dialog?.kind === "manageTags" && (
        <TagManager
          tags={tags}
          onClose={() => setDialog(null)}
          onChanged={(m, deleted) => tagsChanged(m, deleted)}
        />
      )}
      {dialog?.kind === "renameTag" && (
        <TagRenameDialog tag={dialog.tag} tags={tags} onCancel={() => setDialog(null)} onDone={(m) => (setDialog(null), tagsChanged(m))} />
      )}
      {dialog?.kind === "deleteTag" && <TagDeleteDialog tag={dialog.tag} onCancel={() => setDialog(null)} onDone={(m) => (setDialog(null), tagsChanged(m, dialog.tag.id))} />}
      {dialog?.kind === "op" && (
        <OperationPreview
          op={dialog.op}
          entries={dialog.entries}
          onCancel={() => setDialog(null)}
          onTrash={(ids) => moveToTrash(dialog.entries.filter((e) => ids.includes(e.id)))}
          onDeleted={(msg, ids) => {
            setDialog(null);
            if (previewId && ids.includes(previewId)) setPreviewId(null);
            selectOnly(null);
            setAnalysisVersion((v) => v + 1);
            flash(msg, 5000);
          }}
        />
      )}
      {dialog?.kind === "checksum" && <ChecksumDialog entry={dialog.entry} canSave={!status.temporary} onClose={() => setDialog(null)} onNotice={(m) => flash(m, 2500)} />}
      {dialog?.kind === "compare" && <CompareDialog a={dialog.a} b={dialog.b} onClose={() => setDialog(null)} onNotice={(m) => flash(m, 2500)} />}
      {dialog?.kind === "integrity" && <IntegrityPanel onClose={() => setDialog(null)} onNotice={(m) => flash(m, 3000)} />}
      {dialog?.kind === "diagnostics" && <Diagnostics onClose={() => setDialog(null)} />}
      {dialog?.kind === "language" && <LanguageDialog onClose={() => setDialog(null)} />}
      {dialog?.kind === "privacy" && (
        <PrivacyData
          onClose={() => setDialog(null)}
          onCleared={(m, reset) => {
            flash(m, 5000);
            resetThumbs();
            api.tagsList().then(setTags, () => {});
            setIndexVersion((v) => v + 1);
            if (reset) {
              setDialog(null);
              api.status().then(setStatus);
              setReadOnlyState(false);
            }
          }}
        />
      )}
      {dialog?.kind === "history" && <HistoryPanel onClose={() => setDialog(null)} onUndone={(m) => (flash(m, 3500), setAnalysisVersion((v) => v + 1))} />}
      {dialog?.kind === "forget" && (
        <ModalFrame onCancel={() => setDialog(null)}>
          <div className="dialog-icon">
            <Icon name="drive" size={20} />
          </div>
          <h2>{t("Forget “{name}”?", { name: status.rootName })}</h2>
          <p>
            {t(
              "Mori will remove everything it stores about this drive: its index, thumbnails, and its private, protected, favorite, tag and screenshot records. Then the drive is closed.",
            )}
          </p>
          <p className="dialog-note">
            <strong>{t("Nothing on the drive is deleted or changed.")}</strong> {t("This only affects Mori's own data on this computer. Tag names themselves are kept.")}
          </p>
          <div className="dialog-actions">
            <button className="btn" onClick={() => setDialog(null)} autoFocus>
              {t("Cancel")}
            </button>
            <button className="btn primary" onClick={forgetDrive}>
              {t("Forget Drive")}
            </button>
          </div>
        </ModalFrame>
      )}
      {dialog?.kind === "clearSession" && (
        <ModalFrame onCancel={() => setDialog(null)}>
          <h2>{t("Clear session data?")}</h2>
          <p>
            {t(
              "Forgets what this session holds in memory: analysis results, folders picked for analysis, recent locations and search. Running analyses are stopped.",
            )}
          </p>
          <p className="dialog-note">
            {t(
              "Not the same as Clear Cache (thumbnails and indexes), Forget This Drive (Mori's records about a drive) or deleting files — none of those happen here.",
            )}
          </p>
          <div className="dialog-actions">
            <button className="btn" onClick={() => setDialog(null)} autoFocus>
              {t("Cancel")}
            </button>
            <button className="btn primary" onClick={clearSessionData}>
              {t("Clear Session Data")}
            </button>
          </div>
        </ModalFrame>
      )}
      {dialog?.kind === "rename" && (
        <RenameDialog entry={dialog.entry} onCancel={() => setDialog(null)} onDone={(id, name) => renamed(dialog.entry, id, name)} />
      )}

      {dialog?.kind === "transfer" && (
        <TransferDialog
          op={dialog.op}
          entries={dialog.entries}
          rootName={status.rootName}
          current={loc.scope === "folder" ? loc.folder : ""}
          dest={dialog.dest}
          onClose={() => setDialog(null)}
          onDone={(r, plan) => transferred(dialog.op, dialog.entries, r, plan)}
        />
      )}

      {cleanup && (
        <Suspense fallback={<div className="cleanup" />}>
        <QuickCleanup
          key={cleanup.id}
          folder={cleanup}
          status={status}
          readOnly={readOnly}
          browserSort={{ sort, desc }}
          recursiveDefault={recursive}
          indexVersion={indexVersion}
          onClose={() => setCleanup(null)}
          onToast={flash}
          onTrashed={cleanupTrashed}
          onUndo={() => (undoLast(), setCleanup(null))}
        />
        </Suspense>
      )}

      {inspectId && <Inspector id={inspectId} onClose={() => setInspectId(null)} onNotice={(m) => flash(m, 3000)} over={!!previewId} />}

      {drivePrompt}

      {palette && (
        <CommandPalette
          commands={commands}
          onClose={() => setPalette(false)}
          onOpenEntry={(e) => {
            if (e.kind === "folder") navigate({ scope: "folder", folder: e.id });
            else if (e.kind === "link") setInspectId(e.id);
            else setExtPreview({ items: [e], index: 0, isolated: false });
          }}
        />
      )}
      {shortcuts && <ShortcutsHelp onClose={() => setShortcuts(false)} />}

      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}

/**
 * Drag-and-drop target props for a folder outside the file grid (sidebar
 * rows, the drive). Sets `data-drop="ok" | "no"` while a Mori drag is over
 * it; the drop itself goes through the backend Move.
 */
function dropTarget<T extends { id: string; name: string; path: string; kind?: string; guarded?: boolean }>(
  target: T,
  canDrop: (t: T) => { ok: true } | { ok: false; reason: string },
  onDrop: (t: T, copy: boolean) => void,
  onHover?: () => void,
  onLeave?: (id: string) => void,
) {
  const mark = (el: HTMLElement, v: "ok" | "no" | null) => {
    if (v) el.dataset.drop = v;
    else delete el.dataset.drop;
  };
  return {
    onDragOver: (e: React.DragEvent<HTMLElement>) => {
      if (!dragged()) return;
      const v = canDrop(target);
      e.preventDefault();
      e.stopPropagation();
      e.dataTransfer.dropEffect = v.ok ? (e.altKey ? "copy" : "move") : "none";
      mark(e.currentTarget, v.ok ? "ok" : "no");
      e.currentTarget.title = v.ok ? t("Move into “{name}”", { name: target.name }) : v.reason;
      onHover?.();
    },
    onDragLeave: (e: React.DragEvent<HTMLElement>) => {
      if (e.currentTarget.contains(e.relatedTarget as Node | null)) return;
      mark(e.currentTarget, null);
      e.currentTarget.title = target.name;
      onLeave?.(target.id);
    },
    onDrop: (e: React.DragEvent<HTMLElement>) => {
      e.preventDefault();
      e.stopPropagation();
      mark(e.currentTarget, null);
      e.currentTarget.title = target.name;
      onLeave?.(target.id);
      if (dragged() && canDrop(target).ok) onDrop(target, e.altKey);
      endDrag();
    },
  };
}

/**
 * Expandable folder tree for the sidebar. Children come from the existing
 * index query (`subfolders`) and are loaded only when a node is expanded.
 */
function FolderTree({
  roots,
  activeId,
  ancestors,
  version,
  canDrop,
  onDrop,
  onOpen,
  onContextMenu,
}: {
  roots: Entry[];
  activeId: string | null;
  ancestors: string[];
  version: number;
  canDrop: (target: Entry) => { ok: true } | { ok: false; reason: string };
  onDrop: (target: Entry, copy: boolean) => void;
  onOpen: (id: string) => void;
  onContextMenu: (e: React.MouseEvent, entry: Entry) => void;
}) {
  const [expanded, setExpanded] = useState<Set<string>>(() => new Set());
  const [children, setChildren] = useState<Map<string, Entry[]>>(() => new Map());

  // The index changed: drop cached children (they'll reload on demand).
  useEffect(() => setChildren(new Map()), [version]);

  // Reveal the current folder: expand every ancestor.
  const ancestorKey = ancestors.join("/");
  useEffect(() => {
    if (!ancestors.length) return;
    setExpanded((prev) => {
      const next = new Set(prev);
      ancestors.forEach((id) => next.add(id));
      return next;
    });
  }, [ancestorKey]);

  // Load children for expanded nodes that aren't cached yet.
  useEffect(() => {
    let alive = true;
    for (const id of expanded) {
      if (children.has(id)) continue;
      api.subfolders(id).then((kids) => alive && setChildren((m) => new Map(m).set(id, kids)));
    }
    return () => {
      alive = false;
    };
  }, [expanded, children]);

  const toggle = (id: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  // Hovering a dragged item over a collapsed folder expands it after a moment
  // (never while the pointer merely passes over).
  const spring = useRef<{ id: string; timer: number } | null>(null);
  const stopSpring = () => {
    if (spring.current) window.clearTimeout(spring.current.timer);
    spring.current = null;
  };
  useEffect(() => () => stopSpring(), []);
  const hoverExpand = (f: Entry) => {
    if (expanded.has(f.id) || spring.current?.id === f.id) return;
    stopSpring();
    spring.current = {
      id: f.id,
      timer: window.setTimeout(() => {
        spring.current = null;
        setExpanded((prev) => new Set(prev).add(f.id));
      }, SPRING_MS),
    };
  };

  const rows: React.ReactNode[] = [];
  const INDENT = 14;
  const walk = (nodes: Entry[], depth: number) => {
    for (const f of nodes) {
      const open = expanded.has(f.id);
      const kids = children.get(f.id);
      const leaf = kids !== undefined && kids.length === 0;
      rows.push(
        <div
          key={f.id}
          className={`tree-row ${activeId === f.id ? "on" : ""}`}
          style={{ paddingLeft: depth * INDENT }}
          onContextMenu={(e) => onContextMenu(e, f)}
          {...dropTarget(f, canDrop, onDrop, () => !leaf && hoverExpand(f), (id) => spring.current?.id === id && stopSpring())}
          draggable
          onDragStart={(e) => {
            // A sidebar folder can be dragged too (onto another folder).
            beginDrag([f]);
            e.dataTransfer.effectAllowed = "copyMove";
            e.dataTransfer.setData(DRAG_TYPE, "1");
          }}
          onDragEnd={() => (endDrag(), stopSpring())}
        >
          {Array.from({ length: depth }, (_, k) => (
            <span key={k} className="guide" style={{ left: 11 + k * INDENT }} />
          ))}
          <button
            className={`twisty ${open && !leaf ? "open" : ""} ${leaf ? "leaf" : ""}`}
            onClick={() => toggle(f.id)}
            tabIndex={leaf ? -1 : 0}
            aria-label={open ? t("Collapse {name}", { name: f.name }) : t("Expand {name}", { name: f.name })}
            aria-expanded={!leaf ? open : undefined}
          >
            <Icon name="chevron" size={12} />
          </button>
          <button
            className="label"
            onClick={() => onOpen(f.id)}
            title={f.private ? `${f.name} — ${t(PRIVATE_HINT)}` : f.name}
            aria-label={
              f.private && f.protected
                ? t("{name}, private folder, protected folder", { name: f.name })
                : f.private
                  ? t("{name}, private folder", { name: f.name })
                  : f.protected
                    ? t("{name}, protected folder", { name: f.name })
                    : f.name
            }
          >
            <Icon name="folder" size={15} />
            <span className="truncate">{f.name}</span>
            {(f.private || f.protected) && (
              <span className="tree-marks">
                {f.private && (
                  <span className="tree-mark" role="img" aria-label={t("Private folder")} title={t("Private folder: its contents are hidden from global and recursive views")}>
                    <Icon name="lock" size={13} stroke={1.7} />
                  </span>
                )}
                {f.protected && (
                  <span className="tree-mark" role="img" aria-label={t("Protected folder (Never Modify)")} title={t("Protected folder (Never Modify)")}>
                    <Icon name="shield" size={13} stroke={1.7} />
                  </span>
                )}
              </span>
            )}
          </button>
        </div>,
      );
      if (open && kids?.length) walk(kids, depth + 1);
    }
  };
  walk(roots, 0);
  return <div className="tree">{rows}</div>;
}

function ContextMenu({
  x,
  y,
  entry,
  count,
  onPreview,
  onIsolate,
  onFavorite,
  onChecksum,
  onCompare,
  compareOk,
  onSnapshot,
  onTags,
  onCapture,
  onCopy,
  onRename,
  onPrivacy,
  onInfo,
  onProtect,
  onTrash,
  onMoveTo,
  onCopyTo,
  onCleanup,
  canModify,
  onDelete,
  onError,
}: {
  x: number;
  y: number;
  entry: Entry;
  /** Number of items the menu acts on (more than one: a multi-selection). */
  count: number;
  onPreview: () => void;
  onIsolate: () => void;
  onFavorite: () => void;
  onChecksum: () => void;
  onCompare: () => void;
  compareOk: boolean;
  onSnapshot: () => void;
  onTags: () => void;
  onCapture: () => void;
  onCopy: () => void;
  onRename: () => void;
  onPrivacy: () => void;
  onInfo: () => void;
  onProtect: () => void;
  onTrash: () => void;
  onMoveTo: () => void;
  onCopyTo: () => void;
  onCleanup: () => void;
  /** Read-only Mode / Private Inspection: changes to files are unavailable. */
  canModify: boolean;
  onDelete: () => void;
  onError: (msg: string) => void;
}) {
  const folder = entry.kind === "folder";
  const ro = canModify ? undefined : t("Read-only: Mori won't change files right now");
  const transfer = (
    <>
      <button onClick={onMoveTo} disabled={!canModify} title={ro}>
        <Icon name="folder" size={14} /> {t("Move to…")}
        <kbd className="menu-kbd">M</kbd>
      </button>
      <button onClick={onCopyTo} disabled={!canModify} title={ro}>
        <Icon name="copy" size={14} /> {t("Copy to…")}
        <kbd className="menu-kbd">⇧M</kbd>
      </button>
    </>
  );
  if (count > 1) {
    return (
      <div className="menu" role="menu" style={{ left: Math.min(x, window.innerWidth - 230), top: Math.max(8, Math.min(y, window.innerHeight - 250)) }} onContextMenu={(e) => e.preventDefault()}>
        <div className="menu-label">{tn(count, "{n} item selected", "{n} items selected")}</div>
        {count === 2 && compareOk && (
          <button onClick={onCompare} title={t("Exact comparison by SHA-256 (not visual similarity)")}>
            <Icon name="check" size={14} /> {t("Compare Integrity")}
          </button>
        )}
        <button onClick={onFavorite}>
          <Icon name="star" size={14} /> {entry.favorite ? t("Remove from Favorites") : t("Add to Favorites")}
        </button>
        <button onClick={onTags}>
          <Icon name="tag" size={14} /> {t("Tags…")}
        </button>
        <div className="sep" />
        {transfer}
        <div className="sep" />
        <button className="danger" onClick={onTrash} disabled={!canModify} title={ro}>
          <Icon name="trash" size={14} /> {tn(count, "Move {n} item to Trash", "Move {n} items to Trash")}
        </button>
        <button className="danger" onClick={onDelete}>
          <Icon name="close" size={14} /> {t("Delete Permanently…")}
        </button>
      </div>
    );
  }
  const style = { left: Math.min(x, window.innerWidth - 230), top: Math.max(8, Math.min(y, window.innerHeight - 640)) };
  return (
    <div className="menu" role="menu" style={style} onContextMenu={(e) => e.preventDefault()}>
      {entry.kind !== "link" && (
        <button onClick={onPreview}>
          <Icon name={folder ? "folder" : "gallery"} size={14} /> {folder ? t("Open Folder") : t("Preview")}
        </button>
      )}
      {folder && (
        <button onClick={onCleanup} title={t("Go through this folder's files one by one: Keep or Mark for Trash. Nothing moves until you confirm.")}>
          <Icon name="gallery" size={14} /> {t("Quick Cleanup…")}
        </button>
      )}
      {!folder && entry.kind !== "link" && (
        <button onClick={onIsolate} title={t("View worker-rendered copies only. The original is never opened or run.")}>
          <Icon name="shield" size={14} /> {t("Open in Isolation")}
        </button>
      )}
      {!folder && entry.kind !== "link" && (
        <button onClick={() => api.openFile(entry.id).catch((e) => onError(String(e)))}>
          <Icon name="external" size={14} /> {t("Open with Default App")}
        </button>
      )}
      <button onClick={() => api.revealFile(entry.id)}>
        <Icon name="reveal" size={14} /> {isMac ? t("Show in Finder") : t("Show in Folder")}
      </button>
      <button onClick={onCopy}>
        <Icon name="copy" size={14} /> {t("Copy Path")}
      </button>
      <button onClick={onInfo}>
        <Icon name="info" size={14} /> {t("Get Info")}
        <kbd className="menu-kbd">I</kbd>
      </button>
      {(entry.kind === "photo" || entry.kind === "video") && (
        <button onClick={onCapture} title={t("Mori guesses this from the name and the system's screen-capture mark; correct it here. The file isn't changed.")}>
          <Icon name={entry.kind === "video" ? "recording" : "screenshot"} size={14} />
          {entry.capture
            ? entry.capture === 2
              ? t("Not a Screen Recording")
              : t("Not a Screenshot")
            : entry.kind === "video"
              ? t("Mark as Screen Recording")
              : t("Mark as Screenshot")}
        </button>
      )}
      {entry.kind !== "link" && (
        <>
          <div className="sep" />
          <button onClick={onFavorite}>
            <Icon name="star" size={14} /> {entry.favorite ? t("Remove from Favorites") : t("Add to Favorites")}
            <kbd className="menu-kbd">F</kbd>
          </button>
          <button onClick={onTags}>
            <Icon name="tag" size={14} /> {t("Tags…")}
          </button>
          {folder ? (
            <button onClick={onSnapshot} title={t("Record SHA-256 of every file here (saved in Mori's local data) to verify later")}>
              <Icon name="check" size={14} /> {t("Create Integrity Snapshot")}
            </button>
          ) : (
            <button onClick={onChecksum}>
              <Icon name="check" size={14} /> {t("Calculate Checksum")}
            </button>
          )}
        </>
      )}
      <div className="sep" />
      <button onClick={onRename} disabled={!canModify} title={ro}>
        <Icon name="rename" size={14} /> {t("Rename…")}
        <kbd className="menu-kbd">F2</kbd>
      </button>
      {transfer}
      {folder && (
        <button onClick={onPrivacy} title={entry.private ? undefined : t(PRIVATE_HINT)}>
          <Icon name="lock" size={14} /> {entry.private ? t("Make Public") : t("Make Private…")}
        </button>
      )}
      {folder && (
        <button onClick={onProtect} title={entry.protected ? undefined : t("Mori will refuse to rename, move or delete anything inside this folder.")}>
          <Icon name="shield" size={14} /> {entry.protected ? t("Remove Protection…") : t("Never Modify")}
        </button>
      )}
      <div className="sep" />
      <button className="danger" onClick={onTrash} disabled={!canModify} title={ro}>
        <Icon name="trash" size={14} /> {t("Move to Trash")}
        <kbd className="menu-kbd">{isMac ? "⌘⌫" : t("Del")}</kbd>
      </button>
      <button className="danger" onClick={onDelete} title={t("Bypasses the Trash. Shows exactly what will be deleted first.")}>
        <Icon name="close" size={14} /> {t("Delete Permanently…")}
      </button>
    </div>
  );
}

/** Shown while several items are selected: the common file actions, one click away. */
function SelectionBar({
  count,
  targets,
  canModify,
  onMove,
  onCopy,
  onTags,
  onFavorite,
  onTrash,
  onClear,
}: {
  count: number;
  targets: Entry[];
  canModify: boolean;
  onMove: () => void;
  onCopy: () => void;
  onTags: () => void;
  onFavorite: (on: boolean) => void;
  onTrash: () => void;
  onClear: () => void;
}) {
  const ro = canModify ? undefined : t("Read-only: Mori won't change files right now");
  const allFav = targets.length > 0 && targets.every((x) => x.favorite || x.kind === "link");
  return (
    <div className="selection-bar" role="toolbar" aria-label={t("{count} selected", { count })}>
      <span className="count">{t("{count} selected", { count })}</span>
      <button className="btn small ghost" onClick={onMove} disabled={!canModify} title={ro ?? `${t("Move to…")} (M)`}>
        <Icon name="folder" size={13} /> {t("Move to…")}
      </button>
      <button className="btn small ghost" onClick={onCopy} disabled={!canModify} title={ro ?? `${t("Copy to…")} (⇧M)`}>
        <Icon name="copy" size={13} /> {t("Copy to…")}
      </button>
      <button className="btn small ghost" onClick={onTags} title={t("Tags…")}>
        <Icon name="tag" size={13} /> {t("Tags…")}
      </button>
      <button className="btn small ghost" onClick={() => onFavorite(!allFav)} title={`${allFav ? t("Remove from Favorites") : t("Add to Favorites")} (F)`}>
        <Icon name="star" size={13} /> {allFav ? t("Unfavorite") : t("Favorite")}
      </button>
      <span className="sep" />
      <button className="btn small ghost danger" onClick={onTrash} disabled={!canModify} title={ro ?? `${t("Move to Trash")} (${isMac ? "⌘⌫" : t("Delete")})`}>
        <Icon name="trash" size={13} /> {t("Move to Trash")}
      </button>
      <button className="icon-btn" onClick={onClear} title={t("Clear selection (Esc)")} aria-label={t("Clear selection")}>
        <Icon name="close" size={13} />
      </button>
    </div>
  );
}

/** Making a folder private changes what Mori shows elsewhere: confirm once. */
function PrivateDialog({ entry, onCancel, onConfirm }: { entry: Entry; onCancel: () => void; onConfirm: () => void }) {
  return (
    <ModalFrame onCancel={onCancel}>
      <div className="dialog-icon">
        <Icon name="lock" size={20} />
      </div>
      <h2>{t("Make “{name}” private?", { name: entry.name })}</h2>
      <p>
        {t(
          "Its contents will no longer appear in global search, media categories, counts, recursive parent-folder views or analyses started from a parent folder. You can still open the folder normally.",
        )}
      </p>
      <p className="dialog-note">{t("This only changes what Mori shows. Nothing on disk is modified, moved or encrypted.")}</p>
      <div className="dialog-actions">
        <button className="btn" onClick={onCancel}>
          {t("Cancel")}
        </button>
        <button className="btn primary" onClick={onConfirm} autoFocus>
          {t("Make Private")}
        </button>
      </div>
    </ModalFrame>
  );
}


function RenameDialog({ entry, onCancel, onDone }: { entry: Entry; onCancel: () => void; onDone: (id: string, name: string) => void }) {
  const [name, setName] = useState(entry.name);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    // Select the name without its extension, like Finder and Explorer.
    const el = input.current!;
    el.focus();
    const dot = entry.kind === "folder" ? -1 : entry.name.lastIndexOf(".");
    el.setSelectionRange(0, dot > 0 ? dot : entry.name.length);
  }, []);
  const submit = async (ev: React.FormEvent) => {
    ev.preventDefault();
    const next = name.normalize("NFC");
    if (next === entry.name) return onCancel();
    setBusy(true);
    try {
      onDone(await api.renameItem(entry.id, next), next);
    } catch (e) {
      setError(String(e));
      setBusy(false);
    }
  };
  return (
    <ModalFrame onCancel={onCancel}>
      <form onSubmit={submit}>
        <h2>{t("Rename")}</h2>
        <input
          ref={input}
          className="text-input"
          value={name}
          onChange={(e) => {
            setName(e.target.value);
            setError(null);
          }}
          spellCheck={false}
          autoCorrect="off"
          autoCapitalize="off"
          maxLength={255}
          aria-label={t("New name")}
        />
        {error && <p className="field-error">{error}</p>}
        <div className="dialog-actions">
          <button type="button" className="btn" onClick={onCancel}>
            {t("Cancel")}
          </button>
          <button type="submit" className="btn primary" disabled={busy || !name.trim()}>
            {t("Rename")}
          </button>
        </div>
      </form>
    </ModalFrame>
  );
}

function Welcome({ info, onChoose, onPrivate }: { info: InitInfo; onChoose: () => void; onPrivate: () => void }) {
  return (
    <div className={`welcome ${isMac ? "mac" : ""}`} data-tauri-drag-region>
      <div className="welcome-card">
        <Logo size={72} />
        <h1>Mori</h1>
        <p>{t("Choose the drive or folder you want to browse. Mori remembers it, works fully offline, and only changes files when you ask it to.")}</p>
        <button className="btn primary large" onClick={onChoose}>
          {t("Choose Folder…")}
        </button>
        <button className="btn large" onClick={onPrivate} title={t("Read-only, temporary, no automatic decoding, nothing remembered afterwards")}>
          <Icon name="shield" size={14} /> {t("Private Inspection…")}
        </button>
        {info.translocated && (
          <p className="hint">
            {t(
              "macOS is running Mori from a temporary quarantine location, so it can't see which drive it's on. See the README section “Portable use on macOS” to fix this once.",
            )}
          </p>
        )}
      </div>
    </div>
  );
}

// Session-only memory of where the user was, so a UI reload lands in place.
interface SessionState {
  /** Only restored within the same app launch (i.e. after a UI reload). */
  launchId: number;
  loc: Location;
  kind: ViewKind;
  search: string;
}

const validKind = (k: unknown): k is ViewKind =>
  typeof k === "string" && ([...FILTERS, ...CAPTURE_FILTERS].some((f) => f.kind === k) || k === "favorites" || /^tag:\d{1,9}$/.test(k));

function readSession(launchId: number): SessionState | null {
  try {
    const raw = sessionStorage.getItem("mori.location");
    const v = raw ? (JSON.parse(raw) as SessionState) : null;
    if (!v || v.launchId !== launchId || typeof v.loc?.folder !== "string" || !["folder", "library"].includes(v.loc.scope)) return null;
    if (!/^[0-9a-f]{0,16}$/.test(v.loc.folder) || typeof v.search !== "string") return null;
    return { launchId, loc: { scope: v.loc.scope, folder: v.loc.folder }, kind: validKind(v.kind) ? v.kind : "all", search: v.search.slice(0, 256) };
  } catch {
    return null;
  }
}

function writeSession(v: SessionState) {
  try {
    sessionStorage.setItem("mori.location", JSON.stringify(v));
  } catch {
    // Storage unavailable: nothing to restore later, which is fine.
  }
}
