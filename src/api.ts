import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

export type Kind = "folder" | "photo" | "video" | "gif" | "document" | "audio" | "other" | "link";
export type KindFilter = "all" | Exclude<Kind, "folder" | "link"> | "screenshot" | "recording";
/** What a library view shows: a kind, Favorites, or one tag. */
export type ViewKind = KindFilter | "favorites" | `tag:${number}`;
export type ViewMode = "gallery" | "grid" | "list";
export type SortKey = "name" | "modified" | "created" | "size" | "type";
export type Scope = "folder" | "library";

/**
 * One indexed item. `id` is an opaque handle; `name` and `path` are
 * display-only strings (already sanitised by Rust, always rendered as text).
 */
export interface Entry {
  id: string;
  name: string;
  path: string;
  ext: string;
  kind: Kind;
  size: number;
  modified: number;
  created: number | null;
  /** Folder relative to the current view ("" = directly in it). */
  location?: string;
  /** A private folder: its contents aren't surfaced outside it. */
  private?: boolean;
  /** A protected folder ("Never Modify"). */
  protected?: boolean;
  /** Symbolic links: where the link points (never followed). */
  link?: string;
  /** The name alone shows a high-attention pattern (see the inspector). */
  flagged?: boolean;
  /** 1 = screenshot, 2 = screen recording (Mori's guess, correctable). */
  capture?: number;
  favorite?: boolean;
  /** Local tag ids (names from `api.tagsList`). */
  tags?: number[];
}

export interface Status {
  hasRoot: boolean;
  rootName: string;
  scanning: boolean;
  scanCount: number;
  fileCount: number;
  scannedAt: number;
  /** Safe Inspection Mode: nothing on this drive is decoded automatically. */
  safeMode: boolean;
  /** Media decoded since this folder was opened. */
  decoded: number;
  /** Temporary session: nothing about this folder is saved. */
  temporary: boolean;
}

export interface TagInfo {
  id: number;
  name: string;
  count: number;
}

export interface InitInfo {
  status: Status;
  view: ViewMode | null;
  sort: SortKey | null;
  desc: boolean | null;
  recursive: boolean | null;
  searchGlobal: boolean | null;
  readOnly: boolean;
  platform: string;
  launchId: number;
  translocated: boolean;
}

export interface Crumb {
  id: string;
  name: string;
}

export interface QueryResult {
  items: Entry[];
  total: number;
  truncated: boolean;
  crumbs: Crumb[];
  /** The current folder is private, or inside a private folder. */
  privateScope?: boolean;
  /** The current folder is protected, or inside a protected folder. */
  protectedScope?: boolean;
}

export type Stats = Record<"files" | "folders" | "bytes" | "links" | Exclude<KindFilter, "all">, number>;

// ------------------------------------------------------------- inspection

export type RiskLevel = "info" | "attention" | "high";

export interface Finding {
  level: RiskLevel;
  code: string;
  title: string;
  detail: string;
}

export interface FileType {
  id: string;
  label: string;
  family: string;
  mime: string;
}

export interface Permissions {
  mode: string;
  octal: string;
  owner: string;
  group: string;
  special: string[];
  flags: string[];
  xattrs: string[];
  aclEntries: number | null;
  hardLinks: number;
}

export interface FileReport {
  name: string;
  path: string;
  kind: "file" | "folder" | "link";
  size: number;
  modified: number | null;
  created: number | null;
  accessed: number | null;
  ext: string;
  detected: FileType | null;
  extMatches: boolean | null;
  findings: Finding[];
  summary: string;
  permissions: Permissions | null;
  linkTarget: string | null;
  linkOutside: boolean;
  marks: { private: boolean; insidePrivate: boolean; protected: boolean; insideProtected: boolean };
}

export interface Query {
  /** Folder id ("" = root). */
  folder: string;
  scope: Scope;
  kind: ViewKind;
  search: string;
  sort: SortKey;
  desc: boolean;
  /** Include subfolders: flatten everything below `folder`. */
  recursive: boolean;
  /** Search the whole drive instead of the current folder. */
  global: boolean;
}

export type VideoStatus = "playable" | "unsupportedCodec" | "noVideo" | "damaged" | "blocked";

export interface VideoInfo {
  status: VideoStatus;
  container: string;
  videoCodec: string | null;
  audioCodec: string | null;
  width: number;
  height: number;
  durationMs: number;
}

export interface Inspection {
  detected: string;
  preview: "image" | "video" | "text" | "pdf" | "archive" | "audio" | "none";
  canOpen: boolean;
  mismatch: boolean;
  video: VideoInfo | null;
  /** Safe Inspection Mode: only explicit, isolated views are allowed. */
  previewsOff: boolean;
}

export interface PdfInfo {
  pages: number;
  encrypted: boolean;
  locked: boolean;
  title: string | null;
  author: string | null;
  creator: string | null;
  producer: string | null;
  subject: string | null;
  /** Present in the document. Mori never runs it. */
  javascript: boolean;
  openAction: boolean;
  embeddedFiles: boolean;
  forms: boolean;
}

export type ArchiveFlag = "traversal" | "absolute" | "drive-letter" | "control-chars" | "nested-archive" | "high-ratio";

