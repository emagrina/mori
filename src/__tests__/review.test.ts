// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ReviewInfo, ReviewState } from "../api";
import { busy, createReviewService, EMPTY_STATE, mayAsk, RULES } from "../review";

const DAY = 24 * 3_600_000;
const V = "1.0.1";
const STORE: ReviewInfo = { store: "mac", nativePrompt: true, version: V };

function setup(info: ReviewInfo = STORE, stored: Partial<ReviewState> = {}) {
  let now = 1_000_000_000_000;
  let dialogOpen = false;
  let saved: ReviewState = { ...EMPTY_STATE, ...stored };
  const request = vi.fn(async () => true);
  const openPage = vi.fn(async () => {});
  const svc = createReviewService({
    info,
    load: async () => saved,
    save: async (s) => void (saved = s),
    request,
    openPage,
    now: () => now,
    busy: () => dialogOpen,
  });
  return {
    svc,
    request,
    openPage,
    saved: () => saved,
    advance: (ms: number) => ((now += ms), vi.advanceTimersByTime(Math.min(ms, 10_000))),
    setDialog: (open: boolean) => (dialogOpen = open),
    /** A new app launch on top of the saved state. */
    relaunch(id: number, temporary = false) {
      const next = createReviewService({ info, load: async () => saved, save: async (s) => void (saved = s), request, openPage, now: () => now, busy: () => dialogOpen });
      next.start(id, () => temporary);
      return next;
    },
  };
}

