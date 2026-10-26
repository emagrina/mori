import { useEffect, useMemo, useRef, useState } from "react";
import { api, type Entry, type TagInfo } from "../api";
import { fmtNum, t, tn } from "../i18n";
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
  const state = (g: TagInfo) => {
    const n = entries.filter((e) => e.tags?.includes(g.id)).length;
    return n === 0 ? "none" : n === entries.length ? "all" : "some";
  };
  const run = async (tagName: string, on: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await api.tagItems(ids, tagName, on);
      onDone(
        on
          ? entries.length === 1
            ? t("Tagged “{name}” “{tag}”", { name: entries[0].name, tag: tagName })
            : tn(entries.length, "Tagged {n} item “{tag}”", "Tagged {n} items “{tag}”", { tag: tagName })
          : t("Removed “{tag}”", { tag: tagName }),
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const filtered = tags.filter((g) => !name.trim() || g.name.toLowerCase().includes(name.trim().toLowerCase()));
  const exact = tags.some((g) => g.name.toLowerCase() === name.trim().toLowerCase());
  return (
    <ModalFrame onCancel={onClose}>
      <div className="dialog-icon">
        <Icon name="tag" size={20} />
      </div>
      <h2>{entries.length === 1 ? t("Tags for “{name}”", { name: entries[0].name }) : tn(entries.length, "Tags for {n} item", "Tags for {n} items")}</h2>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          if (name.trim()) run(name.trim(), true);
        }}
      >
        <input className="text-input" value={name} onChange={(e) => setName(e.target.value)} placeholder={t("Find or create a tag")} autoFocus maxLength={60} spellCheck={false} />
      </form>
      <div className="tag-pick">
        {filtered.map((g) => {
          const st = state(g);
          return (
            <button key={g.id} className={`tag-option ${st}`} disabled={busy} onClick={() => run(g.name, st !== "all")}>
              <span className="check-box" aria-hidden>
                {st === "all" && <Icon name="check" size={12} stroke={2.2} />}
                {st === "some" && <span className="dash" />}
              </span>
              <span className="truncate">{g.name}</span>
              <span className="muted">{fmtNum(g.count)}</span>
            </button>
          );
        })}
        {name.trim() && !exact && (
          <button className="tag-option create" disabled={busy} onClick={() => run(name.trim(), true)}>
            <Icon name="tag" size={13} /> {t("Create “{name}”", { name: name.trim() })}
          </button>
        )}
        {!tags.length && !name.trim() && <p className="muted small">{t("No tags yet. Type a name to create one.")}</p>}
      </div>
      {error && <p className="field-error">{error}</p>}
      <p className="dialog-note">{t("Tags are kept by Mori on this computer. The files aren't changed.")}</p>
      <div className="dialog-actions">
        <button className="btn" onClick={onClose}>
          {t("Done")}
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
  const shown = useMemo(() => tags.filter((g) => g.name.toLowerCase().includes(query.trim().toLowerCase())), [tags, query]);
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
      <h2>{t("Manage Tags")}</h2>
      <input className="text-input" value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t("Search tags")} spellCheck={false} />
      <div className="tag-pick">
        {shown.map((g) =>
          editing?.id === g.id ? (
            <form
              key={g.id}
              className="tag-option editing"
              onSubmit={(e) => {
                e.preventDefault();
                act(() => api.tagRename(g.id, editing.name), t("Renamed to “{name}”", { name: editing.name.trim() })).then(() => setEditing(null));
              }}
            >
              <input className="text-input slim" value={editing.name} onChange={(e) => setEditing({ id: g.id, name: e.target.value })} autoFocus maxLength={60} />
              <button className="btn small" type="submit">
                {t("Save")}
              </button>
              <button className="btn small" type="button" onClick={() => setEditing(null)}>
                {t("Cancel")}
              </button>
            </form>
          ) : confirm?.id === g.id ? (
            <div key={g.id} className="tag-option editing">
              <span className="truncate">
                {tn(g.count, "Remove “{tag}” from {n} item? No files are deleted or changed.", "Remove “{tag}” from {n} items? No files are deleted or changed.", { tag: g.name })}
              </span>
              <button className="btn small danger-fill primary" onClick={() => act(() => api.tagDelete(g.id), t("Deleted tag “{tag}”. No files were changed.", { tag: g.name }), g.id).then(() => setConfirm(null))}>
                {t("Delete")}
              </button>
              <button className="btn small" onClick={() => setConfirm(null)}>
                {t("Cancel")}
              </button>
            </div>
          ) : (
            <div key={g.id} className="tag-option">
              <Icon name="tag" size={13} />
              <span className="truncate">{g.name}</span>
              <span className="muted">{fmtNum(g.count)}</span>
              <button className="icon-btn" onClick={() => setEditing({ id: g.id, name: g.name })} title={t("Rename")}>
                <Icon name="rename" size={13} />
              </button>
              <button className="icon-btn" onClick={() => setConfirm(g)} title={t("Delete tag")}>
                <Icon name="trash" size={13} />
              </button>
            </div>
          ),
        )}
        {!shown.length && <p className="muted small">{tags.length ? t("No tag matches.") : t("No tags yet. Use “Tags…” on a file or folder to add one.")}</p>}
      </div>
      {error && <p className="field-error">{error}</p>}
      <div className="dialog-actions">
        <button className="btn" onClick={onClose}>
          {t("Done")}
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
  if (!name) return t("A tag needs a name.");
  if ([...name].length > TAG_NAME_MAX) return t("That name is too long (at most {max} characters).", { max: TAG_NAME_MAX });
  const other = tags.find((g) => g.id !== id && g.name.toLowerCase() === name.toLowerCase());
  if (other) return t("A tag named “{name}” already exists. Tags aren't merged: choose another name.", { name: other.name });
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
  onOpen: (g: TagInfo) => void;
  onRename: (g: TagInfo) => void;
  onDelete: (g: TagInfo) => void;
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
  const open = (g: TagInfo, x: number, y: number) => setMenu({ x: Math.min(x, window.innerWidth - 200), y: Math.min(y, window.innerHeight - 90), tag: g });
  return (
    <>
      {tags.map((g) => (
        <button
          key={g.id}
          className={`side-item ${active === g.id ? "on" : ""}`}
          onClick={() => onOpen(g)}
          onContextMenu={(e) => {
            // The menu only: right-clicking never opens the tag.
            e.preventDefault();
            e.stopPropagation();
            open(g, e.clientX, e.clientY);
          }}
          onKeyDown={(e) => {
            if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
              e.preventDefault();
              const r = e.currentTarget.getBoundingClientRect();
              open(g, r.left + 24, r.bottom);
            }
          }}
          aria-haspopup="menu"
          title={t("{name} · right-click to rename or delete", { name: g.name })}
        >
          <Icon name="tag" />
          <span className="truncate">{g.name}</span>
          <span className="count">{fmtNum(g.count)}</span>
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
  const ro = canEdit ? undefined : t("Not available in a temporary session: Mori keeps no records about this folder.");
  return (
    <div ref={ref} className="menu" role="menu" aria-label={t("Tag “{name}”", { name: tag.name })} style={{ left: x, top: y }} onClick={(e) => e.stopPropagation()} onKeyDown={move} onContextMenu={(e) => e.preventDefault()}>
      <button role="menuitem" onClick={onRename} disabled={!canEdit} title={ro}>
        <Icon name="rename" size={14} /> {t("Rename Tag…")}
      </button>
      <button role="menuitem" className="danger" onClick={onDelete} disabled={!canEdit} title={ro ?? t("Removes the tag only. No files are deleted or changed.")}>
        <Icon name="trash" size={14} /> {t("Delete Tag…")}
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
      onDone(t("Renamed tag “{old}” to “{name}”", { old: tag.name, name: next }));
    } catch (err) {
      setError(String(err));
      setBusy(false);
    }
  };
  return (
    <ModalFrame onCancel={onCancel}>
      <form onSubmit={submit}>
        <h2>{t("Rename Tag")}</h2>
        <input
          ref={input}
          className="text-input"
          value={name}
          onChange={(e) => (setName(e.target.value), setError(null))}
          maxLength={TAG_NAME_MAX + 20}
          spellCheck={false}
          autoCorrect="off"
          aria-label={t("New tag name")}
          aria-invalid={!!error}
        />
        {error && <p className="field-error">{error}</p>}
        <p className="dialog-note">
          {tag.count > 0
            ? tn(tag.count, "The {n} item with this tag keeps it under the new name.", "The {n} items with this tag keep it under the new name.")
            : t("No items have this tag yet.")}
        </p>
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

/** Deleting a tag removes the tag and its associations, never any file. */
export function TagDeleteDialog({ tag, onCancel, onDone }: { tag: TagInfo; onCancel: () => void; onDone: (msg: string) => void }) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const run = async () => {
    setBusy(true);
    try {
      await api.tagDelete(tag.id);
      onDone(t("Deleted tag “{tag}”. No files were changed.", { tag: tag.name }));
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
      <h2>{t("Delete tag “{tag}”?", { tag: tag.name })}</h2>
      <p>{tag.count > 0 ? tn(tag.count, "This removes the tag from {n} item.", "This removes the tag from {n} items.") : t("No items have this tag.")}</p>
      <p className="dialog-note">
        <strong>{t("No files are deleted or changed.")}</strong> {t("Only Mori's tag is removed.")}
      </p>
      {error && <p className="field-error">{error}</p>}
      <div className="dialog-actions">
        <button className="btn" onClick={onCancel} disabled={busy} autoFocus>
          {t("Cancel")}
        </button>
        <button className="btn primary danger-fill" onClick={run} disabled={busy}>
          {t("Delete Tag")}
        </button>
      </div>
    </ModalFrame>
  );
}
