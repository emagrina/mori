import type { Kind } from "../api";

/** Monochrome 24px line icons. Colour always comes from CSS (`currentColor`). */
const PATHS = {
  search: "M10.5 18a7.5 7.5 0 1 1 0-15 7.5 7.5 0 0 1 0 15Zm5.3-2.2L21 21",
  gallery: "M3.5 4.5h7v6.5h-7zM13.5 4.5h7v6.5h-7zM3.5 13.5h7V20h-7zM13.5 13.5h7V20h-7z",
  grid: "M4 4h4.5v4.5H4zM9.75 4h4.5v4.5h-4.5zM15.5 4H20v4.5h-4.5zM4 9.75h4.5v4.5H4zM9.75 9.75h4.5v4.5h-4.5zM15.5 9.75H20v4.5h-4.5zM4 15.5h4.5V20H4zM9.75 15.5h4.5V20h-4.5zM15.5 15.5H20V20h-4.5z",
  list: "M9 6.5h11M9 12h11M9 17.5h11M4.5 6.5h.01M4.5 12h.01M4.5 17.5h.01",
  folder: "M3.5 7.5a2 2 0 0 1 2-2h3.8l2 2.1H18.5a2 2 0 0 1 2 2v7.9a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2Z",
  photo: "M5 4.5h14A1.5 1.5 0 0 1 20.5 6v12a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 18V6A1.5 1.5 0 0 1 5 4.5Zm-1.5 11.5 4.5-4.5 4 4 3-3 5.5 5.5M15.5 9.25h.01",
  video: "M4.5 6h10A1.5 1.5 0 0 1 16 7.5v9a1.5 1.5 0 0 1-1.5 1.5h-10A1.5 1.5 0 0 1 3 16.5v-9A1.5 1.5 0 0 1 4.5 6Zm11.5 4.25 5-2.75v9l-5-2.75",
  gif: "M5 4.5h14A1.5 1.5 0 0 1 20.5 6v12a1.5 1.5 0 0 1-1.5 1.5H5A1.5 1.5 0 0 1 3.5 18V6A1.5 1.5 0 0 1 5 4.5Zm5.5 5h-2A1.5 1.5 0 0 0 7 11v2a1.5 1.5 0 0 0 1.5 1.5h2V12H9M13 9.5v5M16 14.5v-5h2.5M16 12h2",
  document: "M6.5 3.5h7.5l4.5 4.5v11a1.5 1.5 0 0 1-1.5 1.5h-10.5A1.5 1.5 0 0 1 5 19V5a1.5 1.5 0 0 1 1.5-1.5Zm7.5 0V8h4.5M8.5 12.5h7M8.5 16h7",
  audio: "M9 17.5V6.5l10.5-2v11M9 17.5a2.75 2.75 0 1 1-5.5 0 2.75 2.75 0 0 1 5.5 0Zm10.5-2a2.75 2.75 0 1 1-5.5 0 2.75 2.75 0 0 1 5.5 0Z",
  other: "M6.5 3.5h7.5l4.5 4.5v11a1.5 1.5 0 0 1-1.5 1.5h-10.5A1.5 1.5 0 0 1 5 19V5a1.5 1.5 0 0 1 1.5-1.5Zm7.5 0V8h4.5",
  all: "M4.5 4.5h6v6h-6zM13.5 4.5h6v6h-6zM4.5 13.5h6v6h-6zM13.5 13.5h6v6h-6z",
  drive: "M3.5 13.5 5.8 6.4A2 2 0 0 1 7.7 5h8.6a2 2 0 0 1 1.9 1.4l2.3 7.1M3.5 13.5V17a1.5 1.5 0 0 0 1.5 1.5h14a1.5 1.5 0 0 0 1.5-1.5v-3.5M3.5 13.5h17M16.5 16h.01",
  left: "M14.5 5.5 8 12l6.5 6.5",
  right: "M9.5 5.5 16 12l-6.5 6.5",
  close: "M6.5 6.5l11 11M17.5 6.5l-11 11",
  external: "M14 4.5h5.5V10M19.5 4.5 11 13M18 14v4.5a1 1 0 0 1-1 1H5.5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1H10",
  reveal: "M3.5 7.5a2 2 0 0 1 2-2h3.8l2 2.1H18.5a2 2 0 0 1 2 2v7.9a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2ZM12 11v5M9.5 13.5 12 16l2.5-2.5",
  copy: "M9.5 9h9a1 1 0 0 1 1 1v9.5a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1V10a1 1 0 0 1 1-1Zm-3 6H5.5a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1h8.5a1 1 0 0 1 1 1v1",
  refresh: "M19.5 11a7.5 7.5 0 0 0-13.7-4.2L4.5 8.5M4.5 4.5v4h4M4.5 13a7.5 7.5 0 0 0 13.7 4.2l1.3-1.7M19.5 19.5v-4h-4",
  up: "M12 18.5v-13M6.5 11 12 5.5 17.5 11",
  down: "M12 5.5v13M6.5 13 12 18.5 17.5 13",
  zoomIn: "M10.5 18a7.5 7.5 0 1 1 0-15 7.5 7.5 0 0 1 0 15Zm5.3-2.2L21 21M10.5 7.75v5.5M7.75 10.5h5.5",
  zoomOut: "M10.5 18a7.5 7.5 0 1 1 0-15 7.5 7.5 0 0 1 0 15Zm5.3-2.2L21 21M7.75 10.5h5.5",
  chevron: "M9.5 6.5 15 12l-5.5 5.5",
  chevronDown: "M7 10l5 5 5-5",
  more: "M5.5 12h.01M12 12h.01M18.5 12h.01",
  check: "M5.5 12.5l4 4 9-9",
  trash: "M4.5 6.5h15M9.5 6.5V4.75a1 1 0 0 1 1-1h3a1 1 0 0 1 1 1V6.5M6.5 6.5l.8 12.1a1.5 1.5 0 0 0 1.5 1.4h6.4a1.5 1.5 0 0 0 1.5-1.4l.8-12.1M10 10.5v6M14 10.5v6",
  rename: "M14.5 5.5l4 4M4.5 19.5l1-4.5L15.8 4.7a1.4 1.4 0 0 1 2 0l1.5 1.5a1.4 1.4 0 0 1 0 2L9 18.5Z",
  duplicate: "M8.5 8.5h10a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1h-10a1 1 0 0 1-1-1v-10a1 1 0 0 1 1-1Zm-3 7h-1a1 1 0 0 1-1-1v-10a1 1 0 0 1 1-1h10a1 1 0 0 1 1 1v1",
  lock: "M7 11V8a5 5 0 0 1 10 0v3M6.5 11h11a1 1 0 0 1 1 1v7a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1Z",
  live: "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6ZM12 18.5a6.5 6.5 0 1 0 0-13 6.5 6.5 0 0 0 0 13ZM12 21.5v.01M12 2.5v.01M21.5 12h.01M2.5 12h.01M18.7 18.7h.01M5.3 5.3h.01M18.7 5.3h.01M5.3 18.7h.01",
  keep: "M12 3.5l7 3v5c0 4.3-2.9 7.8-7 9-4.1-1.2-7-4.7-7-9v-5Zm-3 8.5 2.2 2.2 4.3-4.4",
  link: "M10 14a4.5 4.5 0 0 0 6.4 0l3-3a4.5 4.5 0 0 0-6.4-6.4l-1.2 1.2M14 10a4.5 4.5 0 0 0-6.4 0l-3 3a4.5 4.5 0 0 0 6.4 6.4l1.2-1.2",
  shield: "M12 3.5l7 3v5c0 4.3-2.9 7.8-7 9-4.1-1.2-7-4.7-7-9v-5Z",
  info: "M12 21a9 9 0 1 1 0-18 9 9 0 0 1 0 18Zm0-11v6M12 7.5h.01",
  warning: "M10.3 4.6 3.2 17a2 2 0 0 0 1.7 3h14.2a2 2 0 0 0 1.7-3L13.7 4.6a2 2 0 0 0-3.4 0ZM12 9.5v4M12 16.8h.01",
    sliders: "M4 7h10M18 7h2M4 17h4M12 17h8M14 4v6M8 14v6",
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({ name, size = 16, className, stroke = 1.6 }: { name: IconName; size?: number; className?: string; stroke?: number }) {
  // The dotted "more" glyph needs a heavier stroke to read as dots.
  const width = name === "more" ? 2.6 : stroke;
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={width}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d={PATHS[name]} />
    </svg>
  );
}

export const kindIcon = (k: Kind): IconName => k;

/** Two-tone graphite folder used on folder cards (colours from CSS tokens). */
export function FolderGlyph({ size = 64 }: { size?: number }) {
  return (
    <svg className="folder-glyph" width={size} height={size * 0.78} viewBox="0 0 64 50" aria-hidden>
      <path className="back" d="M4 6a4 4 0 0 1 4-4h14.5a4 4 0 0 1 2.9 1.25L29 7h27a4 4 0 0 1 4 4v3H4Z" />
      <rect className="front" x="2" y="11" width="60" height="37" rx="4.5" />
    </svg>
  );
}

/** Mori's folder-spy mark: dark folder on light backgrounds, light folder on dark. */
export function Logo({ size = 22, className }: { size?: number; className?: string }) {
  return (
    <picture className={className}>
      <source srcSet="/mori-icon-white.png" media="(prefers-color-scheme: dark)" />
      <img src="/mori-icon-black.png" width={size} height={size} alt="" draggable={false} />
    </picture>
  );
}
