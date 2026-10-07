/**
 * Desktop file-manager selection, independent of any view: grid, list,
 * gallery, search results, flattened folders and Quick Cleanup's review all
 * use it. Items are identified by their opaque id, never by name (two files
 * in different folders can share one).
 *
 * Every function returns a new state (or the same object when nothing
 * changed, so React can skip work).
 */

export interface Selection {
  readonly ids: ReadonlySet<string>;
  /** Fixed end of Shift ranges. */
  readonly anchor: string | null;
  /** The item with keyboard focus (the "current" item). */
  readonly focus: string | null;
}

export const EMPTY_SELECTION: Selection = { ids: new Set(), anchor: null, focus: null };

export interface ClickMods {
  /** Cmd (macOS) / Ctrl (Windows, Linux): add or remove one item. */
  toggle: boolean;
  /** Shift: everything from the anchor to here. */
  range: boolean;
}

export function selectOnly(id: string | null): Selection {
  return id ? { ids: new Set([id]), anchor: id, focus: id } : EMPTY_SELECTION;
}

/** Ids from `a` to `b` (inclusive) in `order`; just `b` if `a` isn't listed. */
function span(order: readonly string[], a: string | null, b: string): string[] {
  const j = order.indexOf(b);
  const i = a === null ? -1 : order.indexOf(a);
  if (j < 0) return [];
  if (i < 0) return [b];
  return order.slice(Math.min(i, j), Math.max(i, j) + 1);
}

/** A click on `id` with modifiers (a plain click selects only that item). */
export function click(sel: Selection, order: readonly string[], id: string, mods: ClickMods): Selection {
  if (mods.range) {
    const anchor = sel.anchor ?? sel.focus ?? id;
    const range = span(order, anchor, id);
    // Cmd/Ctrl+Shift adds the range to what's selected; Shift alone replaces it.
    const ids = mods.toggle ? new Set([...sel.ids, ...range]) : new Set(range);
    return { ids, anchor, focus: id };
  }
  if (mods.toggle) {
    const ids = new Set(sel.ids);
    if (ids.has(id)) ids.delete(id);
    else ids.add(id);
    return { ids, anchor: id, focus: id };
  }
  return selectOnly(id);
}

/**
 * Keyboard: focus the item at `index` (clamped). With `extend` (Shift +
 * arrows) the selection becomes the range from the anchor.
 */
export function moveTo(sel: Selection, order: readonly string[], index: number, extend: boolean): Selection {
  if (!order.length) return sel;
  const id = order[Math.min(order.length - 1, Math.max(0, index))];
  if (!extend) return sel.focus === id && sel.ids.size === 1 && sel.ids.has(id) ? sel : selectOnly(id);
  const anchor = sel.anchor ?? sel.focus ?? id;
  return { ids: new Set(span(order, anchor, id)), anchor, focus: id };
}

export function selectAll(sel: Selection, order: readonly string[]): Selection {
  if (!order.length) return EMPTY_SELECTION;
  const focus = sel.focus && order.includes(sel.focus) ? sel.focus : order[0];
  return { ids: new Set(order), anchor: sel.anchor && order.includes(sel.anchor) ? sel.anchor : order[0], focus };
}

/** Forget items that are no longer listed (moved, trashed, filtered out). */
export function reconcile(sel: Selection, order: readonly string[]): Selection {
  if (!sel.ids.size && sel.focus === null) return sel;
  const present = new Set(order);
  const ids = [...sel.ids].filter((id) => present.has(id));
  const focus = sel.focus !== null && present.has(sel.focus) ? sel.focus : null;
  const anchor = sel.anchor !== null && present.has(sel.anchor) ? sel.anchor : null;
  if (ids.length === sel.ids.size && focus === sel.focus && anchor === sel.anchor) return sel;
  return { ids: new Set(ids), anchor, focus };
}

/** Index of the focused item in `order` (-1 if none). */
export const focusIndex = (sel: Selection, order: readonly string[]) => (sel.focus === null ? -1 : order.indexOf(sel.focus));

/** The selected items, in list order. */
export function selectedItems<T extends { id: string }>(sel: Selection, items: readonly T[]): T[] {
  return sel.ids.size ? items.filter((e) => sel.ids.has(e.id)) : [];
}

/**
 * After `removed` items disappear, the item to continue with: the first
 * remaining item at or after the position of `current`, else the one before.
 */
export function neighborAfterRemoval(order: readonly string[], removed: ReadonlySet<string>, current: string | null): string | null {
  const start = current === null ? -1 : order.indexOf(current);
  if (start < 0) return null;
  for (let i = start; i < order.length; i++) if (!removed.has(order[i])) return order[i];
  for (let i = start - 1; i >= 0; i--) if (!removed.has(order[i])) return order[i];
  return null;
}
