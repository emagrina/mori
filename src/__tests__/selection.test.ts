import { describe, expect, it } from "vitest";
import { click, EMPTY_SELECTION, focusIndex, moveTo, neighborAfterRemoval, reconcile, selectAll, selectedItems, selectOnly } from "../selection";

const order = ["a", "b", "c", "d", "e", "f"];
const ids = (s: { ids: ReadonlySet<string> }) => [...s.ids].sort();
const plain = { toggle: false, range: false };
const toggle = { toggle: true, range: false };
const range = { toggle: false, range: true };

describe("selection", () => {
  it("a plain click selects only that item (it never opens anything)", () => {
    const s = click(selectOnly("a"), order, "c", plain);
    expect(ids(s)).toEqual(["c"]);
    expect(s.focus).toBe("c");
    expect(s.anchor).toBe("c");
  });

  it("Cmd/Ctrl+Click adds and removes single items", () => {
    let s = click(EMPTY_SELECTION, order, "b", plain);
    s = click(s, order, "d", toggle);
    s = click(s, order, "f", toggle);
    expect(ids(s)).toEqual(["b", "d", "f"]);
    s = click(s, order, "d", toggle);
    expect(ids(s)).toEqual(["b", "f"]);
  });

  it("Shift+Click selects the range from the anchor, in either direction", () => {
    let s = click(EMPTY_SELECTION, order, "c", plain);
    s = click(s, order, "e", range);
    expect(ids(s)).toEqual(["c", "d", "e"]);
    // The anchor stays: a second Shift+Click re-ranges from it.
    s = click(s, order, "a", range);
    expect(ids(s)).toEqual(["a", "b", "c"]);
    expect(s.anchor).toBe("c");
  });

  it("Cmd/Ctrl+Shift+Click adds a range to the selection", () => {
    let s = click(EMPTY_SELECTION, order, "a", plain);
    s = click(s, order, "e", toggle);
    s = click(s, order, "f", { toggle: true, range: true });
    expect(ids(s)).toEqual(["a", "e", "f"]);
  });

  it("Select All, then Escape-style clearing", () => {
    const s = selectAll(selectOnly("c"), order);
    expect(s.ids.size).toBe(6);
    expect(s.focus).toBe("c");
    expect(selectAll(EMPTY_SELECTION, [])).toBe(EMPTY_SELECTION);
    expect(selectOnly(null)).toBe(EMPTY_SELECTION);
  });

  it("arrows move the focus; Shift+arrows extend from the anchor", () => {
    let s = moveTo(EMPTY_SELECTION, order, 1, false);
    expect(ids(s)).toEqual(["b"]);
    s = moveTo(s, order, 2, true);
    s = moveTo(s, order, 3, true);
    expect(ids(s)).toEqual(["b", "c", "d"]);
    s = moveTo(s, order, 0, true);
    expect(ids(s)).toEqual(["a", "b"]);
    expect(focusIndex(s, order)).toBe(0);
    // Clamped at the ends.
    expect(moveTo(s, order, 99, false).focus).toBe("f");
  });

  it("forgets items that disappeared or moved, and keeps the object when nothing changed", () => {
    let s = click(selectOnly("b"), order, "d", toggle);
    expect(reconcile(s, order)).toBe(s);
    s = reconcile(s, ["a", "b", "c", "e", "f"]);
    expect(ids(s)).toEqual(["b"]);
    expect(s.focus).toBeNull();
  });

  it("identifies items by id, never by name", () => {
    const items = [
      { id: "1", name: "IMG_0001.jpg" },
      { id: "2", name: "IMG_0001.jpg" },
      { id: "3", name: "other.jpg" },
    ];
    const s = click(
      EMPTY_SELECTION,
      items.map((e) => e.id),
      "2",
      plain,
    );
    expect(selectedItems(s, items)).toEqual([items[1]]);
  });

  it("continues with the next sensible item after a removal", () => {
    expect(neighborAfterRemoval(order, new Set(["c"]), "c")).toBe("d");
    expect(neighborAfterRemoval(order, new Set(["c", "d", "e", "f"]), "c")).toBe("b");
    expect(neighborAfterRemoval(order, new Set(order), "c")).toBeNull();
  });

  it("stays fast with very large folders", () => {
    const big = Array.from({ length: 50_000 }, (_, i) => `id${i}`);
    const t = performance.now();
    let s = click(EMPTY_SELECTION, big, "id10", plain);
    s = click(s, big, "id49000", range);
    s = selectAll(s, big);
    s = reconcile(s, big.slice(1000));
    expect(s.ids.size).toBe(49_000);
    expect(performance.now() - t).toBeLessThan(500);
  });
});
