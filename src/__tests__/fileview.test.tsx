// @vitest-environment jsdom
import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, beforeAll, describe, expect, it, vi } from "vitest";
import type { Entry } from "../api";
import { canDropInto, dragged, endDrag } from "../dnd";
import { FileView } from "../components/FileView";

beforeAll(() => {
  // Layout for the virtualized view: a 1000×800 scroller.
  Object.defineProperty(HTMLElement.prototype, "clientWidth", { configurable: true, get: () => 1000 });
  Object.defineProperty(HTMLElement.prototype, "clientHeight", { configurable: true, get: () => 800 });
  globalThis.ResizeObserver = class {
    constructor(private cb: () => void) {}
    observe() {
      this.cb();
    }
    disconnect() {}
    unobserve() {}
  } as unknown as typeof ResizeObserver;
});
afterEach(() => {
  cleanup();
  endDrag();
});

const entry = (id: string, name: string, kind: Entry["kind"], extra: Partial<Entry> = {}): Entry => ({
  id,
  name,
  path: name,
  ext: "",
  kind,
  size: 10,
  modified: 0,
  created: null,
  ...extra,
});
// Documents and folders: no thumbnails to fetch in these tests.
const items = [
  entry("f1", "Archive", "folder"),
  entry("f2", "Vault", "folder", { guarded: true }),
  entry("d1", "a.txt", "document", { favorite: true }),
  entry("d2", "b.txt", "document"),
  entry("d3", "c.txt", "document"),
];

function setup(view: "grid" | "list" | "gallery" = "grid", selected: string[] = []) {
  const fns = {
    onClickItem: vi.fn(),
    onActivate: vi.fn(),
    onQuickLook: vi.fn(),
    onFavorite: vi.fn(),
    onDrop: vi.fn(),
    onContextMenu: vi.fn(),
    onDragStartItem: vi.fn((i: number) => (selected.includes(items[i].id) ? items.filter((e) => selected.includes(e.id)) : [items[i]])),
  };
  const r = render(
    <FileView
      items={items}
      view={view}
      focus={-1}
      selectedIds={new Set(selected)}
      locationKey="x"
      keyboardActive
      sort="name"
      desc={false}
      showLocation={false}
      baseLabel="Drive"
      onSort={() => {}}
      onMove={() => {}}
      onClear={() => {}}
      canDrop={(t) => canDropInto(t, dragged() ?? [], { readOnly: false })}
      {...fns}
    />,
  );
  const tile = (name: string) => r.container.querySelector(`[title="${name}"]`) as HTMLElement;
  return { ...r, fns, tile };
}

const dt = () => ({ setData: vi.fn(), setDragImage: vi.fn(), effectAllowed: "", dropEffect: "" });

describe("favorite star", () => {
  for (const view of ["grid", "gallery", "list"] as const) {
    it(`toggles without selecting, opening or Quick Look (${view})`, () => {
      const { fns, getAllByRole } = setup(view);
      const add = getAllByRole("button", { name: "Add to Favorites" })[0];
      fireEvent.click(add);
      fireEvent.doubleClick(add);
      expect(fns.onFavorite).toHaveBeenCalledTimes(1);
      expect(fns.onClickItem).not.toHaveBeenCalled();
      expect(fns.onActivate).not.toHaveBeenCalled();
      expect(fns.onQuickLook).not.toHaveBeenCalled();
    });
  }

  it("shows the state and its action, separately from selection", () => {
    const { getByRole, tile } = setup("grid", ["d1"]);
    const on = getByRole("button", { name: "Remove from Favorites" });
    expect(on.getAttribute("aria-pressed")).toBe("true");
    expect(on.className).toContain("on");
    // Selected and favorite are two independent states on the same card.
    expect(tile("a.txt").getAttribute("data-selected")).not.toBeNull();
    expect(tile("b.txt").getAttribute("data-selected")).toBeNull();
  });

  it("grabbing the star never drags the card", () => {
    const { fns, getByRole } = setup();
    const star = getByRole("button", { name: "Remove from Favorites" });
    const ev = fireEvent.dragStart(star, { dataTransfer: dt() });
    expect(ev).toBe(false); // cancelled
    expect(fns.onDragStartItem).not.toHaveBeenCalled();
  });

  it("is a static mark where favorites can't be saved (temporary sessions)", () => {
    const r = render(
      <FileView
        items={items}
        view="grid"
        focus={-1}
        selectedIds={new Set()}
        locationKey="x"
        keyboardActive
        sort="name"
        desc={false}
        showLocation={false}
        baseLabel="Drive"
        onSort={() => {}}
        onMove={() => {}}
        onClear={() => {}}
        onActivate={() => {}}
        onClickItem={() => {}}
        onContextMenu={() => {}}
        onDragStartItem={() => []}
        canDrop={() => ({ ok: false, reason: "" })}
        onDrop={() => {}}
      />,
    );
    expect(r.queryAllByRole("button", { name: /Favorites/ })).toHaveLength(0);
    expect(r.getAllByRole("img", { name: "Favorite" })).toHaveLength(1);
  });
});

