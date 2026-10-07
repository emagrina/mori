import { memo, useEffect, useLayoutEffect, useRef, useState } from "react";
import { formatDate, formatShortDate, formatSize, isMac, parentOf, PRIVATE_HINT, typeLabel, type Entry, type SortKey, type ViewMode } from "../api";

/** Folder an item came from, relative to the view ("" = the viewed folder itself). */
const locationOf = (e: Entry) => (e.location ?? parentOf(e.path)).split("/").join(" / ");
import { Icon } from "./Icon";
import { Thumb } from "./Thumb";

interface Props {
  items: Entry[];
  view: ViewMode;
  /** Keyboard focus (the anchor end of Shift ranges is kept by the caller). */
  focus: number;
  /** Ids of every selected item. */
  selectedIds: ReadonlySet<string>;
  /** Changes whenever the location changes, to reset scroll. */
  locationKey: string;
  keyboardActive: boolean;
  sort: SortKey;
  desc: boolean;
  /** Show where each item lives (recursive views, searches, library). */
  showLocation: boolean;
  /** Name shown for items located directly in the viewed folder. */
  baseLabel: string;
  onSort: (key: SortKey) => void;
  /** Arrow keys, Home, End: focus `index`; `extend` (Shift) grows the selection. */
  onMove: (index: number, extend: boolean) => void;
  /** Clear the selection (a click on empty space). */
  onClear: () => void;
  /** Double-click or Return: open the folder or preview the file. */
  onActivate: (i: number) => void;
  /** Space: Mori Quick Look. */
  onQuickLook?: (i: number) => void;
  /** A click selects (it never opens): Cmd/Ctrl toggles, Shift extends a range. */
  onClickItem: (i: number, mods: { toggle: boolean; range: boolean }) => void;
  onContextMenu: (e: React.MouseEvent, i: number) => void;
}

// Keep in sync with --pad-x in styles.css.
const PAD = 28;
const LIST_ROW = 36;
const LIST_HEADER = 32;
/** Grid card: 6px inner padding around a square thumbnail, then a two-line caption. */
const GRID_CHROME = 12;
const GRID_CAPTION = 48;

function layout(view: ViewMode, width: number) {
  const inner = Math.max(0, width - PAD * 2);
  if (view === "list") return { cols: 1, tileW: inner, rowH: LIST_ROW, gap: 0 };
  const min = view === "gallery" ? 230 : 156;
  const gap = view === "gallery" ? 6 : 10;
  const cols = Math.max(1, Math.floor((inner + gap) / (min + gap)));
  const tileW = (inner - gap * (cols - 1)) / cols;
  const tileH = view === "gallery" ? Math.round(tileW * 0.75) : Math.round(tileW - GRID_CHROME) + GRID_CHROME + GRID_CAPTION;
  return { cols, tileW, rowH: tileH + gap, gap };
}

const isMedia = (e: Entry) => e.kind === "photo" || e.kind === "video" || e.kind === "gif";

