import { useEffect, useMemo, useRef, useState } from "react";
import { api, plural, type Entry, type TagInfo } from "../api";
import { Icon } from "./Icon";
import { ModalFrame } from "./Modal";

/**
 * Tags for one or more items. Tags live in Mori's app data only: the files
 * and their metadata are never changed.
 */
export function TagDialog({ entries, tags, onClose, onDone }: { entries: Entry[]; tags: TagInfo[]; onClose: () => void; onDone: (msg: string) => void }) {
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const ids = entries.map((e) => e.id);
  const state = (t: TagInfo) => {
    const n = entries.filter((e) => e.tags?.includes(t.id)).length;
    return n === 0 ? "none" : n === entries.length ? "all" : "some";
  };
  const run = async (tagName: string, on: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await api.tagItems(ids, tagName, on);
      onDone(on ? `Tagged ${entries.length === 1 ? `“${entries[0].name}”` : plural(entries.length, "item")} “${tagName}”` : `Removed “${tagName}”`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const filtered = tags.filter((t) => !name.trim() || t.name.toLowerCase().includes(name.trim().toLowerCase()));
  const exact = tags.some((t) => t.name.toLowerCase() === name.trim().toLowerCase());
  return (
    <ModalFrame onCancel={onClose}>
      <div className="dialog-icon">
        <Icon name="tag" size={20} />
      </div>
      <h2>Tags for {entries.length === 1 ? `“${entries[0].name}”` : plural(entries.length, "item")}</h2>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (name.trim()) run(name.trim(), true);
        }}
      >
        <input className="text-input" value={name} onChange={(e) => setName(e.target.value)} placeholder="Find or create a tag" autoFocus maxLength={60} spellCheck={false} />
      </form>
      <div className="tag-pick">
        {filtered.map((t) => {
          const st = state(t);
          return (
            <button key={t.id} className={`tag-option ${st}`} disabled={busy} onClick={() => run(t.name, st !== "all")}>
              <span className="check-box" aria-hidden>
                {st === "all" && <Icon name="check" size={12} stroke={2.2} />}
                {st === "some" && <span className="dash" />}
              </span>
              <span className="truncate">{t.name}</span>
              <span className="muted">{t.count.toLocaleString()}</span>
            </button>
          );
        })}
        {name.trim() && !exact && (
          <button className="tag-option create" disabled={busy} onClick={() => run(name.trim(), true)}>
            <Icon name="tag" size={13} /> Create “{name.trim()}”
          </button>
        )}
        {!tags.length && !name.trim() && <p className="muted small">No tags yet. Type a name to create one.</p>}
      </div>
      {error && <p className="field-error">{error}</p>}
      <p className="dialog-note">Tags are kept by Mori on this computer. The files aren't changed.</p>
      <div className="dialog-actions">
        <button className="btn" onClick={onClose}>
          Done
        </button>
      </div>
    </ModalFrame>
  );
}

