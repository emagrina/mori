import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  api,
  isMac,
  resetThumbs,
  setPreviewOpen,
  type Entry,
  type InitInfo,
  type KindFilter,
  type QueryResult,
  type Scope,
  type SortKey,
  type Stats,
  type Status,
  type ViewMode,
} from "./api";
import { FileView } from "./components/FileView";
import { Icon, Logo, type IconName } from "./components/Icon";
import { Preview } from "./components/Preview";

interface Location {
  scope: Scope;
  folder: string;
}

const FILTERS: { kind: KindFilter; label: string; icon: IconName }[] = [
  { kind: "all", label: "All", icon: "all" },
  { kind: "photo", label: "Photos", icon: "photo" },
  { kind: "video", label: "Videos", icon: "video" },
  { kind: "gif", label: "GIFs", icon: "gif" },
  { kind: "document", label: "Documents", icon: "document" },
  { kind: "audio", label: "Audio", icon: "audio" },
  { kind: "other", label: "Other", icon: "other" },
];

const LIBRARY_TITLE: Record<KindFilter, string> = {
  all: "All Files",
  photo: "Photos",
  video: "Videos",
  gif: "GIFs",
  document: "Documents",
  audio: "Audio",
  other: "Other Files",
};

const SORTS: { key: SortKey; label: string }[] = [
  { key: "name", label: "Name" },
  { key: "modified", label: "Date Modified" },
  { key: "created", label: "Date Created" },
  { key: "size", label: "Size" },
  { key: "type", label: "Type" },
];

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
  const [kind, setKind] = useState<KindFilter>("all");
  const [search, setSearch] = useState("");
  const [debounced, setDebounced] = useState("");
  const [result, setResult] = useState<QueryResult>(EMPTY);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [previewId, setPreviewId] = useState<string | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; entry: Entry } | null>(null);
  // Small anchored menus: sort options and the sidebar overflow ("more") menu.
  const [pop, setPop] = useState<{ kind: "sort" | "more"; x: number; y: number } | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [indexVersion, setIndexVersion] = useState(0);
  const searchRef = useRef<HTMLInputElement>(null);
  const settingsLoaded = useRef(false);

  // ------------------------------------------------------------- startup

  useEffect(() => {
    api.init().then((i) => {
      setInfo(i);
      setStatus(i.status);
      if (i.view) setView(i.view);
      if (i.sort) setSort(i.sort);
      if (i.desc !== null) setDesc(i.desc);
      if (i.recursive !== null) setRecursive(i.recursive);
      if (i.searchGlobal !== null) setSearchGlobal(i.searchGlobal);
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
          setToast(`Mori recovered after the video engine stopped responding on “${names[names.length - 1]}”. That file won't be previewed again.`);
          setTimeout(() => setToast(null), 7000);
        }
      });
    });
    let timer: number | undefined;
    const unlisten = [
      listen<Status>("status", (e) => setStatus(e.payload)),
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
    if (settingsLoaded.current && info) writeSession({ launchId: info.launchId, loc, kind, search });
  }, [loc, kind, search, info]);

  // Background video decoding (thumbnail frames) pauses while previewing.
  useEffect(() => setPreviewOpen(previewId !== null), [previewId]);

  useEffect(() => {
    if (!status?.hasRoot) return;
    api.stats().then(setStats);
    api.subfolders("").then(setTopFolders);
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
  const selected = useMemo(() => (selectedId ? items.findIndex((e) => e.id === selectedId) : -1), [items, selectedId]);
  const previewIndex = useMemo(() => (previewId ? files.findIndex((e) => e.id === previewId) : -1), [files, previewId]);
  const crumbs = result.crumbs;
  const parentFolder = crumbs.length > 1 ? crumbs[crumbs.length - 2].id : "";

  // -------------------------------------------------------------- actions

  const navigate = useCallback(
    (next: Location, opts: { keepKind?: boolean } = {}) => {
      setHistory((h) => [...h.slice(-49), loc]);
      setLoc(next);
      setSearch("");
      setSelectedId(null);
      if (!opts.keepKind && next.scope === "folder") setKind("all");
    },
    [loc],
  );

  const goBack = useCallback(() => {
    if (search) return setSearch("");
    const prev = history[history.length - 1];
    if (prev) {
      setHistory((h) => h.slice(0, -1));
      setLoc(prev);
      setSelectedId(null);
    } else if (loc.scope === "folder" && loc.folder) {
      setLoc({ scope: "folder", folder: parentFolder });
    }
  }, [history, loc, search, parentFolder]);

  const activate = useCallback(
    (i: number) => {
      const e = items[i];
      if (!e) return;
      if (e.kind === "folder") navigate({ scope: "folder", folder: e.id }, { keepKind: true });
      else setPreviewId(e.id);
    },
    [items, navigate],
  );

  const flash = (msg: string) => {
    setToast(msg);
    setTimeout(() => setToast((t) => (t === msg ? null : t)), 1800);
  };

  const copyPath = async (e: Entry) => {
    const text = await api.copyPath(e.id).catch(() => null);
    if (!text) return flash("Path unavailable");
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
    flash("Path copied");
  };

  const chooseRoot = async () => {
    // The native picker runs in Rust; the UI never handles a filesystem path.
    const s = await api.chooseRoot().catch(() => null);
    if (!s) return;
    resetThumbs();
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
      const mod = isMac ? e.metaKey : e.ctrlKey;
      const inInput = (e.target as HTMLElement).tagName === "INPUT";
      if (mod && e.key.toLowerCase() === "f") {
        e.preventDefault();
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      if (previewId) return; // the preview owns the keyboard
      if (e.key === "Escape") {
        if (pop) setPop(null);
        else if (menu) setMenu(null);
        else if (inInput) {
          setSearch("");
          searchRef.current?.blur();
        } else setSelectedId(null);
      } else if (mod && ["1", "2", "3"].includes(e.key)) {
        e.preventDefault();
        setView((["gallery", "grid", "list"] as const)[Number(e.key) - 1]);
      } else if (!inInput && (e.key === "Backspace" || (mod && e.key === "[") || (mod && e.key === "ArrowUp"))) {
        e.preventDefault();
        if (mod && e.key === "ArrowUp" && loc.scope === "folder" && loc.folder) {
          navigate({ scope: "folder", folder: parentFolder });
        } else goBack();
      } else if (inInput && (e.key === "ArrowDown" || e.key === "Enter") && items.length) {
        // Jump from the search field into the results.
        e.preventDefault();
        searchRef.current?.blur();
        setSelectedId(items[0].id);
        if (e.key === "Enter") activate(0);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [previewId, menu, pop, loc, items, goBack, navigate, activate, parentFolder]);

  useEffect(() => {
    if (!menu && !pop) return;
    const close = () => {
      setMenu(null);
      setPop(null);
    };
    window.addEventListener("click", close);
    window.addEventListener("blur", close);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("blur", close);
    };
  }, [menu, pop]);

  // --------------------------------------------------------------- render

  if (!info) return <div className="app loading" />;
  if (!status?.hasRoot) return <Welcome info={info} onChoose={chooseRoot} />;

  const searching = debounced.length > 0;
  const folderScope = loc.scope === "folder";
  const folderName = crumbs.length ? crumbs[crumbs.length - 1].name : status.rootName;
  const driveWide = !folderScope || (searching && searchGlobal);
  const recursiveView = folderScope && recursive;
  const searchPlaceholder = !folderScope
    ? `Search ${LIBRARY_TITLE[kind].toLowerCase()} in ${status.rootName}…`
    : searchGlobal
      ? `Search all of ${status.rootName}…`
      : recursive
        ? `Search in ${folderName} and subfolders…`
        : `Search in ${folderName}…`;
  const showLocation = recursiveView || searching || !folderScope;
  const count = (k: KindFilter) => (stats ? (k === "all" ? stats.files : stats[k]) : undefined);

  const openPop = (kind: "sort" | "more") => (ev: React.MouseEvent) => {
    ev.stopPropagation();
    const r = (ev.currentTarget as HTMLElement).getBoundingClientRect();
    setMenu(null);
    setPop((p) => (p?.kind === kind ? null : { kind, x: kind === "sort" ? r.right : r.left, y: kind === "sort" ? r.bottom + 6 : r.top - 6 }));
  };
  const clearCache = () =>
    api.clearCache().then(() => {
      resetThumbs();
      flash("Thumbnails and index cleared");
    });
  const pageTitle = searching && driveWide ? `Results for “${debounced}”` : !folderScope ? LIBRARY_TITLE[kind] : folderName;
  const pageNote = searching && driveWide ? `in all of ${status.rootName}` : !folderScope ? `across ${status.rootName}` : searching ? `results for “${debounced}”` : "";
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
        <nav>
          <button
            className={`side-item ${loc.scope === "folder" && !loc.folder && !searching ? "on" : ""}`}
            onClick={() => navigate({ scope: "folder", folder: "" })}
            title={status.rootName}
          >
            <Icon name="drive" />
            <span className="truncate">{status.rootName}</span>
          </button>
          <div className="side-heading">Library</div>
          {FILTERS.filter((f) => f.kind !== "other" || (stats?.other ?? 0) > 0).map((f) => (
            <button
              key={f.kind}
              className={`side-item ${loc.scope === "library" && kind === f.kind && !searching ? "on" : ""}`}
              onClick={() => {
                navigate({ scope: "library", folder: "" }, { keepKind: true });
                setKind(f.kind);
              }}
            >
              <Icon name={f.icon} />
              <span>{LIBRARY_TITLE[f.kind]}</span>
              <span className="count">{count(f.kind)?.toLocaleString()}</span>
            </button>
          ))}
          {topFolders.length > 0 && <div className="side-heading">Folders</div>}
          <FolderTree
            roots={topFolders}
            activeId={folderScope && !searching ? loc.folder : null}
            ancestors={folderScope ? crumbs.slice(0, -1).map((c) => c.id) : []}
            version={indexVersion}
            onOpen={(id) => navigate({ scope: "folder", folder: id })}
            onContextMenu={(e, entry) => {
              e.preventDefault();
              setPop(null);
              setMenu({ x: e.clientX, y: e.clientY, entry });
            }}
          />
        </nav>
        <div className="side-footer">
          {status.scanning ? (
            <div className="scan-status">
              <span className="dot-spinner" />
              <span className="truncate">Scanning… {status.scanCount.toLocaleString()} files</span>
            </div>
          ) : (
            <div className="scan-status">
              <span className="truncate">{status.fileCount.toLocaleString()} files indexed</span>
            </div>
          )}
          <button className="icon-btn" onClick={() => api.rescan()} disabled={status.scanning} title="Rescan" aria-label="Rescan">
            <Icon name="refresh" size={15} />
          </button>
          <button className={`icon-btn ${pop?.kind === "more" ? "on" : ""}`} onClick={openPop("more")} title="More" aria-label="More">
            <Icon name="more" size={16} />
          </button>
        </div>
      </aside>

      <main className="content">
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
              <button className="clear" onClick={() => setSearch("")} title="Clear" aria-label="Clear search">
                <Icon name="close" size={11} />
              </button>
            ) : (
              <kbd>{isMac ? "⌘F" : "Ctrl F"}</kbd>
            )}
          </label>
          <div className="segmented filters" role="tablist" aria-label="File type">
            {FILTERS.map((f) => (
              <button key={f.kind} className={kind === f.kind ? "on" : ""} onClick={() => setKind(f.kind)} title={f.label} role="tab" aria-selected={kind === f.kind}>
                <Icon name={f.icon} size={14} />
                <span className="label">{f.label}</span>
              </button>
            ))}
          </div>
          <div className="spacer" data-tauri-drag-region />
          <div className="icon-group" aria-label="View">
            {(["gallery", "grid", "list"] as const).map((v, i) => (
              <button
                key={v}
                className={`icon-btn ${view === v ? "on" : ""}`}
                onClick={() => setView(v)}
                title={`${v[0].toUpperCase() + v.slice(1)} (${isMac ? "⌘" : "Ctrl+"}${i + 1})`}
                aria-label={v}
                aria-pressed={view === v}
              >
                <Icon name={v} size={15} />
              </button>
            ))}
          </div>
          <button className={`dropdown ${pop?.kind === "sort" ? "open" : ""}`} onClick={openPop("sort")} title="Sort" aria-haspopup="menu">
            <Icon name={desc ? "down" : "up"} size={13} />
            <span className="sort-label">{SORTS.find((s) => s.key === sort)?.label}</span>
            <Icon name="chevronDown" size={13} />
          </button>
        </header>

        <div className="page-head">
          <div className="crumbs">
            <button className="icon-btn back" onClick={goBack} disabled={!canGoBack} title="Back" aria-label="Back">
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
              <span className="crumb">{folderScope && !searching ? "Drive" : status.rootName}</span>
            )}
          </div>
          <div className="title-row">
            <h1 className="page-title" title={pageTitle}>
              {pageTitle}
              {pageNote && <span className="scope-note">{pageNote}</span>}
            </h1>
            {searching && folderScope && (
              <div className="segmented small" role="radiogroup" aria-label="Search scope">
                <button className={!searchGlobal ? "on" : ""} onClick={() => setSearchGlobal(false)} title={`Search only in ${folderName}`}>
                  In {folderName}
                </button>
                <button className={searchGlobal ? "on" : ""} onClick={() => setSearchGlobal(true)} title="Search the whole drive">
                  Entire drive
                </button>
              </div>
            )}
            {folderScope && !(searching && searchGlobal) && (
              <label className="toggle" title="Show every file in this folder and all of its subfolders together">
                <input type="checkbox" checked={recursive} onChange={(e) => setRecursive(e.target.checked)} />
                <span className="switch" aria-hidden />
                Include subfolders
              </label>
            )}
            <div className="item-count">
              {result.total.toLocaleString()} {result.total === 1 ? "item" : "items"}
              {result.truncated && ` · first ${items.length.toLocaleString()}`}
            </div>
          </div>
        </div>

        {items.length ? (
          <FileView
            items={items}
            view={view}
            selected={selected}
            locationKey={`${loc.scope}|${loc.folder}|${kind}|${debounced}|${recursiveView}|${searchGlobal}`}
            keyboardActive={!previewId}
            sort={sort}
            desc={desc}
            showLocation={showLocation}
            baseLabel={driveWide ? status.rootName : folderName}
            onSort={(k) => (k === sort ? setDesc((d) => !d) : (setSort(k), setDesc(false)))}
            onSelect={(i) => setSelectedId(items[i]?.id ?? null)}
            onActivate={activate}
            onContextMenu={(e, i) => {
              setPop(null);
              setMenu({ x: e.clientX, y: e.clientY, entry: items[i] });
            }}
          />
        ) : status.scanning && !status.fileCount ? (
          <div className="empty">
            <span className="dot-spinner large" />
            <p>Scanning {status.rootName}…</p>
          </div>
        ) : searching ? (
          <div className="empty">
            <Logo size={64} className="mark" />
            <h2>No files found</h2>
            <p>
              Nothing matches “{debounced}”{driveWide ? "" : recursive ? ` in ${folderName} or its subfolders` : ` in ${folderName}`}.
            </p>
          </div>
        ) : kind === "all" ? (
          <div className="empty">
            <Logo size={64} className="mark" />
            <h2>{recursiveView ? "No files here" : "This folder is empty"}</h2>
            {recursiveView && <p>No files in this folder or its subfolders.</p>}
          </div>
        ) : (
          <div className="empty">
            <Icon name={FILTERS.find((f) => f.kind === kind)!.icon} size={34} stroke={1.3} />
            <p>
              No {LIBRARY_TITLE[kind].toLowerCase()} here{recursiveView ? " or in subfolders" : ""}.
            </p>
          </div>
        )}
      </main>

      {previewId && previewIndex >= 0 && (
        <Preview
          items={files}
          index={previewIndex}
          onIndex={(i) => {
            setPreviewId(files[i].id);
            setSelectedId(files[i].id);
          }}
          onClose={() => setPreviewId(null)}
          onCopyPath={copyPath}
          onError={flash}
        />
      )}

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          entry={menu.entry}
          onPreview={() => (menu.entry.kind === "folder" ? navigate({ scope: "folder", folder: menu.entry.id }) : setPreviewId(menu.entry.id))}
          onCopy={() => copyPath(menu.entry)}
          onError={flash}
        />
      )}

      {pop?.kind === "sort" && (
        <div className="menu" style={{ top: pop.y, left: Math.max(8, pop.x - 210) }} role="menu" onClick={(e) => e.stopPropagation()}>
          <div className="menu-label">Sort by</div>
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
              {o.label}
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
              {d ? "Descending" : "Ascending"}
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
            <Icon name="folder" size={14} /> Change Folder…
          </button>
          <button
            disabled={status.scanning}
            onClick={() => {
              setPop(null);
              clearCache();
            }}
            title="Delete Mori's thumbnails and index (your files are not touched)"
          >
            <Icon name="refresh" size={14} /> Clear Cache
          </button>
        </div>
      )}

      {toast && <div className="toast">{toast}</div>}
    </div>
  );
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
  onOpen,
  onContextMenu,
}: {
  roots: Entry[];
  activeId: string | null;
  ancestors: string[];
  version: number;
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
        >
          {Array.from({ length: depth }, (_, k) => (
            <span key={k} className="guide" style={{ left: 11 + k * INDENT }} />
          ))}
          <button
            className={`twisty ${open && !leaf ? "open" : ""} ${leaf ? "leaf" : ""}`}
            onClick={() => toggle(f.id)}
            tabIndex={leaf ? -1 : 0}
            aria-label={open ? `Collapse ${f.name}` : `Expand ${f.name}`}
            aria-expanded={!leaf ? open : undefined}
          >
            <Icon name="chevron" size={12} />
          </button>
          <button className="label" onClick={() => onOpen(f.id)} title={f.name}>
            <Icon name="folder" size={15} />
            <span className="truncate">{f.name}</span>
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
  onPreview,
  onCopy,
  onError,
}: {
  x: number;
  y: number;
  entry: Entry;
  onPreview: () => void;
  onCopy: () => void;
  onError: (msg: string) => void;
}) {
  const style = { left: Math.min(x, window.innerWidth - 220), top: Math.min(y, window.innerHeight - 170) };
  const folder = entry.kind === "folder";
  return (
    <div className="menu" style={style} onContextMenu={(e) => e.preventDefault()}>
      <button onClick={onPreview}>
        <Icon name={folder ? "folder" : "gallery"} size={14} /> {folder ? "Open Folder" : "Preview"}
      </button>
      {!folder && (
        <button onClick={() => api.openFile(entry.id).catch((e) => onError(String(e)))}>
          <Icon name="external" size={14} /> Open with Default App
        </button>
      )}
      <button onClick={() => api.revealFile(entry.id)}>
        <Icon name="reveal" size={14} /> {isMac ? "Show in Finder" : "Show in Folder"}
      </button>
      <div className="sep" />
      <button onClick={onCopy}>
        <Icon name="copy" size={14} /> Copy Path
      </button>
    </div>
  );
}