/** Lets the async success() bookkeeping and the settle timer run. */
async function settle() {
  await vi.advanceTimersByTimeAsync(RULES.settleMs + 50);
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("mayAsk", () => {
  const base = { ...EMPTY_STATE, sessions: 2, successes: 3 };
  it("never in the first session", () => expect(mayAsk({ ...base, sessions: 1, successes: 50 }, 0, V)).toBe("first session"));
  it("after enough tasks, or enough use", () => {
    expect(mayAsk(base, 0, V)).toBe(true);
    expect(mayAsk({ ...base, successes: 1 }, 0, V)).toBe("not enough use yet");
    expect(mayAsk({ ...base, successes: 1, usageMs: RULES.minUsageMs }, 0, V)).toBe(true);
  });
  it("not right after an error", () => {
    expect(mayAsk({ ...base, lastProblemAt: 0 }, 60_000, V)).toBe("recent error");
    expect(mayAsk({ ...base, lastProblemAt: 0 }, RULES.quietAfterProblemMs + 1, V)).toBe(true);
  });
  it("waits 30 days and more tasks before asking again", () => {
    const asked = { ...base, lastAskedAt: 0, successesSinceAsk: 10 };
    expect(mayAsk(asked, 29 * DAY, V)).toBe("asked recently");
    expect(mayAsk({ ...asked, successesSinceAsk: 2 }, 31 * DAY, V)).toBe("not enough tasks since last time");
    expect(mayAsk(asked, 31 * DAY, V)).toBe(true);
  });
  it("not again in a version the user rated", () => {
    expect(mayAsk({ ...base, ratedVersion: V }, 0, V)).toBe("already rated this version");
    expect(mayAsk({ ...base, ratedVersion: "1.0.0" }, 0, V)).toBe(true);
  });
});

describe("review service", () => {
  it("does nothing at all outside a store", async () => {
    const h = setup({ store: null, nativePrompt: false, version: V }, { sessions: 5, successes: 9 });
    h.svc.start(1, () => false);
    await h.svc.success("transfer");
    await settle();
    expect(h.request).not.toHaveBeenCalled();
    expect(h.svc.canRate).toBe(false);
    expect(h.saved().sessions).toBe(5);
  });

  it("first session: tasks are counted, nothing is asked", async () => {
    const h = setup();
    h.svc.start(1, () => false);
    for (let i = 0; i < 5; i++) await h.svc.success("quick-cleanup");
    await settle();
    expect(h.request).not.toHaveBeenCalled();
    expect(h.saved()).toMatchObject({ sessions: 1, successes: 5 });
  });

  it("second session: asks once, right after a success, then not again for 30 days", async () => {
    const h = setup();
    h.svc.start(1, () => false);
    await h.svc.success("transfer");
    const s2 = h.relaunch(2);
    await s2.success("duplicates");
    await settle();
    expect(h.request).not.toHaveBeenCalled(); // 2 tasks: not yet
    await s2.success("duplicates");
    await settle();
    expect(h.request).toHaveBeenCalledTimes(1);
    for (let i = 0; i < 10; i++) await s2.success("sanitize");
    await settle();
    h.advance(20 * DAY);
    await s2.success("sanitize");
    await settle();
    expect(h.request).toHaveBeenCalledTimes(1);
    h.advance(11 * DAY);
    await s2.success("sanitize");
    await settle();
    expect(h.request).toHaveBeenCalledTimes(2);
    expect(h.saved().askCount).toBe(2);
  });

  it("an interface reload isn't a new session", async () => {
    const h = setup();
    h.svc.start(7, () => false);
    await Promise.resolve();
    h.relaunch(7);
    await settle();
    expect(h.saved().sessions).toBe(1);
  });

  it("~10 minutes of use over two sessions asks at the next success", async () => {
    const h = setup(STORE, { sessions: 1, lastLaunch: 1 });
    const s = h.relaunch(2);
    await settle();
    for (let i = 0; i < 41; i++) {
      h.advance(RULES.tickMs);
      s.tick(true);
    }
    expect(s.state.usageMs).toBeGreaterThanOrEqual(RULES.minUsageMs);
    expect(h.request).not.toHaveBeenCalled(); // never interrupts: waits for a success moment
    await s.success("empty-folders");
    await settle();
    expect(h.request).toHaveBeenCalledTimes(1);
  });

  it("time in the background doesn't count", async () => {
    const h = setup(STORE, { sessions: 1, lastLaunch: 1 });
    const s = h.relaunch(2);
    for (let i = 0; i < 60; i++) {
      h.advance(RULES.tickMs);
      s.tick(false);
    }
    expect(s.state.usageMs).toBe(0);
  });

  it("waits for open dialogs to close, and gives up after a while", async () => {
    const h = setup(STORE, { sessions: 3, successes: 5, lastLaunch: 1 });
    const s = h.relaunch(2);
    h.setDialog(true);
    await s.success("quick-cleanup");
    await settle();
    await vi.advanceTimersByTimeAsync(5_000);
    expect(h.request).not.toHaveBeenCalled();
    h.setDialog(false);
    await vi.advanceTimersByTimeAsync(1_100);
    expect(h.request).toHaveBeenCalledTimes(1);

    // Another success, dialog left open past the window: dropped.
    h.advance(31 * DAY);
    for (let i = 0; i < RULES.successesBetweenAsks; i++) await s.success("transfer");
    h.setDialog(true);
    await vi.advanceTimersByTimeAsync(RULES.pendingMs + 5_000);
    h.advance(RULES.pendingMs + 5_000);
    h.setDialog(false);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(h.request).toHaveBeenCalledTimes(1);
  });

  it("an error right after a success cancels it, and blocks asking for a while", async () => {
    const h = setup(STORE, { sessions: 3, successes: 5, lastLaunch: 1 });
    const s = h.relaunch(2);
    await s.success("transfer");
    s.problem();
    await settle();
    expect(h.request).not.toHaveBeenCalled();
    h.advance(60_000);
    await s.success("transfer");
    await settle();
    expect(h.request).not.toHaveBeenCalled();
    h.advance(RULES.quietAfterProblemMs);
    await s.success("transfer");
    await settle();
    expect(h.request).toHaveBeenCalledTimes(1);
  });

  it("temporary sessions count nothing and never ask", async () => {
    const h = setup(STORE, { sessions: 3, successes: 5, lastLaunch: 1 });
    const s = h.relaunch(2, true);
    await s.success("transfer");
    await settle();
    expect(h.request).not.toHaveBeenCalled();
    expect(h.saved()).toMatchObject({ sessions: 3, successes: 5 });
  });

  it("Rate opens the review page and stops asking in this version", async () => {
    const h = setup(STORE, { sessions: 3, successes: 5, lastLaunch: 1 });
    const s = h.relaunch(2);
    await s.rate();
    expect(h.openPage).toHaveBeenCalledTimes(1);
    await s.success("transfer");
    await settle();
    expect(h.request).not.toHaveBeenCalled();
    expect(h.saved().ratedVersion).toBe(V);
  });

  it("without a native prompt (Microsoft Store) only the menu item exists", async () => {
    const h = setup({ store: "microsoft", nativePrompt: false, version: V }, { sessions: 3, successes: 5, lastLaunch: 1 });
    const s = h.relaunch(2);
    await s.success("transfer");
    await settle();
    expect(h.request).not.toHaveBeenCalled();
    expect(s.canRate).toBe(true);
  });
});

describe("busy", () => {
  it("sees dialogs, menus and a window in the background", () => {
    vi.spyOn(document, "hasFocus").mockReturnValue(true);
    expect(busy()).toBe(false);
    document.body.innerHTML = '<div role="menu"></div>';
    expect(busy()).toBe(true);
    document.body.innerHTML = '<div class="dialog-backdrop"><div class="dialog" role="dialog"></div></div>';
    expect(busy()).toBe(true);
    document.body.innerHTML = "";
    vi.spyOn(document, "hasFocus").mockReturnValue(false);
    expect(busy()).toBe(true);
  });
});