export function FileView(p: Props) {
  const scroller = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [scrollTop, setScrollTop] = useState(0);
  const { cols, tileW, rowH, gap } = layout(p.view, size.w);
  const rows = Math.ceil(p.items.length / cols);
  // In list view the sticky header occupies LIST_HEADER px of scroll space above the rows.
  const headerH = p.view === "list" ? LIST_HEADER : 0;
  // The page header above already provides the breathing room.
  const top = p.view === "list" ? 4 : 2;
  const height = top + rows * rowH + PAD;

  useLayoutEffect(() => {
    const el = scroller.current!;
    const ro = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useLayoutEffect(() => {
    scroller.current!.scrollTop = 0;
    setScrollTop(0);
  }, [p.locationKey, p.view]);

  // Keep the focused item visible (keyboard navigation, preview next/prev).
  useEffect(() => {
    const el = scroller.current;
    if (!el || p.focus < 0) return;
    const rowTop = headerH + top + Math.floor(p.focus / cols) * rowH;
    if (rowTop - headerH < el.scrollTop) el.scrollTop = rowTop - headerH - (p.view === "list" ? 0 : gap);
    else if (rowTop + rowH > el.scrollTop + el.clientHeight) el.scrollTop = rowTop + rowH - el.clientHeight + PAD / 2;
  }, [p.focus, cols, rowH]);

  // Arrow-key navigation within the grid; Shift extends the selection.
  useEffect(() => {
    if (!p.keyboardActive) return;
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT" || t.isContentEditable || e.metaKey || e.ctrlKey || e.altKey) return;
      const n = p.items.length;
      if (!n) return;
      const cur = p.focus;
      const step: Record<string, number> = { ArrowRight: 1, ArrowLeft: -1, ArrowDown: cols, ArrowUp: -cols };
      if (p.view === "list") {
        step.ArrowRight = 0;
        step.ArrowLeft = 0;
        step.ArrowDown = 1;
        step.ArrowUp = -1;
      }
      if (e.key in step) {
        e.preventDefault();
        if (!step[e.key]) return;
        p.onMove(cur < 0 ? 0 : Math.min(n - 1, Math.max(0, cur + step[e.key])), e.shiftKey && cur >= 0);
      } else if (e.key === "Home") {
        e.preventDefault();
        p.onMove(0, e.shiftKey);
      } else if (e.key === "End") {
        e.preventDefault();
        p.onMove(n - 1, e.shiftKey);
      } else if (e.key === "Enter" && cur >= 0 && !e.shiftKey) {
        e.preventDefault();
        p.onActivate(cur);
      } else if (e.key === " " && cur >= 0) {
        e.preventDefault();
        (p.onQuickLook ?? p.onActivate)(cur);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [p.keyboardActive, p.items, p.focus, cols, p.view]);

  // Stable handlers for the memoized tiles: a selection change re-renders
  // only the tiles whose own state changed, never every visible card.
  const latest = useRef(p);
  latest.current = p;
  const events = useRef<TileEvents>({
    click: (i, ev) => latest.current.onClickItem(i, { toggle: isMac ? ev.metaKey : ev.ctrlKey, range: ev.shiftKey }),
    open: (i) => latest.current.onActivate(i),
    menu: (i, ev) => {
      ev.preventDefault();
      latest.current.onContextMenu(ev, i);
    },
  }).current;

  const overscan = p.view === "list" ? 10 : 2;
  const first = Math.max(0, Math.floor((scrollTop - headerH - top) / rowH) - overscan);
  const last = Math.min(rows - 1, Math.ceil((scrollTop + size.h - headerH - top) / rowH) + overscan);

  const tiles: React.ReactNode[] = [];
  const multi = p.selectedIds.size > 1;
  for (let r = first; r <= last && size.w > 0; r++) {
    for (let c = 0; c < cols; c++) {
      const i = r * cols + c;
      if (i >= p.items.length) break;
      const e = p.items[i];
      tiles.push(
        <Tile
          key={e.id}
          e={e}
          i={i}
          view={p.view}
          top={top + r * rowH}
          left={p.view === "list" ? PAD : PAD + c * (tileW + gap)}
          width={tileW}
          height={p.view === "list" ? rowH : rowH - gap}
          selected={p.selectedIds.has(e.id)}
          focused={multi && i === p.focus}
          showLocation={p.showLocation}
          baseLabel={p.baseLabel}
          events={events}
        />,
      );
    }
  }

  const header = (key: SortKey, label: string, cls: string) => (
    <button className={`${cls} ${p.sort === key ? "active" : ""}`} onClick={() => p.onSort(key)}>
      {label}
      {p.sort === key && <Icon name={p.desc ? "down" : "up"} size={11} />}
    </button>
  );

  return (
    <div
      className={`files view-${p.view} ${p.showLocation ? "with-folder" : ""}`}
      ref={scroller}
      onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
      onClick={(e) => e.target === e.currentTarget && p.onClear()}
    >
      {p.view === "list" && (
        <div className="list-header" style={{ paddingLeft: PAD + 10, paddingRight: PAD + 10 }}>
          {header("name", "Name", "c-name")}
          {header("type", "Type", "c-type")}
          {header("size", "Size", "c-size")}
          {header("modified", "Modified", "c-date")}
          {p.showLocation && <span className="c-folder">Location</span>}
        </div>
      )}
      <div className="sizer" style={{ height }} role="listbox" aria-multiselectable aria-label="Files" onClick={(e) => e.target === e.currentTarget && p.onClear()}>
        {tiles}
      </div>
    </div>
  );
}

interface TileEvents {
  click: (i: number, ev: React.MouseEvent) => void;
  open: (i: number) => void;
  menu: (i: number, ev: React.MouseEvent) => void;
}

interface TileProps {
  e: Entry;
  i: number;
  view: ViewMode;
  top: number;
  left: number;
  width: number;
  height: number;
  selected: boolean;
  /** Keyboard focus inside a multi-selection. */
  focused: boolean;
  showLocation: boolean;
  baseLabel: string;
  events: TileEvents;
}

/** One card or row. Memoized: re-renders only when its own props change. */
const Tile = memo(function Tile({ e, i, view, top, left, width: tileW, height, selected, focused, showLocation, baseLabel, events }: TileProps) {
  const common = {
    style: { top, left, width: tileW, height } as React.CSSProperties,
    "data-selected": selected || undefined,
    "data-focus": focused || undefined,
    "aria-selected": selected,
    role: "option",
    onClick: (ev: React.MouseEvent) => events.click(i, ev),
    onDoubleClick: (ev: React.MouseEvent) => !ev.shiftKey && !ev.metaKey && !ev.ctrlKey && events.open(i),
    onMouseDown: (ev: React.MouseEvent) => (ev.shiftKey || ev.detail > 1) && ev.preventDefault(), // no text selection on Shift/double click
    onContextMenu: (ev: React.MouseEvent) => events.menu(i, ev),
    title: e.name,
  };
  if (view === "list") {
    return (
      <div className={`row ${e.kind === "folder" ? "folder" : ""}`} {...common}>
        <span className="c-name">
          <Icon name={e.kind} size={16} />
          <span className="truncate">{e.name}</span>
          {e.flagged && (
            <span className="risk-mark" title="The name shows a suspicious pattern. Press I for details.">
              <Icon name="warning" size={11} />
            </span>
          )}
          {e.protected && (
            <span className="private-mark" title="Never Modify">
              <Icon name="shield" size={11} />
            </span>
          )}
          {e.favorite && (
            <span className="private-mark" title="Favorite">
              <Icon name="star" size={11} />
            </span>
          )}
          {e.link && <span className="link-target truncate">→ {e.link}</span>}
          {e.private && (
            <span className="private-mark" title={PRIVATE_HINT}>
              <Icon name="lock" size={11} />
            </span>
          )}
        </span>
        <span className="c-type">{typeLabel(e)}</span>
        <span className="c-size">{e.kind === "folder" ? "—" : formatSize(e.size)}</span>
        <span className="c-date">{formatDate(e.modified)}</span>
        {showLocation && <span className="c-folder truncate">{locationOf(e) || baseLabel}</span>}
      </div>
    );
  } else if (view === "gallery") {
    return (
      <div className={`tile gallery ${isMedia(e) ? "" : "plain"}`} {...common}>
        <Thumb entry={e} fit={e.kind === "folder" ? "contain" : "cover"} iconSize={e.kind === "folder" ? Math.round(tileW * 0.22) : 40} />
        <div className="caption-overlay">
          <div className="truncate">
            {e.private && (
              <span className="private-mark" title={PRIVATE_HINT}>
                <Icon name="lock" size={11} />{" "}
              </span>
            )}
            {e.name}
          </div>
          {showLocation && locationOf(e) && <div className="truncate where">{locationOf(e)}</div>}
        </div>
      </div>
    );
  } else {
    return (
      <div className={`tile grid ${e.kind === "folder" ? "folder" : ""}`} {...common}>
        <div className="frame" style={{ height: Math.round(tileW - GRID_CHROME) }}>
          <Thumb entry={e} fit={isMedia(e) ? "cover" : "contain"} iconSize={e.kind === "folder" ? Math.round((tileW - GRID_CHROME) * 0.27) : 36} />
        </div>
        <div className="caption">
          <div className="name truncate">
            {e.flagged && (
              <span className="risk-mark" title="The name shows a suspicious pattern. Press I for details.">
                <Icon name="warning" size={11} />{" "}
              </span>
            )}
            {e.favorite && (
              <span className="private-mark" title="Favorite">
                <Icon name="star" size={10} />{" "}
              </span>
            )}
            {e.name}
          </div>
          <div className="meta truncate">
            {e.kind === "folder" && e.protected && (
              <>
                <span className="private-mark" title="Never Modify">
                  <Icon name="shield" size={10} /> Protected
                </span>
                {" · "}
              </>
            )}
            {e.kind === "folder" && e.private && (
              <>
                <span className="private-mark" title={PRIVATE_HINT}>
                  <Icon name="lock" size={10} /> Private
                </span>
                {" · "}
              </>
            )}
            {e.kind === "folder"
              ? showLocation && locationOf(e)
                ? locationOf(e)
                : `${e.private ? "" : "Folder · "}${formatShortDate(e.modified)}`
              : showLocation && locationOf(e)
                ? locationOf(e)
                : `${formatSize(e.size)} · ${formatShortDate(e.modified)}`}
          </div>
        </div>
      </div>
    );
  }
});