export interface ArchiveEntry {
  path: string;
  size: number;
  compressed: number;
  dir: boolean;
  symlink: boolean;
  encrypted: boolean;
  flags: ArchiveFlag[];
  nested?: ArchiveListing;
}

export interface ArchiveListing {
  format: string;
  entries: ArchiveEntry[];
  totalEntries: number;
  totalSize: number;
  totalCompressed: number;
  nestedDepth: number;
  truncated: boolean;
  findings: Finding[];
}

export type MetaCategory = "location" | "person" | "device" | "software" | "comment" | "identifier";

export interface MetaField {
  group: string;
  name: string;
  value: string;
  sensitive?: MetaCategory;
}

export interface FileMeta {
  container: string;
  fields: MetaField[];
  gps?: [number, number];
  orientation?: number;
  partial: boolean;
  categories: MetaCategory[];
  sanitizable: boolean;
}

export interface MetaProgress {
  stage: "collecting" | "reading";
  done: number;
  total: number;
  found: number;
  failed: number;
  paused: boolean;
}

export interface MetaHit {
  id: string;
  name: string;
  path: string;
  location: string;
  drive: string;
  size: number;
  modified: number;
  created: number | null;
  kind: Kind;
  ext: string;
  categories: MetaCategory[];
  gps: [number, number] | null;
  fields: MetaField[];
  sanitizable: boolean;
}

export interface MetaView {
  locations: LocationInfo[];
  stats: { scanned: number; withSensitive: number; failed: number; unreadable: number; places: number };
  counts: Partial<Record<MetaCategory, number>>;
  total: number;
  hits: MetaHit[];
}

export interface Places {
  ids: string[];
  names: string[];
  /** Flat [lat, lon, lat, lon, …]. */
  coords: number[];
}

export interface SanitizeOutcome {
  id: string;
  name: string;
  newName: string | null;
  error: string | null;
}

export const CATEGORY_LABEL: Record<MetaCategory, string> = {
  location: "Location",
  person: "People & authors",
  device: "Device",
  software: "Software",
  comment: "Comments & descriptions",
  identifier: "Unique IDs",
};

export interface StorageBucket {
  key: string;
  bytes: number;
  count: number;
}

export interface StorageTile {
  kind: "folder" | "file" | "files" | "private";
  id: string;
  name: string;
  bytes: number;
  files: number;
}

export interface StorageReport {
  totalBytes: number;
  totalFiles: number;
  byKind: StorageBucket[];
  byYear: StorageBucket[];
  largestFiles: Entry[];
  largestVideos: Entry[];
  largestImages: Entry[];
  largestFolders: { id: string; name: string; path: string; bytes: number; files: number }[];
  folder: string;
  folderBytes: number;
  tiles: StorageTile[];
  privateFolders: number;
}

export interface EmptyFolder {
  id: string;
  name: string;
  path: string;
  nested: number;
  protected: boolean;
  /** What the on-disk check found ("" = empty). */
  note: string;
}

export type HealthCategory = "risk" | "broken" | "unsupported" | "failed";

export interface HealthItem extends Entry {
  category: HealthCategory;
  reason: string;
}

export interface HealthView {
  checked: number;
  counts: Partial<Record<HealthCategory, number>>;
  total: number;
  items: HealthItem[];
}

export interface HealthProgress {
  done: number;
  total: number;
  found: number;
  paused: boolean;
}

export interface ConnectedDrive {
  key: string;
  label: string;
  path: string;
}

export interface Settings {
  view: ViewMode;
  sort: SortKey;
  desc: boolean;
  recursive: boolean;
  searchGlobal: boolean;
}

export interface Failure {
  /** Display-only path (sanitised). */
  path: string;
  reason: string;
}

export interface TrashSummary {
  files: number;
  folders: number;
  bytes: number;
}

export interface TrashResult {
  trashed: string[];
  bytes: number;
  failed: Failure[];
}

// ------------------------------------------------------ duplicate analyzer

export type AnalysisKind = "images" | "videos" | "documents" | "audio" | "other";

export interface LocationInfo {
  /** Opaque key ("library", "pictures", "custom-0"…); the UI never sends paths. */
  key: string;
  label: string;
  path: string;
  /** Storage device the folder is on. */
  drive: string;
}

export type AnalysisStage = "collecting" | "comparingSizes" | "fingerprinting" | "verifying" | "done";

export interface AnalysisProgress {
  stage: AnalysisStage;
  filesTotal: number;
  filesDone: number;
  bytesTotal: number;
  bytesDone: number;
  groups: number;
  recoverable: number;
}

export interface AnalysisDone {
  status: "done" | "cancelled" | "failed";
  message: string | null;
}

export interface DupFile {
  id: string;
  name: string;
  path: string;
  location: string;
  drive: string;
  size: number;
  modified: number;
  created: number | null;
  kind: Exclude<Kind, "folder">;
  ext: string;
}

/** One copy of the content: a file, or both halves of a Live Photo. */
export interface DupMember {
  files: DupFile[];
  /** Half of a Live Photo whose other half isn't duplicated: never trashed alone. */
  locked: boolean;
}

export interface DupGroup {
  index: number;
  live: boolean;
  unitSize: number;
  recoverable: number;
  suggested: number;
  /** Why the suggested copy was picked. */
  reasons: string[];
  members: DupMember[];
}