/** Rename and delete tags. Deleting a tag never touches the tagged files. */
export function TagManager({ tags, onClose, onChanged }: { tags: TagInfo[]; onClose: () => void; onChanged: (msg: string, deleted?: number) => void }) {
  const [query, setQuery] = useState("");
  const [editing, setEditing] = useState<{ id: number; name: string } | null>(null);
  const [confirm, setConfirm] = useState<TagInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const shown = useMemo(() => tags.filter((t) => t.name.toLowerCase().includes(query.trim().toLowerCase())), [tags, query]);
  const act = async (f: () => Promise<void>, msg: string, deleted?: number) => {
    setError(null);
    try {
      await f();
      onChanged(msg, deleted);
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <ModalFrame onCancel={onClose}>
      <h2>Manage Tags</h2>
      <input className="text-input" value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search tags" spellCheck={false} />
      <div className="tag-pick">
        {shown.map((t) =>
          editing?.id === t.id ? (
            <form
              key={t.id}
              className="tag-option editing"
              onSubmit={(e) => {
                e.preventDefault();
                act(() => api.tagRename(t.id, editing.name), `Renamed to “${editing.name.trim()}”`).then(() => setEditing(null));
              }}
            >
              <input className="text-input slim" value={editing.name} onChange={(e) => setEditing({ id: t.id, name: e.target.value })} autoFocus maxLength={60} />
              <button className="btn small" type="submit">
                Save
              </button>
              <button className="btn small" type="button" onClick={() => setEditing(null)}>
                Cancel
              </button>
            </form>
          ) : confirm?.id === t.id ? (
            <div key={t.id} className="tag-option editing">
              <span className="truncate">
                Remove “{t.name}” from {plural(t.count, "item")}? No files are deleted or changed.
              </span>
              <button className="btn small danger-fill primary" onClick={() => act(() => api.tagDelete(t.id), `Deleted tag “${t.name}”. No files were changed.`, t.id).then(() => setConfirm(null))}>
                Delete
              </button>
              <button className="btn small" onClick={() => setConfirm(null)}>
                Cancel
              </button>
            </div>
          ) : (
            <div key={t.id} className="tag-option">
              <Icon name="tag" size={13} />
              <span className="truncate">{t.name}</span>
              <span className="muted">{t.count.toLocaleString()}</span>
              <button className="icon-btn" onClick={() => setEditing({ id: t.id, name: t.name })} title="Rename">
                <Icon name="rename" size={13} />
              </button>
              <button className="icon-btn" onClick={() => setConfirm(t)} title="Delete tag">
                <Icon name="trash" size={13} />
              </button>
            </div>
          ),
        )}
        {!shown.length && <p className="muted small">{tags.length ? "No tag matches." : "No tags yet. Use “Tags…” on a file or folder to add one."}</p>}
      </div>
      {error && <p className="field-error">{error}</p>}
      <div className="dialog-actions">
        <button className="btn" onClick={onClose}>
          Done
        </button>
      </div>
    </ModalFrame>
  );
}

/** Longest tag name the backend accepts (tags.rs MAX_NAME). */
export const TAG_NAME_MAX = 60;

/**
 * Why `raw` can't become the new name of tag `id` (null: it can). Mirrors the
 * backend's rules (which check again): trimmed, not empty, not too long, and
 * not the name of another tag (case-insensitive; tags are never merged).
 * A case-only change of the same tag is allowed.
 */
export function tagNameProblem(raw: string, id: number, tags: TagInfo[]): string | null {
  const name = raw.trim();
  if (!name) return "A tag needs a name.";
  if ([...name].length > TAG_NAME_MAX) return `That name is too long (at most ${TAG_NAME_MAX} characters).`;
  const other = tags.find((t) => t.id !== id && t.name.toLowerCase() === name.toLowerCase());
  if (other) return `A tag named “${other.name}” already exists. Tags aren't merged: choose another name.`;
  return null;
}

/**
 * The Tags section of the sidebar. Click a tag to open it; right-click (or
 * the context-menu key / Shift+F10) for Rename Tag… and Delete Tag…, which
 * never opens the tag.
 */
export function TagList({
  tags,
  active,
  canEdit,
  onOpen,
  onRename,
  onDelete,
}: {
  tags: TagInfo[];
  /** Id of the tag being viewed, if any. */
  active: number | null;
  /** False in temporary sessions: Mori keeps no records then. */
  canEdit: boolean;
  onOpen: (t: TagInfo) => void;
  onRename: (t: TagInfo) => void;
  onDelete: (t: TagInfo) => void;
}) {
  const [menu, setMenu] = useState<{ x: number; y: number; tag: TagInfo } | null>(null);
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        close();
      }
    };
    window.addEventListener("click", close);
    window.addEventListener("blur", close);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("click", close);
      window.removeEventListener("blur", close);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [menu]);
  const open = (t: TagInfo, x: number, y: number) => setMenu({ x: Math.min(x, window.innerWidth - 200), y: Math.min(y, window.innerHeight - 90), tag: t });
  return (
    <>
      {tags.map((t) => (
        <button
          key={t.id}
          className={`side-item ${active === t.id ? "on" : ""}`}
          onClick={() => onOpen(t)}
          onContextMenu={(e) => {
            // The menu only: right-clicking never opens the tag.
            e.preventDefault();
            e.stopPropagation();
            open(t, e.clientX, e.clientY);
          }}
          onKeyDown={(e) => {
            if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
              e.preventDefault();
              const r = e.currentTarget.getBoundingClientRect();
              open(t, r.left + 24, r.bottom);
            }
          }}
          aria-haspopup="menu"
          title={`${t.name} · right-click to rename or delete`}
        >
          <Icon name="tag" />
          <span className="truncate">{t.name}</span>
          <span className="count">{t.count.toLocaleString()}</span>
        </button>
      ))}
      {menu && (
        <TagMenu
          x={menu.x}
          y={menu.y}
          tag={menu.tag}
          canEdit={canEdit}
          onRename={() => (setMenu(null), onRename(menu.tag))}
          onDelete={() => (setMenu(null), onDelete(menu.tag))}
        />
      )}
    </>
  );
}

