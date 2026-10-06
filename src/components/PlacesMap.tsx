import { useEffect, useMemo, useRef, useState } from "react";
import { LAND_PATH } from "../assets/world";
import { plural, type Places } from "../api";
import { Icon } from "./Icon";

/** Screen pixels per cluster cell. */
const CELL_PX = 44;
const MAX_SCALE = 400;

interface Cluster {
  x: number;
  y: number;
  members: number[];
}

/**
 * Offline map: bundled land outlines (Natural Earth, equirectangular) and
 * the scanned positions, clustered on a screen-space grid. No tiles, no map
 * service, no network — coordinates never leave this view.
 */
export function PlacesMap({ places, onOpen }: { places: Places; onOpen: (index: number, members: number[]) => void }) {
  const box = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 800, h: 500 });
  // View: map units (0..360 × 0..180) → screen = (u - ox) * scale.
  const [view, setView] = useState<{ ox: number; oy: number; scale: number } | null>(null);
  const [picked, setPicked] = useState<Cluster | null>(null);
  const drag = useRef<{ x: number; y: number; ox: number; oy: number; moved: boolean } | null>(null);

  const n = places.ids.length;
  const pts = useMemo(() => {
    const xs = new Float64Array(n);
    const ys = new Float64Array(n);
    for (let i = 0; i < n; i++) {
      xs[i] = places.coords[2 * i + 1] + 180;
      ys[i] = 90 - places.coords[2 * i];
    }
    return { xs, ys };
  }, [places]);

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const fit = () => {
    let [x0, y0, x1, y1] = [360, 180, 0, 0];
    for (let i = 0; i < n; i++) {
      x0 = Math.min(x0, pts.xs[i]);
      x1 = Math.max(x1, pts.xs[i]);
      y0 = Math.min(y0, pts.ys[i]);
      y1 = Math.max(y1, pts.ys[i]);
    }
    if (!n) [x0, y0, x1, y1] = [0, 0, 360, 180];
    const pad = Math.max(2, (x1 - x0) * 0.15, (y1 - y0) * 0.15);
    [x0, y0, x1, y1] = [x0 - pad, y0 - pad, x1 + pad, y1 + pad];
    const scale = Math.min(MAX_SCALE, size.w / (x1 - x0), size.h / (y1 - y0));
    setView({ scale, ox: (x0 + x1) / 2 - size.w / 2 / scale, oy: (y0 + y1) / 2 - size.h / 2 / scale });
  };
  useEffect(() => {
    if (size.w > 0) fit();
  }, [places, size.w > 0]);

  const v = view ?? { ox: 0, oy: 0, scale: size.w / 360 };
  const minScale = Math.min(size.w / 360, size.h / 180) * 0.9;

  const clusters = useMemo(() => {
    const cell = CELL_PX / v.scale;
    const map = new Map<string, Cluster & { sx: number; sy: number }>();
    for (let i = 0; i < n; i++) {
      const key = `${Math.floor(pts.xs[i] / cell)}:${Math.floor(pts.ys[i] / cell)}`;
      let c = map.get(key);
      if (!c) {
        c = { x: 0, y: 0, sx: 0, sy: 0, members: [] };
        map.set(key, c);
      }
      c.sx += pts.xs[i];
      c.sy += pts.ys[i];
      c.members.push(i);
    }
    return [...map.values()].map((c) => ({ x: c.sx / c.members.length, y: c.sy / c.members.length, members: c.members }));
  }, [pts, n, Math.round(Math.log2(v.scale) * 4)]);

  const zoomAt = (factor: number, sx: number, sy: number) =>
    setView((cur) => {
      const c = cur ?? v;
      const scale = Math.min(MAX_SCALE, Math.max(minScale, c.scale * factor));
      const ux = c.ox + sx / c.scale;
      const uy = c.oy + sy / c.scale;
      return { scale, ox: ux - sx / scale, oy: uy - sy / scale };
    });

  const toScreen = (x: number, y: number) => [(x - v.ox) * v.scale, (y - v.oy) * v.scale];
  const visible = clusters.filter((c) => {
    const [sx, sy] = toScreen(c.x, c.y);
    return sx > -40 && sy > -40 && sx < size.w + 40 && sy < size.h + 40;
  });

  return (
    <div className="places">
      <div
        className="places-map"
        ref={box}
        onWheel={(e) => {
          const r = box.current!.getBoundingClientRect();
          zoomAt(Math.exp(-e.deltaY * 0.0025), e.clientX - r.left, e.clientY - r.top);
        }}
        onPointerDown={(e) => {
          (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
          drag.current = { x: e.clientX, y: e.clientY, ox: v.ox, oy: v.oy, moved: false };
        }}
        onPointerMove={(e) => {
          const d = drag.current;
          if (!d) return;
          if (Math.abs(e.clientX - d.x) + Math.abs(e.clientY - d.y) > 3) d.moved = true;
          setView({ scale: v.scale, ox: d.ox - (e.clientX - d.x) / v.scale, oy: d.oy - (e.clientY - d.y) / v.scale });
        }}
        onPointerUp={() => (drag.current = null)}
        onDoubleClick={(e) => {
          const r = box.current!.getBoundingClientRect();
          zoomAt(2, e.clientX - r.left, e.clientY - r.top);
        }}
      >
        <svg width={size.w} height={size.h} aria-label="Map of photo and video positions">
          <g transform={`scale(${v.scale}) translate(${-v.ox} ${-v.oy})`}>
            {[-360, 0, 360].map((dx) => (
              <path key={dx} d={LAND_PATH} transform={`translate(${dx} 0)`} className="land" vectorEffect="non-scaling-stroke" />
            ))}
            {Array.from({ length: 13 }, (_, i) => (
              <line key={`m${i}`} x1={i * 30} x2={i * 30} y1={0} y2={180} className="grid" vectorEffect="non-scaling-stroke" />
            ))}
            {Array.from({ length: 7 }, (_, i) => (
              <line key={`p${i}`} x1={-360} x2={720} y1={i * 30} y2={i * 30} className="grid" vectorEffect="non-scaling-stroke" />
            ))}
          </g>
          {visible.map((c) => {
            const [sx, sy] = toScreen(c.x, c.y);
            const r = c.members.length === 1 ? 6 : Math.min(26, 10 + Math.log2(c.members.length) * 3);
            const on = picked && picked.members[0] === c.members[0];
            return (
              <g
                key={c.members[0]}
                className={`cluster ${on ? "on" : ""}`}
                transform={`translate(${sx} ${sy})`}
                onPointerDown={(e) => e.stopPropagation()}
                onClick={() => setPicked(c)}
                onDoubleClick={(e) => {
                  e.stopPropagation();
                  zoomAt(3, sx, sy);
                }}
              >
                <circle r={r} />
                {c.members.length > 1 && (
                  <text dy="0.35em" textAnchor="middle">
                    {c.members.length > 999 ? `${Math.round(c.members.length / 1000)}k` : c.members.length}
                  </text>
                )}
              </g>
            );
          })}
        </svg>
        <div className="places-tools">
          <button className="icon-btn" onClick={() => zoomAt(1.6, size.w / 2, size.h / 2)} title="Zoom in">
            <Icon name="zoomIn" size={15} />
          </button>
          <button className="icon-btn" onClick={() => zoomAt(1 / 1.6, size.w / 2, size.h / 2)} title="Zoom out">
            <Icon name="zoomOut" size={15} />
          </button>
          <button className="icon-btn" onClick={fit} title="Fit all places">
            <Icon name="all" size={15} />
          </button>
        </div>
        <div className="places-note">Offline map · Natural Earth outlines · no map service is contacted</div>
      </div>
      {picked && (
        <aside className="places-list">
          <header>
            <strong>{plural(picked.members.length, "file")}</strong>
            <button className="icon-btn" onClick={() => setPicked(null)} aria-label="Close">
              <Icon name="close" size={12} />
            </button>
          </header>
          <div className="muted small mono">
            {(90 - picked.y).toFixed(4)}, {(picked.x - 180).toFixed(4)}
          </div>
          <ul>
            {picked.members.slice(0, 300).map((i) => (
              <li key={i}>
                <button onClick={() => onOpen(i, picked.members)} title={places.names[i]}>
                  <span className="truncate">{places.names[i]}</span>
                </button>
              </li>
            ))}
          </ul>
          {picked.members.length > 300 && <div className="muted small">Zoom in to see the other {picked.members.length - 300}.</div>}
        </aside>
      )}
    </div>
  );
}