export interface AnalysisStats {
  scanned: number;
  emptyIgnored: number;
  unreadable: number;
  changed: number;
  hardlinksSkipped: number;
  livePairs: number;
}

export interface AnalysisView {
  locations: LocationInfo[];
  stats: AnalysisStats;
  groups: DupGroup[];
}

export interface PlanItem {
  group: number;
  /** Member indexes to move to Trash. */
  trash: number[];
}

export interface CleanupOutcome {
  trashedFiles: number;
  trashedBytes: number;
  keptFiles: number;
  groupsCleaned: number;
  groupsSkipped: number;
  failures: Failure[];
  cancelled: boolean;
}

// ------------------------------------------------------------ similar media

export type Sensitivity = "strict" | "balanced" | "broad";
export type SimStage = "collecting" | "photos" | "videos" | "comparing" | "done";

export interface SimProgress {
  stage: SimStage;
  photosTotal: number;
  photosDone: number;
  videosTotal: number;
  videosDone: number;
  compareTotal: number;
  compareDone: number;
  groups: number;
}

export interface SimFile extends DupFile {
  width: number;
  height: number;
  durationMs: number | null;
  container: string | null;
  codec: string | null;
  exif: boolean;
}

export interface SimMember {
  files: SimFile[];
  /** Estimated similarity to the suggested copy (0–99); 100 only when the bytes are verified identical. */
  similarity: number;
  exact: boolean;
}

export interface SimGroup {
  index: number;
  video: boolean;
  similarity: number;
  recoverable: number;
  /** Why member 0 is preferred. */
  reasons: string[];
  /** Photos taken in quick succession by one camera. */
  burst: { frames: number; spanMs: number } | null;
  /** Member 0 is Mori's suggested copy to keep. */
  members: SimMember[];
}

export interface SimStats {
  photos: number;
  videos: number;
  unanalyzable: number;
  uninformative: number;
  cached: number;
  hashComparisons: number;
  verified: number;
  matches: number;
  dismissed: number;
  livePairs: number;
  elapsedMs: number;
}

export interface SimView {
  locations: LocationInfo[];
  stats: SimStats;
  groups: SimGroup[];
}