function TagMenu({ x, y, tag, canEdit, onRename, onDelete }: { x: number; y: number; tag: TagInfo; canEdit: boolean; onRename: () => void; onDelete: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  // Keyboard: focus the first item; ↑ ↓ move between items.
  useEffect(() => ref.current?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus(), []);
  const move = (e: React.KeyboardEvent) => {
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const items = [...(ref.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? [])];
    const i = items.indexOf(document.activeElement as HTMLButtonElement);
    items[(i + (e.key === "ArrowDown" ? 1 : items.length - 1)) % items.length]?.focus();
  };
  const ro = canEdit ? undefined : "Not available in a temporary session: Mori keeps no records about this folder.";
  return (
    <div ref={ref} className="menu" role="menu" aria-label={`Tag “${tag.name}”`} style={{ left: x, top: y }} onClick={(e) => e.stopPropagation()} onKeyDown={move} onContextMenu={(e) => e.preventDefault()}>
      <button role="menuitem" onClick={onRename} disabled={!canEdit} title={ro}>
        <Icon name="rename" size={14} /> Rename Tag…
      </button>
      <button role="menuitem" className="danger" onClick={onDelete} disabled={!canEdit} title={ro ?? "Removes the tag only. No files are deleted or changed."}>
        <Icon name="trash" size={14} /> Delete Tag…
      </button>
    </div>
  );
}

/** Rename a tag in place: same tag (same id), so every tagged item keeps it. */
export function TagRenameDialog({ tag, tags, onCancel, onDone }: { tag: TagInfo; tags: TagInfo[]; onCancel: () => void; onDone: (msg: string) => void }) {
  const [name, setName] = useState(tag.name);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => input.current?.select(), []);
  const problem = tagNameProblem(name, tag.id, tags);
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const next = name.trim();
    if (next === tag.name) return onCancel();
    if (problem) return setError(problem);
    setBusy(true);
    try {
      await api.tagRename(tag.id, next);
      onDone(`Renamed tag “${tag.name}” to “${next}”`);
    } catch (err) {
      setError(String(err));
      setBusy(false);
    }
  };
  return (
    <ModalFrame onCancel={onCancel}>
      <form onSubmit={submit}>
        <h2>Rename Tag</h2>
        <input
          ref={input}
          className="text-input"
          value={name}
          onChange={(e) => (setName(e.target.value), setError(null))}
          maxLength={TAG_NAME_MAX + 20}
          spellCheck={false}
          autoCorrect="off"
          aria-label="New tag name"
          aria-invalid={!!error}
        />
        {error && <p className="field-error">{error}</p>}
        <p className="dialog-note">
          {tag.count > 0 ? `The ${plural(tag.count, "item")} with this tag keep it under the new name.` : "No items have this tag yet."}
        </p>
        <div className="dialog-actions">
          <button type="button" className="btn" onClick={onCancel}>
            Cancel
          </button>
          <button type="submit" className="btn primary" disabled={busy || !name.trim()}>
            Rename
          </button>
        </div>
      </form>
    </ModalFrame>
  );
}

/** Deleting a tag removes the tag and its associations, never any file. */
export function TagDeleteDialog({ tag, onCancel, onDone }: { tag: TagInfo; onCancel: () => void; onDone: (msg: string) => void }) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true);
    try {
      await api.tagDelete(tag.id);
      onDone(`Deleted tag “${tag.name}”. No files were changed.`);
    } catch (err) {
      setError(String(err));
      setBusy(false);
    }
  };
  return (
    <ModalFrame onCancel={() => !busy && onCancel()}>
      <div className="dialog-icon">
        <Icon name="tag" size={20} />
      </div>
      <h2>Delete tag “{tag.name}”?</h2>
      <p>{tag.count > 0 ? `This removes the tag from ${plural(tag.count, "item")}.` : "No items have this tag."}</p>
      <p className="dialog-note">
        <strong>No files are deleted or changed.</strong> Only Mori's tag is removed.
      </p>
      {error && <p className="field-error">{error}</p>}
      <div className="dialog-actions">
        <button className="btn" onClick={onCancel} disabled={busy} autoFocus>
          Cancel
        </button>
        <button className="btn primary danger-fill" onClick={run} disabled={busy}>
          Delete Tag
        </button>
      </div>
    </ModalFrame>
  );
}
