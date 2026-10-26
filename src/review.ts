import { api, type ReviewInfo, type ReviewState } from "./api";

/**
 * When to ask for a store rating, in one place.
 *
 * Mori asks through the system's own prompt (StoreKit on the Mac App Store),
 * never with a dialog of its own, and only right after the user finishes
 * something that went well (a success moment):
 *   - Quick Cleanup reviewed and moved to the Trash,
 *   - Exact Duplicates / Similar Media cleaned up,
 *   - empty folders removed,
 *   - files moved or copied,
 *   - sanitized copies created,
 * and only with nothing failed, no other dialog or menu open, the window in
 * front, and no error in the last few minutes. Never in the first session or
 * a temporary one. The system may still decide not to show the prompt.
 *
 * Builds outside a store (today's) do nothing here: no counting, no prompt.
 */
export const RULES = {
  /** Never in the first session. */
  minSessions: 2,
  /** Ask after this many completed tasks… */
  minSuccesses: 3,
  /** …or after this much use (window in front), at the next success moment. */
  minUsageMs: 10 * 60_000,
  /** After asking: wait this long… */
  cooldownMs: 30 * 24 * 3_600_000,
  /** …and this many more completed tasks. */
  successesBetweenAsks: 5,
  /** No asking this soon after an error. */
  quietAfterProblemMs: 10 * 60_000,
  /** A success moment stays valid this long while a dialog is still open. */
  pendingMs: 2 * 60_000,
  /** Pause before asking, so the result of the task is seen first. */
  settleMs: 1500,
  /** Usage is sampled this often (and saved every few samples). */
  tickMs: 15_000,
};

export type SuccessKind = "quick-cleanup" | "duplicates" | "similar" | "empty-folders" | "transfer" | "sanitize";

export const EMPTY_STATE: ReviewState = {
  sessions: 0,
  usageMs: 0,
  successes: 0,
  successesSinceAsk: 0,
  firstSeenAt: null,
  lastAskedAt: null,
  askCount: 0,
  ratedVersion: null,
  lastProblemAt: null,
  lastLaunch: null,
};

/** Whether a success moment now may ask; the reason when not (for tests and debugging). */
export function mayAsk(s: ReviewState, now: number, version: string): true | string {
  if (s.ratedVersion === version) return "already rated this version";
  if (s.sessions < RULES.minSessions) return "first session";
  if (s.lastProblemAt !== null && now - s.lastProblemAt < RULES.quietAfterProblemMs) return "recent error";
  if (s.lastAskedAt !== null) {
    if (now - s.lastAskedAt < RULES.cooldownMs) return "asked recently";
    if (s.successesSinceAsk < RULES.successesBetweenAsks) return "not enough tasks since last time";
    return true;
  }
  if (s.successes >= RULES.minSuccesses || s.usageMs >= RULES.minUsageMs) return true;
  return "not enough use yet";
}

/** Something else is on screen (dialog, menu, palette, preview, Quick Cleanup), or Mori isn't in front. */
export function busy(doc: Document = document): boolean {
  return doc.visibilityState !== "visible" || !doc.hasFocus() || !!doc.querySelector('[role="dialog"], [role="menu"], .dialog-backdrop, .palette-backdrop');
}

interface Deps {
  info: ReviewInfo;
  load: () => Promise<ReviewState>;
  save: (s: ReviewState) => Promise<void>;
  request: () => Promise<boolean>;
  openPage: () => Promise<void>;
  now: () => number;
  busy: () => boolean;
}

export function createReviewService(d: Deps) {
  const enabled = d.info.store !== null;
  let state: ReviewState = { ...EMPTY_STATE };
  let ready: Promise<void> = Promise.resolve();
  let temporary = () => false;
  let pendingUntil = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let lastTick = 0;
  let ticks = 0;
  const save = () => d.save(state).catch(() => {});

  const tryAsk = () => {
    timer = null;
    if (!pendingUntil || d.now() > pendingUntil || temporary()) return void (pendingUntil = 0);
    if (d.busy()) return void (timer = setTimeout(tryAsk, 1000));
    if (mayAsk(state, d.now(), d.info.version) !== true) return void (pendingUntil = 0);
    pendingUntil = 0;
    state = { ...state, lastAskedAt: d.now(), askCount: state.askCount + 1, successesSinceAsk: 0 };
    save();
    d.request().catch(() => {});
  };

  return {
    /** Whether "Rate Mori…" belongs in the menu. */
    canRate: enabled,
    /** One per app launch (an interface reload with the same launch id isn't a new session). */
    start(launchId: number, isTemporary: () => boolean) {
      temporary = isTemporary;
      if (!enabled) return;
      ready = d.load().then((s) => {
        state = { ...EMPTY_STATE, ...s };
        if (temporary() || state.lastLaunch === launchId) return;
        state = { ...state, sessions: state.sessions + 1, lastLaunch: launchId, firstSeenAt: state.firstSeenAt ?? d.now() };
        save();
      }, () => {});
      lastTick = d.now();
    },
    /** Called every RULES.tickMs: adds time while Mori is in front. */
    tick(inFront: boolean) {
      const now = d.now();
      const elapsed = Math.min(now - lastTick, RULES.tickMs * 2);
      lastTick = now;
      if (!enabled || temporary() || !inFront) return;
      state = { ...state, usageMs: state.usageMs + elapsed };
      if (++ticks % 4 === 0) save();
    },
    /** A task finished without problems: maybe ask, once nothing else is on screen. */
    async success(_kind: SuccessKind) {
      if (!enabled || temporary()) return;
      await ready;
      state = { ...state, successes: state.successes + 1, successesSinceAsk: state.successesSinceAsk + 1 };
      save();
      if (!d.info.nativePrompt || mayAsk(state, d.now(), d.info.version) !== true) return;
      pendingUntil = d.now() + RULES.pendingMs;
      if (timer) clearTimeout(timer);
      timer = setTimeout(tryAsk, RULES.settleMs);
    },
    /** Something failed: no asking for a while, and a pending success moment is dropped. */
    problem() {
      if (!enabled) return;
      pendingUntil = 0;
      state = { ...state, lastProblemAt: d.now() };
    },
    /** "Rate Mori…": the store's review page; not asked again in this version. */
    async rate() {
      await d.openPage();
      state = { ...state, ratedVersion: d.info.version };
      save();
    },
    /** For tests. */
    get state() {
      return state;
    },
  };
}

export type ReviewService = ReturnType<typeof createReviewService>;

const OFF: ReviewInfo = { store: null, nativePrompt: false, version: "" };

/** The app's service; inert until init() learns this build is in a store. */
export let review: ReviewService = createReviewService({ info: OFF, load: api.reviewState, save: api.saveReviewState, request: api.requestReview, openPage: api.openStoreReview, now: Date.now, busy });

export async function initReview(launchId: number, isTemporary: () => boolean) {
  const info = await api.reviewInfo().catch(() => OFF);
  review = createReviewService({ info, load: api.reviewState, save: api.saveReviewState, request: api.requestReview, openPage: api.openStoreReview, now: Date.now, busy });
  review.start(launchId, isTemporary);
  if (review.canRate) setInterval(() => review.tick(document.visibilityState === "visible" && document.hasFocus()), RULES.tickMs);
  return review;
}