export const api = {
  init: () => invoke<InitInfo>("init"),
  updateSettings: (s: Settings) => invoke<void>("update_settings", { ...s }),
  chooseRoot: () => invoke<Status>("choose_root"),
  rescan: () => invoke<void>("rescan"),
  clearCache: () => invoke<void>("clear_cache"),
  status: () => invoke<Status>("get_status"),
  query: (q: Query) => invoke<QueryResult>("query", { q }),
  stats: () => invoke<Stats>("stats"),
  subfolders: (parent: string) => invoke<Entry[]>("subfolders", { parent }),
  inspect: (id: string) => invoke<Inspection>("inspect", { id }),
  previewInfo: (id: string) => invoke<[number, number] | null>("preview_info", { id }),
  readText: (id: string) => invoke<string>("read_text", { id }),
  openFile: (id: string) => invoke<void>("open_file", { id }),
  revealFile: (id: string) => invoke<void>("reveal_file", { id }),
  copyPath: (id: string) => invoke<string>("copy_path", { id }),
  takeRecovered: () => invoke<string[]>("take_recovered"),
  trashSummary: (ids: string[]) => invoke<TrashSummary>("trash_summary", { ids }),
  /** Moves items to the system Trash / Recycle Bin. Never deletes permanently. */
  trashItems: (ids: string[]) => invoke<TrashResult>("trash_items", { ids }),
  /** Returns the item's new id. Never overwrites an existing item. */
  renameItem: (id: string, name: string) => invoke<string>("rename_item", { id, name }),
  analysisLocations: () => invoke<LocationInfo[]>("analysis_locations"),
  analysisChooseFolder: () => invoke<LocationInfo>("analysis_choose_folder"),
  analysisStart: (locations: string[], kinds: AnalysisKind[], recursive: boolean) => invoke<void>("analysis_start", { locations, kinds, recursive }),
  analysisCancel: () => invoke<void>("analysis_cancel"),
  analysisResults: () => invoke<AnalysisView | null>("analysis_results"),
  analysisClear: () => invoke<void>("analysis_clear"),
  analysisCleanup: (plan: PlanItem[]) => invoke<CleanupOutcome>("analysis_cleanup", { plan }),
  analysisCleanupCancel: () => invoke<void>("analysis_cleanup_cancel"),
  similarStart: (locations: string[], photos: boolean, videos: boolean, recursive: boolean, sensitivity: Sensitivity) =>
    invoke<void>("similar_start", { locations, photos, videos, recursive, sensitivity }),
  similarCancel: () => invoke<void>("similar_cancel"),
  similarResults: () => invoke<SimView | null>("similar_results"),
  /** "Not duplicates": the whole group, or one member. Remembered locally by content identity. */
  similarDismiss: (group: number, member: number | null) => invoke<void>("similar_dismiss", { group, member }),
  similarDismissedCount: () => invoke<number>("similar_dismissed_count"),
  similarForgetDecisions: () => invoke<void>("similar_forget_decisions"),
  similarClear: () => invoke<void>("similar_clear"),
  similarCleanup: (plan: PlanItem[]) => invoke<CleanupOutcome>("similar_cleanup", { plan }),
  /** Mark a folder private (a Mori visibility boundary) or public. Never touches the folder. */
  setFolderPrivate: (id: string, isPrivate: boolean) => invoke<void>("set_folder_private", { id, private: isPrivate }),
  /** Factual report: real type, risk indicators, permissions. Nothing is decoded or followed. */
  fileReport: (id: string) => invoke<FileReport>("file_report", { id }),
  /** Enforced by the backend mutation policy, not just the UI. */
  setReadOnly: (on: boolean) => invoke<void>("set_read_only", { on }),
  setFolderProtected: (id: string, isProtected: boolean) => invoke<void>("set_folder_protected", { id, protected: isProtected }),
  /** Read by the sandboxed worker; active content is reported, never run. */
  pdfInfo: (id: string, explicit: boolean) => invoke<PdfInfo>("pdf_info", { id, explicit }),
  /** Listing only: nothing is extracted or written. */
  archiveListing: (id: string) => invoke<ArchiveListing>("archive_listing", { id }),
  openDriveSafely: (key: string) => invoke<Status>("open_drive_safely", { key }),
  /** "Generate previews" for the current drive (or back to metadata only). */
  setDrivePreviews: (on: boolean) => invoke<void>("set_drive_previews", { on }),
  /** Only Mori's records change; files are never touched. */
  setFavorite: (ids: string[], on: boolean) => invoke<void>("set_favorite", { ids, on }),
  tagsList: () => invoke<TagInfo[]>("tags_list"),
  /** Adds (or removes) the tag named `name`, creating it if needed. Never writes into the files. */
  tagItems: (ids: string[], name: string, on: boolean) => invoke<number>("tag_items", { ids, name, on }),
  tagRename: (id: number, name: string) => invoke<void>("tag_rename", { id, name }),
  tagDelete: (id: number) => invoke<void>("tag_delete", { id }),
  /** Browse without indexing: nothing about the folder is written to disk. */
  openTemporary: () => invoke<Status>("open_temporary"),
  endTemporary: () => invoke<Status>("end_temporary"),
  /** Removes Mori's knowledge of the current drive. Nothing on the drive is changed. */
  forgetDrive: () => invoke<{ drive: string; indexes: number }>("forget_drive"),
  /** Clears in-memory results of this session. Files, caches and records are kept. */
  clearSessionData: () => invoke<void>("clear_session_data"),
  /** From the index: nothing is read from disk. `folder` = folder id ("" = drive). */
  storageReport: (folder: string) => invoke<StorageReport>("storage_report", { folder }),
  /** Index candidates, each re-checked on disk. */
  emptyFolders: () => invoke<EmptyFolder[]>("empty_folders"),
  /** Re-verified right before; Trash only. */
  trashEmptyFolders: (ids: string[]) => invoke<TrashResult>("trash_empty_folders", { ids }),
  healthStart: () => invoke<void>("health_start"),
  healthPause: (paused: boolean) => invoke<void>("health_pause", { paused }),
  healthCancel: () => invoke<void>("health_cancel"),
  healthClear: () => invoke<void>("health_clear"),
  healthResults: (category: HealthCategory | null, offset: number, limit: number) => invoke<HealthView | null>("health_results", { category, offset, limit }),
  /** Correct the screenshot / screen-recording guess: "auto", "not" or "yes". Only Mori's view changes. */
  setCaptureOverride: (id: string, mode: "auto" | "not" | "yes") => invoke<void>("set_capture_override", { id, mode }),
  /** Read by the sandboxed worker. */
  fileMetadata: (id: string) => invoke<FileMeta>("file_metadata", { id }),
  metaStart: (locations: string[], recursive: boolean) => invoke<void>("meta_start", { locations, recursive }),
  metaPause: (paused: boolean) => invoke<void>("meta_pause", { paused }),
  metaCancel: () => invoke<void>("meta_cancel"),
  metaClear: () => invoke<void>("meta_clear"),
  metaResults: (category: MetaCategory | null, offset: number, limit: number) => invoke<MetaView | null>("meta_results", { category, offset, limit }),
  metaPlaces: () => invoke<Places | null>("meta_places"),
  /** New files next to the originals; originals are never modified. */
  sanitizeCopies: (ids: string[]) => invoke<SanitizeOutcome[]>("sanitize_copies", { ids }),
};

/**
 * Tell Rust a video is about to be loaded by the webview, so that if the
 * system media engine hangs or crashes the web content process, Rust knows
 * which file to block. Returns the function that ends the session.
 */
export async function beginMediaSession(id: string): Promise<() => void> {
  const token = await invoke<number>("video_session_start", { id });
  let ended = false;
  return () => {
    if (ended) return;
    ended = true;
    invoke("video_session_end", { token }).catch(() => {});
  };
}

/**
 * Liveness ping to Rust once a second for as long as the page is alive. If
 * the web content process wedges (pings stop while the window is in front),
 * Rust restarts it and blocks the video that was involved.
 */
export function startLivenessPing() {
  const ping = () => invoke("ui_alive").catch(() => {});
  ping();
  window.setInterval(ping, 1000);
}

// ------------------------------------------------------------- mori:// URLs