describe("drag and drop", () => {
  it("dragging a selected item drags the whole selection onto a folder", () => {
    const { fns, tile } = setup("grid", ["d1", "d2", "d3"]);
    const data = dt();
    fireEvent.dragStart(tile("b.txt"), { dataTransfer: data });
    expect(dragged()?.map((e) => e.id)).toEqual(["d1", "d2", "d3"]);
    // Only an opaque marker travels with the drag, never a path.
    expect(data.setData).toHaveBeenCalledWith("application/x-mori-items", "3");
    const over = dt();
    fireEvent.dragOver(tile("Archive"), { dataTransfer: over });
    expect(over.dropEffect).toBe("move");
    expect(tile("Archive").getAttribute("data-drop")).toBe("ok");
    fireEvent.drop(tile("Archive"), { dataTransfer: dt() });
    expect(fns.onDrop).toHaveBeenCalledTimes(1);
    expect(fns.onDrop.mock.calls[0][0].id).toBe("f1");
    expect(dragged()).toBeNull();
  });

  it("an unselected item is dragged alone", () => {
    const { fns, tile } = setup("list", ["d1", "d2"]);
    fireEvent.dragStart(tile("c.txt"), { dataTransfer: dt() });
    expect(fns.onDragStartItem).toHaveBeenCalledWith(4);
    expect(dragged()?.map((e) => e.id)).toEqual(["d3"]);
  });

  it("a protected folder visibly refuses the drop and nothing moves", () => {
    const { fns, tile } = setup("grid", ["d1"]);
    fireEvent.dragStart(tile("a.txt"), { dataTransfer: dt() });
    const over = dt();
    fireEvent.dragOver(tile("Vault"), { dataTransfer: over });
    expect(over.dropEffect).toBe("none");
    expect(tile("Protected (Never Modify): nothing can be moved into it.").getAttribute("data-drop")).toBe("no");
    fireEvent.drop(tile("Protected (Never Modify): nothing can be moved into it."), { dataTransfer: dt() });
    expect(fns.onDrop).not.toHaveBeenCalled();
  });

  it("a folder can't be dropped onto itself; files aren't drop targets", () => {
    const { fns, tile } = setup("grid");
    fireEvent.dragStart(tile("Archive"), { dataTransfer: dt() });
    fireEvent.dragOver(tile("Archive"), { dataTransfer: dt() });
    fireEvent.drop(tile("A folder can't be moved into itself."), { dataTransfer: dt() });
    fireEvent.drop(tile("b.txt"), { dataTransfer: dt() });
    expect(fns.onDrop).not.toHaveBeenCalled();
  });

  it("clicks still select and double-clicks still open", () => {
    const { fns, tile } = setup();
    fireEvent.click(tile("b.txt"), { metaKey: true, ctrlKey: true });
    fireEvent.doubleClick(tile("Archive"));
    expect(fns.onClickItem).toHaveBeenCalledWith(3, expect.objectContaining({ toggle: true }));
    expect(fns.onActivate).toHaveBeenCalledWith(0);
  });
});
