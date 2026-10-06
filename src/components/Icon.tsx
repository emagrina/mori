import type { Kind } from "../api";

const PATHS = {
  search: "M10.5 18a7.5 7.5 0 1 1 0-15 7.5 7.5 0 0 1 0 15Zm5.3-2.2L21 21",
  gallery: "M3 4h8v7H3zM13 4h8v7h-8zM3 13h8v7H3zM13 13h8v7h-8z",
  grid: "M3 3h5v5H3zM9.5 3h5v5h-5zM16 3h5v5h-5zM3 9.5h5v5H3zM9.5 9.5h5v5h-5zM16 9.5h5v5h-5zM3 16h5v5H3zM9.5 16h5v5h-5zM16 16h5v5h-5z",
  list: "M8 6h13M8 12h13M8 18h13M3.5 6h.01M3.5 12h.01M3.5 18h.01",
  folder: "M3 7a2 2 0 0 1 2-2h4.2l2 2.2H19a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z",
  photo: "M4 5h16a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1Zm-1 11 5-5 4 4 3-3 6 6M15.5 9.5h.01",
  video: "M4 6h11a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1Zm12 4.5 5-3v9l-5-3",
  gif: "M4 5h16a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1Zm6.5 4.5h-2a1.5 1.5 0 0 0-1.5 1.5v2a1.5 1.5 0 0 0 1.5 1.5h2V12h-1.5M13 9.5v5M16 14.5v-5h2.5M16 12h2",
  document: "M6 3h8l5 5v12a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1Zm8 0v5h5M8.5 13h7M8.5 16.5h7",
  audio: "M9 18V6l11-2v12M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0Zm11-2a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z",
  other: "M6 3h8l5 5v12a1 1 0 0 1-1 1H6a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1Zm8 0v5h5",
  all: "M4 4h6v6H4zM14 4h6v6h-6zM4 14h6v6H4zM14 14h6v6h-6z",
  drive: "M3 13.5 5.5 6a2 2 0 0 1 1.9-1.4h9.2A2 2 0 0 1 18.5 6L21 13.5M3 13.5V18a1 1 0 0 0 1 1h16a1 1 0 0 0 1-1v-4.5M3 13.5h18M17 16.3h.01",
  left: "M15 5l-7 7 7 7",
  right: "M9 5l7 7-7 7",
  close: "M6 6l12 12M18 6 6 18",
  external: "M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5",
  reveal: "M3 7a2 2 0 0 1 2-2h4.2l2 2.2H19a2 2 0 0 1 2 2V17a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2ZM12 11v5M9.5 13.5 12 16l2.5-2.5",
  copy: "M9 9h10a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H9a1 1 0 0 1-1-1V10a1 1 0 0 1 1-1Zm-3 6H5a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1h9a1 1 0 0 1 1 1v1",
  refresh: "M20 11a8 8 0 0 0-14.6-4.5L4 8M4 4v4h4M4 13a8 8 0 0 0 14.6 4.5L20 16M20 20v-4h-4",
  up: "M12 19V5M5 12l7-7 7 7",
  down: "M12 5v14M5 12l7 7 7-7",
  zoomIn: "M10.5 18a7.5 7.5 0 1 1 0-15 7.5 7.5 0 0 1 0 15Zm5.3-2.2L21 21M10.5 7.5v6M7.5 10.5h6",
  zoomOut: "M10.5 18a7.5 7.5 0 1 1 0-15 7.5 7.5 0 0 1 0 15Zm5.3-2.2L21 21M7.5 10.5h6",
  chevron: "M9 6l6 6-6 6",
  sliders: "M4 7h10M18 7h2M4 17h4M12 17h8M14 4v6M8 14v6",
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({ name, size = 16, className }: { name: IconName; size?: number; className?: string }) {
  return (
    <svg
      className={className}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.7}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d={PATHS[name]} />
    </svg>
  );
}

export const kindIcon = (k: Kind): IconName => k;

/** Mori's folder-spy mark: black folder on light backgrounds, white on dark. */
export function Logo({ size = 22 }: { size?: number }) {
  return (
    <picture>
      <source srcSet="/mori-icon-white.png" media="(prefers-color-scheme: dark)" />
      <img src="/mori-icon-black.png" width={size} height={size} alt="" draggable={false} />
    </picture>
  );
}