/** URL on Mori's own protocol. Only ids ever appear in it, never paths. */
const moriUrl = (route: string, id: string, iso = false) => convertFileSrc(`${iso ? "iso-" : ""}${route}/${id}`, "mori");

export const thumbUrl = (id: string) => moriUrl("thumb", id);
/**
 * A re-encoded, size-limited copy produced by the sandboxed worker. `iso`
 * marks an explicit isolated-view request (allowed in Safe Inspection Mode).
 */
export const previewUrl = (id: string, iso = false) => moriUrl("preview", id, iso);
/** Byte ranges of a video whose container was verified by magic bytes. */
export const mediaUrl = (id: string, iso = false) => moriUrl("media", id, iso);
/** A PDF page rasterised by the sandboxed worker. */
export const pdfPageUrl = (id: string, page: number, size: number, iso = false) => moriUrl("pdf", `${id}/${page}/${size}`, iso);
/** Byte ranges of an audio file whose format was verified by magic bytes. */
export const audioUrl = (id: string, iso = false) => moriUrl("audio", id, iso);
/** Sampled video frame `k`, re-encoded by the sandboxed worker. */
export const frameUrl = (id: string, k: number, iso = false) => moriUrl("frame", `${id}/${k}`, iso);

// ------------------------------------------------------------- thumbnails

const MAX_IN_FLIGHT = 4;
/** id -> loaded thumbnail URL, or null when none can be made. */
const cache = new Map<string, string | null>();
type Job = { id: string; run: () => Promise<string | null>; done: (v: string | null) => void };
const queue: Job[] = [];
let inFlight = 0;

function pump() {
  while (inFlight < MAX_IN_FLIGHT && queue.length) {
    // Newest requests first: those are the tiles currently on screen.
    const job = queue.pop()!;
    inFlight++;
    job
      .run()
      .catch(() => null)
      .then((url) => {
        inFlight--;
        pump();
        if (url === undefined) return; // handed over to the video capture queue
        cache.set(job.id, url);
        job.done(url);
      });
  }
}

function loadImage(src: string): Promise<boolean> {
  return new Promise((resolve) => {
    const img = new Image();
    img.onload = () => resolve(true);
    img.onerror = () => resolve(false);
    img.src = src;
  });
}

export function cachedThumb(e: Entry): string | null | undefined {
  return cache.get(e.id);
}

/**
 * Lazily fetch a thumbnail. Returns a cancel function so tiles scrolled out
 * of view before their turn cost nothing.
 */
export function requestThumb(e: Entry, onDone: (url: string | null) => void): () => void {
  const hit = cache.get(e.id);
  if (hit !== undefined) {
    onDone(hit);
    return () => {};
  }
  let cancelCapture = () => {};
  const job: Job = {
    id: e.id,
    done: onDone,
    run: async () => {
      const url = thumbUrl(e.id);
      if (await loadImage(url)) return url;
      if (e.kind !== "video") return null;
      // Video frames are captured separately (one at a time, never during a
      // preview) so they don't hold up image thumbnails.
      cancelCapture = enqueueCapture(e, async (ok) => {
        const again = `${url}?v=1`;
        const final = ok && (await loadImage(again)) ? again : null;
        cache.set(e.id, final);
        onDone(final);
      });
      return undefined as unknown as null;
    },
  };
  queue.push(job);
  pump();
  return () => {
    const i = queue.indexOf(job);
    if (i >= 0) queue.splice(i, 1);
    cancelCapture();
  };
}

// --------------------------------------------------- video frame capture

/** One webview decoding job: a thumbnail frame or similarity samples. */
type Capture = { run: (signal: AbortSignal) => Promise<boolean>; done: (ok: boolean) => void; urgent?: boolean };
const captures: Capture[] = [];
let capturing: AbortController | null = null;
let previewOpen = false;

/** While a preview is open no background video decoding happens at all. */
export function setPreviewOpen(open: boolean) {
  previewOpen = open;
  if (open) capturing?.abort();
  else pumpCaptures();
}

function enqueueCapture(e: Entry, done: (ok: boolean) => void, urgent = false): () => void {
  return enqueue({ run: (signal) => captureVideoThumb(e, signal), done }, urgent);
}

function enqueue(c: Capture, urgent: boolean): () => void {
  c.urgent = urgent;
  captures.push(c);
  pumpCaptures();
  return () => {
    const i = captures.indexOf(c);
    if (i >= 0) captures.splice(i, 1);
  };
}

function pumpCaptures() {
  if (capturing || previewOpen || !captures.length) return;
  // Newest first (tiles currently on screen), but analyzer requests before thumbnails.
  let at = captures.length - 1;
  for (let i = captures.length - 1; i >= 0; i--) {
    if (captures[i].urgent) {
      at = i;
      break;
    }
  }
  const job = captures.splice(at, 1)[0];
  const ctrl = new AbortController();
  capturing = ctrl;
  job
    .run(ctrl.signal)
    .catch(() => false)
    .then((ok) => {
      capturing = null;
      if (ctrl.signal.aborted && previewOpen) captures.push(job); // retry after the preview closes
      else job.done(ok);
      pumpCaptures();
    });
}

// ---------------------------------------------- similarity frame sampling

const SAMPLES = 12;
const SIDE = 64;

