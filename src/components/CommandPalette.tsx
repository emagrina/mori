import { useEffect, useMemo, useRef, useState } from "react";
import { api, isMac, type Entry } from "../api";
import { Icon, type IconName } from "./Icon";

export interface Command {
  id: string;
  title: string;
  section: string;
  icon?: IconName;
  /** Shortcut shown next to the command. */
  keys?: string;
  /** Extra words that should match. */
  words?: string;
  run: () => void;
}

/**
 * ⌘K / Ctrl+K: every command and view by name, plus files and folders of
 * the browsed drive matched by the index (nothing is read from disk).
 */
export function CommandPalette({ commands, onClose, onOpenEntry }: { commands: Command[]; onClose: () => void; onOpenEntry: (e: Entry) => void }) {
  const [q, setQ] = useState("");
  const [sel, setSel] = useState(0);
  const [found, setFound] = useState<Entry[]>([]);
  const list = useRef<HTMLDivElement>(null);

  const matches = useMemo(() => {
    const words = q.toLowerCase().split(/\s+/).filter(Boolean);
    return commands.filter((c) => words.every((w) => `${c.title} ${c.section} ${c.words ?? ""}`.toLowerCase().includes(w)));
  }, [q, commands]);

  useEffect(() => {
    const term = q.trim();
    if (term.length < 2) {
      setFound([]);
      return;
    }
    let alive = true;
    const t = window.setTimeout(() => {
      api
        .query({ scope: "folder", folder: "", kind: "all", search: term, sort: "name", desc: false, recursive: false, global: true })
        .then((r) => alive && setFound(r.items.slice(0, 8)), () => {});
    }, 120);
    return () => {
      alive = false;
      window.clearTimeout(t);
    };
  }, [q]);

  type Row = { key: string; title: string; sub: string; icon: IconName; keys?: string; run: () => void };
  const rows: Row[] = [
    ...matches.map((c) => ({ key: c.id, title: c.title, sub: c.section, icon: c.icon ?? "command", keys: c.keys, run: c.run })),
    ...found.map((e) => ({
      key: `f:${e.id}`,
      title: e.name,
      sub: e.path.includes("/") ? e.path.slice(0, e.path.lastIndexOf("/")) : "Drive",
      icon: (e.kind === "folder" ? "folder" : e.kind) as IconName,
      run: () => onOpenEntry(e),
    })),
  ];

  useEffect(() => setSel(0), [q]);
  useEffect(() => {
    list.current?.querySelector(".on")?.scrollIntoView({ block: "nearest" });
  }, [sel]);

  const choose = (r: Row | undefined) => {
    if (!r) return;
    onClose();
    r.run();
  };

  return (
    <div className="palette-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="palette" role="dialog" aria-label="Command palette">
        <div className="palette-input">
          <Icon name="search" size={15} />
          <input
            autoFocus
            value={q}
            placeholder="Type a command, view, file or folder…"
            spellCheck={false}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") {
                e.preventDefault();
                setSel((s) => Math.min(rows.length - 1, s + 1));
              } else if (e.key === "ArrowUp") {
                e.preventDefault();
                setSel((s) => Math.max(0, s - 1));
              } else if (e.key === "Enter") {
                e.preventDefault();
                choose(rows[sel]);
              } else if (e.key === "Escape") {
                e.preventDefault();
                e.stopPropagation();
                onClose();
              }
            }}
          />
          <kbd>esc</kbd>
        </div>
        <div className="palette-list" ref={list} role="listbox">
          {rows.map((r, i) => (
            <button key={r.key} className={i === sel ? "on" : ""} role="option" aria-selected={i === sel} onMouseEnter={() => setSel(i)} onClick={() => choose(r)}>
              <Icon name={r.icon} size={14} />
              <span className="truncate palette-title">{r.title}</span>
              <span className="truncate muted palette-sub">{r.sub}</span>
              {r.keys && <kbd>{r.keys}</kbd>}
            </button>
          ))}
          {!rows.length && <p className="muted small palette-empty">Nothing matches “{q}”.</p>}
        </div>
      </div>
    </div>
  );
}

const mod = isMac ? "⌘" : "Ctrl+";

export const SHORTCUTS: [string, string][] = [
  ["↑ ↓ ← →", "Move the selection"],
  ["Return", "Open (folder or preview)"],
  ["Space", "Quick Look (Mori's own, never the system one)"],
  ["Esc", "Close / clear"],
  ["I", "Get Info"],
  ["F", "Add to / remove from Favorites"],
  [`${mod}F`, "Search"],
  [`${mod}K`, "Command palette"],
  [`${mod}1  ${mod}2  ${mod}3`, "List · Grid · Gallery"],
  ["← →  (preview)", "Previous / next file"],
  [`${mod}+  ${mod}−  ${mod}0`, "Zoom in / out / fit (preview)"],
  [isMac ? "⌘⌫" : "Delete", "Move to Trash"],
  [`${mod}Z`, "Undo the last file operation"],
  [`${mod}↑  ⌫`, "Parent folder / back"],
];

export function ShortcutsHelp({ onClose }: { onClose: () => void }) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [onClose]);
  return (
    <div className="palette-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="palette shortcuts" role="dialog" aria-label="Keyboard shortcuts">
        <h2>Keyboard Shortcuts</h2>
        <dl>
          {SHORTCUTS.map(([k, v]) => (
            <div key={k}>
              <dt>
                <kbd>{k}</kbd>
              </dt>
              <dd>{v}</dd>
            </div>
          ))}
        </dl>
        <p className="muted small">Shortcuts never fire while you type in a text field.</p>
        <div className="dialog-actions">
          <button className="btn" onClick={onClose} autoFocus>
            Done
          </button>
        </div>
      </div>
    </div>
  );
}