function Welcome({ info, onChoose }: { info: InitInfo; onChoose: () => void }) {
  return (
    <div className={`welcome ${isMac ? "mac" : ""}`} data-tauri-drag-region>
      <div className="welcome-card">
        <Logo size={72} />
        <h1>Mori</h1>
        <p>Choose the drive or folder you want to browse. Mori remembers it, works fully offline, and never changes your files.</p>
        <button className="btn primary large" onClick={onChoose}>
          Choose Folder…
        </button>
        {info.translocated && (
          <p className="hint">
            macOS is running Mori from a temporary quarantine location, so it can't see which drive it's on. See the README section “Portable use on
            macOS” to fix this once.
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
  kind: KindFilter;
  search: string;
}

function readSession(launchId: number): SessionState | null {
  try {
    const raw = sessionStorage.getItem("mori.location");
    const v = raw ? (JSON.parse(raw) as SessionState) : null;
    if (!v || v.launchId !== launchId || typeof v.loc?.folder !== "string" || !["folder", "library"].includes(v.loc.scope)) return null;
    if (!/^[0-9a-f]{0,16}$/.test(v.loc.folder) || typeof v.search !== "string") return null;
    return { launchId, loc: { scope: v.loc.scope, folder: v.loc.folder }, kind: FILTERS.some((f) => f.kind === v.kind) ? v.kind : "all", search: v.search.slice(0, 256) };
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