/**
 * Answer the analyzer's requests for sampled video frames. Frames are decoded
 * by the (sandboxed) system webview through the same guarded path as previews
 * (probe, blocklist, media session watchdog), reduced to 64×64 grayscale here,
 * and sent to Rust as raw pixels. One request at a time; paused during previews.
 */
export function startSimilarCaptureService(): () => void {
  const un = listen<{ token: number; id: string; times: number[] }>("similar-capture", ({ payload }) => {
    enqueue(
      {
        run: async (signal) => {
          const r = await sampleVideo(payload.id, payload.times, signal).catch(() => null);
          if (signal.aborted) return false; // retried after the preview closes
          const headers: Record<string, string> = { "mori-token": String(payload.token) };
          if (r) {
            Object.assign(headers, {
              "mori-width": String(r.width),
              "mori-height": String(r.height),
              "mori-duration": String(r.durationMs),
              "mori-container": r.container,
              "mori-codec": r.codec,
            });
          }
          await invoke("similar_frames", r ? r.planes : new Uint8Array(0), { headers }).catch(() => {});
          return true;
        },
        done: () => {},
      },
      true,
    );
  });
  return () => {
    un.then((f) => f());
  };
}

async function sampleVideo(id: string, times: number[], signal: AbortSignal) {
  const info = await api.inspect(id).catch(() => null);
  if (signal.aborted || info?.preview !== "video" || !info.video) return null;
  const end = await beginMediaSession(id).catch(() => null);
  if (!end) return null;
  try {
    const r = await grabSamples(mediaUrl(id), times, signal);
    return r && { ...r, container: info.video.container, codec: info.video.videoCodec ?? "" };
  } finally {
    end();
  }
}

/** Seek to each time and keep a 64×64 grayscale copy of the frame. */
function grabSamples(src: string, times: number[], signal: AbortSignal) {
  return new Promise<{ width: number; height: number; durationMs: number; planes: Uint8Array } | null>((resolve) => {
    const v = document.createElement("video");
    let finished = false;
    const finish = (r: { width: number; height: number; durationMs: number; planes: Uint8Array } | null) => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      signal.removeEventListener("abort", onAbort);
      v.pause();
      v.removeAttribute("src");
      v.load();
      v.remove();
      resolve(r);
    };
    const onAbort = () => finish(null);
    signal.addEventListener("abort", onAbort);
    const timer = setTimeout(() => finish(null), 30000 + times.length * 1500);
    const big = document.createElement("canvas");
    big.width = big.height = 256;
    const small = document.createElement("canvas");
    small.width = small.height = SIDE;
    const bctx = big.getContext("2d", { willReadFrequently: false })!;
    const sctx = small.getContext("2d", { willReadFrequently: true })!;
    bctx.imageSmoothingQuality = sctx.imageSmoothingQuality = "high";
    const nextFrame = () =>
      new Promise<void>((r) => {
        const rvfc = (v as HTMLVideoElement & { requestVideoFrameCallback?: (cb: () => void) => void }).requestVideoFrameCallback;
        let called = false;
        const once = () => {
          if (!called) {
            called = true;
            r();
          }
        };
        if (rvfc) rvfc.call(v, once);
        setTimeout(once, 400);
      });
    const seek = (t: number) =>
      new Promise<boolean>((r) => {
        const to = setTimeout(() => r(false), 5000);
        v.onseeked = () => {
          clearTimeout(to);
          r(true);
        };
        v.currentTime = t;
      });
    v.muted = true;
    v.playsInline = true;
    v.preload = "auto";
    v.crossOrigin = "anonymous";
    v.style.cssText = "position:fixed;left:-9999px;top:0;width:4px;height:4px;opacity:0;pointer-events:none";
    v.onerror = () => finish(null);
    v.onloadedmetadata = async () => {
      const d = v.duration;
      if (!isFinite(d) || d <= 0 || !v.videoWidth || !v.videoHeight) return finish(null);
      const at = times.length ? times : Array.from({ length: SAMPLES }, (_, k) => (d * (k + 0.5)) / SAMPLES);
      const planes = new Uint8Array(at.length * SIDE * SIDE);
      for (let k = 0; k < at.length; k++) {
        if (finished) return;
        if (!(await seek(Math.min(Math.max(at[k], 0), Math.max(0, d - 0.05))))) return finish(null);
        await nextFrame();
        try {
          bctx.clearRect(0, 0, 256, 256);
          bctx.drawImage(v, 0, 0, 256, 256);
          sctx.drawImage(big, 0, 0, SIDE, SIDE);
          const px = sctx.getImageData(0, 0, SIDE, SIDE).data;
          let painted = false;
          for (let i = 0; i < SIDE * SIDE; i++) {
            const a = px[i * 4 + 3];
            painted ||= a > 0;
            planes[k * SIDE * SIDE + i] = Math.round(0.299 * px[i * 4] + 0.587 * px[i * 4 + 1] + 0.114 * px[i * 4 + 2]);
          }
          if (!painted) return finish(null);
        } catch {
          return finish(null);
        }
      }
      finish({ width: v.videoWidth, height: v.videoHeight, durationMs: Math.round(d * 1000), planes });
    };
    document.body.appendChild(v);
    v.src = src;
  });
}

