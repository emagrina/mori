import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { formatDate, formatShortDate, formatSize, parentOf, typeLabel, type Entry, type SortKey, type ViewMode } from "../api";

/** Folder an item came from, relative to the view ("" = the viewed folder itself). */
const locationOf = (e: Entry) => (e.location ?? parentOf(e.path)).split("/").join(" / ");
import { Icon } from "./Icon";
import { Thumb } from "./Thumb";

interface Props {
  items: Entry[];
  view: ViewMode;
  selected: number;
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
  onSelect: (i: number) => void;
  onActivate: (i: number) => void;
  onContextMenu: (e: React.MouseEvent, i: number) => void;
}

const PAD = 20;
const LIST_ROW = 30;
const LIST_HEADER = 30;

function layout(view: ViewMode, width: number) {
  const inner = Math.max(0, width - PAD * 2);
  if (view === "list") return { cols: 1, tileW: inner, rowH: LIST_ROW, gap: 0 };
  const min = view === "gallery" ? 240 : 128;
  const gap = view === "gallery" ? 8 : 14;
  const cols = Math.max(1, Math.floor((inner + gap) / (min + gap)));
  const tileW = (inner - gap * (cols - 1)) / cols;
  const tileH = view === "gallery" ? Math.round(tileW * 0.75) : Math.round(tileW) + 42;
  return { cols, tileW, rowH: tileH + gap, gap };
}

export function FileView(p: Props) {
  const scroller = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [scrollTop, setScrollTop] = useState(0);
  const { cols, tileW, rowH, gap } = layout(p.view, size.w);
  const rows = Math.ceil(p.items.length / cols);
  // In list view the sticky header occupies LIST_HEADER px of scroll space above the rows.
  const headerH = p.view === "list" ? LIST_HEADER : 0;
  const top = p.view === "list" ? 4 : PAD;
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

  // Keep the selected item visible (keyboard navigation, preview next/prev).
  useEffect(() => {
    const el = scroller.current;
    if (!el || p.selected < 0) return;
    const rowTop = headerH + top + Math.floor(p.selected / cols) * rowH;
    if (rowTop - headerH < el.scrollTop) el.scrollTop = rowTop - headerH - (p.view === "list" ? 0 : gap);
    else if (rowTop + rowH > el.scrollTop + el.clientHeight) el.scrollTop = rowTop + rowH - el.clientHeight + PAD / 2;
  }, [p.selected, cols, rowH]);

  // Arrow-key navigation within the grid.
  useEffect(() => {
    if (!p.keyboardActive) return;
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement;
      if (t.tagName === "INPUT" || t.tagName === "SELECT" || t.isContentEditable || e.metaKey || e.ctrlKey || e.altKey) return;
      const n = p.items.length;
      if (!n) return;
      const cur = p.selected;
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
        const next = cur < 0 ? 0 : Math.min(n - 1, Math.max(0, cur + step[e.key]));
        p.onSelect(next);
      } else if (e.key === "Home") {
        e.preventDefault();
        p.onSelect(0);
      } else if (e.key === "End") {
        e.preventDefault();
        p.onSelect(n - 1);
      } else if ((e.key === " " || e.key === "Enter") && cur >= 0) {
        e.preventDefault();
        p.onActivate(cur);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [p.keyboardActive, p.items, p.selected, cols, p.view]);

  const overscan = p.view === "list" ? 10 : 2;
  const first = Math.max(0, Math.floor((scrollTop - headerH - top) / rowH) - overscan);
  const last = Math.min(rows - 1, Math.ceil((scrollTop + size.h - headerH - top) / rowH) + overscan);

  const tiles: React.ReactNode[] = [];
  for (let r = first; r <= last && size.w > 0; r++) {
    for (let c = 0; c < cols; c++) {
      const i = r * cols + c;
      if (i >= p.items.length) break;
      const e = p.items[i];
      const style: React.CSSProperties =
        p.view === "list"
          ? { top: top + r * rowH, left: PAD, width: tileW, height: rowH }
          : { top: top + r * rowH, left: PAD + c * (tileW + gap), width: tileW, height: rowH - gap };
      const common = {
        style,
        "data-selected": i === p.selected || undefined,
        onClick: () => {
          p.onSelect(i);
          p.onActivate(i);
        },
        onContextMenu: (ev: React.MouseEvent) => {
          ev.preventDefault();
          p.onSelect(i);
          p.onContextMenu(ev, i);
        },
        title: e.name,
      };
      if (p.view === "list") {
        tiles.push(
          <div key={e.id} className={`row ${i % 2 ? "odd" : ""}`} {...common}>
            <span className="c-name">
              <Icon name={e.kind} size={15} className={`kind-${e.kind}`} />
              <span className="truncate">{e.name}</span>
            </span>
            <span className="c-type">{typeLabel(e)}</span>
            <span className="c-size">{e.kind === "folder" ? "—" : formatSize(e.size)}</span>
            <span className="c-date">{formatDate(e.modified)}</span>
            {p.showLocation && <span className="c-folder truncate">{locationOf(e) || p.baseLabel}</span>}
          </div>,
        );
      } else if (p.view === "gallery") {
        tiles.push(
          <div key={e.id} className="tile gallery" {...common}>
            <Thumb entry={e} fit={e.kind === "folder" ? "contain" : "cover"} iconSize={56} />
            <div className="caption-overlay">
              <div className="truncate">{e.name}</div>
              {p.showLocation && locationOf(e) && <div className="truncate where">{locationOf(e)}</div>}
            </div>
          </div>,
        );
      } else {
        tiles.push(
          <div key={e.id} className="tile grid" {...common}>
            <div className="frame" style={{ height: tileW }}>
              <Thumb entry={e} iconSize={44} />
            </div>
            <div className="caption">
              <div className="name truncate">{e.name}</div>
              <div className="meta truncate">
                {e.kind === "folder"
                  ? "Folder"
                  : p.showLocation && locationOf(e)
                    ? locationOf(e)
                    : `${formatSize(e.size)} · ${formatShortDate(e.modified)}`}
              </div>
            </div>
          </div>,
        );
      }
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
      onClick={(e) => e.target === e.currentTarget && p.onSelect(-1)}
    >
      {p.view === "list" && (
        <div className="list-header" style={{ paddingLeft: PAD + 8, paddingRight: PAD + 8 }}>
          {header("name", "Name", "c-name")}
          {header("type", "Type", "c-type")}
          {header("size", "Size", "c-size")}
          {header("modified", "Modified", "c-date")}
          {p.showLocation && <span className="c-folder">Location</span>}
        </div>
      )}
      <div className="sizer" style={{ height }} onClick={(e) => e.target === e.currentTarget && p.onSelect(-1)}>
        {tiles}
      </div>
    </div>
  );
}