/**
 * Video thumbnails: only for files the sandboxed probe approved. The system
 * webview (itself sandboxed) decodes one frame; the pixels go back to Rust and
 * are re-encoded by the sandboxed worker before being cached.
 */
async function captureVideoThumb(e: Entry, signal: AbortSignal): Promise<boolean> {
  const info = await api.inspect(e.id).catch(() => null);
  if (signal.aborted || info?.preview !== "video" || info.previewsOff) return false;
  const end = await beginMediaSession(e.id).catch(() => null);
  if (!end) return false;
  let png: Uint8Array | undefined;
  try {
    png = await grabFrame(mediaUrl(e.id), signal);
  } finally {
    end();
  }
  if (!png || signal.aborted) return false;
  return invoke<boolean>("store_frame", png, { headers: { "mori-id": e.id } }).catch(() => false);
}

/**
 * Sample `count` evenly spaced frames of a video for the isolated view (and
 * the filmstrip). The webview decodes them; each is re-encoded by the
 * sandboxed worker before it can be displayed, and the page only ever shows
 * those copies. Calls `onFrame(k)` as each one is stored. Returns how many
 * frames were stored.
 */
export async function captureFrames(id: string, count: number, explicit: boolean, signal: AbortSignal, onFrame: (k: number) => void): Promise<number> {
  const end = await beginMediaSession(id).catch(() => null);
  if (!end) return 0;
  let stored = 0;
  try {
    await grabSequence(mediaUrl(id, explicit), count, signal, async (k, png) => {
      const headers: Record<string, string> = { "mori-id": id, "mori-frame": String(k) };
      if (explicit) headers["mori-explicit"] = "1";
      if (await invoke<boolean>("store_frame", png, { headers }).catch(() => false)) {
        stored++;
        onFrame(k);
      }
    });
  } finally {
    end();
  }
  return stored;
}

/** Seek through a hidden video element and hand over each frame as PNG (≤480 px). */
function grabSequence(src: string, count: number, signal: AbortSignal, onPng: (k: number, png: Uint8Array) => Promise<void>): Promise<void> {
  return new Promise((resolve) => {
    const v = document.createElement("video");
    let finished = false;
    const finish = () => {
      if (finished) return;
      finished = true;
      clearTimeout(timer);
      signal.removeEventListener("abort", finish);
      v.pause();
      v.removeAttribute("src");
      v.load();
      v.remove();
      resolve();
    };
    signal.addEventListener("abort", finish);
    const timer = setTimeout(finish, 20000 + count * 4000);
    const seek = (t: number) =>
      new Promise<boolean>((r) => {
        const to = setTimeout(() => r(false), 5000);
        v.onseeked = () => {
          clearTimeout(to);
          r(true);
        };
        v.currentTime = t;
      });
    const presented = () =>
      new Promise<void>((r) => {
        const rvfc = (v as HTMLVideoElement & { requestVideoFrameCallback?: (cb: () => void) => void }).requestVideoFrameCallback;
        let called = false;
        const once = () => {
          if (!called) {
            called = true;
            r();
          }
        };
        if (rvfc) rvfc.call(v, once);
        setTimeout(once, 500);
      });
    v.muted = true;
    v.playsInline = true;
    v.preload = "auto";
    v.crossOrigin = "anonymous";
    v.style.cssText = "position:fixed;left:-9999px;top:0;width:4px;height:4px;opacity:0;pointer-events:none";
    v.onerror = finish;
    v.onloadedmetadata = async () => {
      const d = v.duration;
      if (!isFinite(d) || d <= 0 || !v.videoWidth || !v.videoHeight) return finish();
      const scale = Math.min(1, 480 / Math.max(v.videoWidth, v.videoHeight));
      const c = document.createElement("canvas");
      c.width = Math.max(1, Math.round(v.videoWidth * scale));
      c.height = Math.max(1, Math.round(v.videoHeight * scale));
      const ctx = c.getContext("2d", { willReadFrequently: true })!;
      for (let k = 0; k < count; k++) {
        if (finished) return;
        if (!(await seek(Math.min((d * (k + 0.5)) / count, Math.max(0, d - 0.05))))) return finish();
        await presented();
        try {
          ctx.globalCompositeOperation = "source-over";
          ctx.clearRect(0, 0, c.width, c.height);
          ctx.drawImage(v, 0, 0, c.width, c.height);
          const px = ctx.getImageData(0, 0, c.width, c.height).data;
          let painted = false;
          for (let i = 3; i < px.length && !painted; i += 4 * 7) painted = px[i] > 0;
          if (!painted) continue;
          ctx.globalCompositeOperation = "destination-over";
          ctx.fillStyle = "#000";
          ctx.fillRect(0, 0, c.width, c.height);
          const blob = await new Promise<Blob | null>((r) => c.toBlob(r, "image/png"));
          if (blob && !finished) await onPng(k, new Uint8Array(await blob.arrayBuffer()));
        } catch {
          return finish();
        }
      }
      finish();
    };
    document.body.appendChild(v);
    v.src = src;
  });
}

/** Decode one frame in a detached, always-cleaned-up video element. */
function grabFrame(src: string, signal: AbortSignal): Promise<Uint8Array | undefined> {
  return new Promise((resolve) => {
    const v = document.createElement("video");
    let done = false;
    const finish = (png: Uint8Array | undefined) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      signal.removeEventListener("abort", onAbort);
      v.pause();
      v.removeAttribute("src");
      v.load(); // releases the decoder and any pending network work
      v.remove();
      resolve(png);
    };
    const onAbort = () => finish(undefined);
    signal.addEventListener("abort", onAbort);
    const timer = setTimeout(() => finish(undefined), 8000);
    const draw = () => {
      if (done) return;
      try {
        const scale = Math.min(1, 512 / Math.max(v.videoWidth, v.videoHeight));
        const c = document.createElement("canvas");
        c.width = Math.max(1, Math.round(v.videoWidth * scale));
        c.height = Math.max(1, Math.round(v.videoHeight * scale));
        const ctx = c.getContext("2d")!;
        ctx.drawImage(v, 0, 0, c.width, c.height);
        // WebKit may leave the canvas untouched (fully transparent) if no frame was presented yet.
        const px = ctx.getImageData(0, 0, c.width, c.height).data;
        let painted = false;
        for (let i = 3; i < px.length && !painted; i += 4 * 7) painted = px[i] > 0;
        if (!painted) return finish(undefined);
        ctx.globalCompositeOperation = "destination-over";
        ctx.fillStyle = "#000";
        ctx.fillRect(0, 0, c.width, c.height);
        c.toBlob(async (b) => finish(b ? new Uint8Array(await b.arrayBuffer()) : undefined), "image/png");
      } catch {
        finish(undefined);
      }
    };
    v.muted = true;
    v.playsInline = true;
    v.preload = "auto";
    v.crossOrigin = "anonymous";
    // Must be in the document for WebKit to present (and let us read) frames.
    v.style.cssText = "position:fixed;left:-9999px;top:0;width:4px;height:4px;opacity:0;pointer-events:none";
    v.onloadedmetadata = () => {
      v.currentTime = Math.min(1, (v.duration || 0) * 0.1);
    };
    v.onseeked = () => {
      const rvfc = (v as HTMLVideoElement & { requestVideoFrameCallback?: (cb: () => void) => void }).requestVideoFrameCallback;
      if (rvfc) {
        rvfc.call(v, draw);
        setTimeout(draw, 600); // in case no new frame callback arrives for a paused video
      } else setTimeout(draw, 150);
    };
    v.onerror = () => finish(undefined);
    document.body.appendChild(v);
    v.src = src;
  });
}

/** Frames sampled for filmstrips, hover scrub and the isolated view. */
export const SCRUB_FRAMES = 8;
const scrubRequested = new Set<string>();

/**
 * Hover scrub: ask (once) for a video's sampled frames in the background,
 * through the same queue as video thumbnails (never during a preview). The
 * backend refuses them in Safe Inspection Mode.
 */
export function requestScrubFrames(e: Entry, onReady: () => void) {
  if (scrubRequested.has(e.id)) return;
  scrubRequested.add(e.id);
  enqueue(
    {
      run: async (signal) => {
        const info = await api.inspect(e.id).catch(() => null);
        if (signal.aborted || info?.preview !== "video" || info.previewsOff) return false;
        return (await captureFrames(e.id, SCRUB_FRAMES, false, signal, () => {})) > 0;
      },
      done: (ok) => ok && onReady(),
    },
    false,
  );
}

export function resetThumbs() {
  cache.clear();
}

// ------------------------------------------------------------ formatting

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = bytes / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`;
}

const dateFmt = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });
const shortDateFmt = new Intl.DateTimeFormat(undefined, { dateStyle: "medium" });
export const formatDate = (ms: number | null) => (ms ? dateFmt.format(ms) : "—");
export const formatShortDate = (ms: number | null) => (ms ? shortDateFmt.format(ms) : "—");

export const KIND_LABEL: Record<Kind, string> = {
  link: "Link",
  folder: "Folder",
  photo: "Photo",
  video: "Video",
  gif: "GIF",
  document: "Document",
  audio: "Audio",
  other: "File",
};

export function typeLabel(e: Entry): string {
  if (e.kind === "folder") return "Folder";
  if (e.kind === "link") return "Symbolic link";
  const noun = { photo: "image", gif: "image", video: "video", audio: "audio", document: "document", other: "file" }[e.kind];
  return e.ext ? `${e.ext.toUpperCase()} ${noun}` : KIND_LABEL[e.kind];
}

export const parentOf = (p: string) => (p.includes("/") ? p.slice(0, p.lastIndexOf("/")) : "");

export const isMac = navigator.userAgent.includes("Mac");

/** Tooltip for the private-folder indicator. */
export const PRIVATE_HINT = "Private: contents are hidden from global and recursive views.";

export const plural = (n: number, one: string, many = `${one}s`) => `${n.toLocaleString()} ${n === 1 ? one : many}`;

export const formatDuration = (ms: number | null) => {
  if (ms === null) return "—";
  const s = Math.round(ms / 1000);
  return s >= 3600 ? `${Math.floor(s / 3600)}:${String(Math.floor((s % 3600) / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}` : `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
};

/** Display entry for an analyzer file (the preview and thumbnails work by id). */
export const dupEntry = (f: DupFile): Entry => ({ id: f.id, name: f.name, path: f.path, ext: f.ext, kind: f.kind, size: f.size, modified: f.modified, created: f.created });
