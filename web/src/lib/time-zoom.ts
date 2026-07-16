/** Discrete zoom steps from 1-minute tiles (1h window) to 1-day tiles (30d window). */
export type TimeZoomLevel = {
  /** Trailing window length in hours. */
  windowHours: number
  /** Duration each tile represents, in minutes. */
  bucketMinutes: number
  /** Short label for the strip chrome. */
  windowLabel: string
  /** Short label for tile size. */
  bucketLabel: string
}

export const TIME_ZOOM_LEVELS: readonly TimeZoomLevel[] = [
  {
    windowHours: 1,
    bucketMinutes: 1,
    windowLabel: "Last 1 hour",
    bucketLabel: "1 min",
  },
  {
    windowHours: 3,
    bucketMinutes: 2,
    windowLabel: "Last 3 hours",
    bucketLabel: "2 min",
  },
  {
    windowHours: 6,
    bucketMinutes: 5,
    windowLabel: "Last 6 hours",
    bucketLabel: "5 min",
  },
  {
    windowHours: 12,
    bucketMinutes: 10,
    windowLabel: "Last 12 hours",
    bucketLabel: "10 min",
  },
  {
    windowHours: 24,
    bucketMinutes: 25,
    windowLabel: "Last 24 hours",
    bucketLabel: "25 min",
  },
  {
    windowHours: 72,
    bucketMinutes: 60,
    windowLabel: "Last 3 days",
    bucketLabel: "1 hour",
  },
  {
    windowHours: 168,
    bucketMinutes: 180,
    windowLabel: "Last 7 days",
    bucketLabel: "3 hours",
  },
  {
    windowHours: 360,
    bucketMinutes: 720,
    windowLabel: "Last 15 days",
    bucketLabel: "12 hours",
  },
  {
    windowHours: 720,
    bucketMinutes: 1440,
    windowLabel: "Last 30 days",
    bucketLabel: "1 day",
  },
] as const

/** Default: 24 hours · 25 min tiles. */
export const DEFAULT_TIME_ZOOM_INDEX = Math.max(
  0,
  TIME_ZOOM_LEVELS.findIndex(
    (l) => l.windowHours === 24 && l.bucketMinutes === 25
  )
)

export function clampTimeZoomIndex(index: number): number {
  return Math.max(0, Math.min(TIME_ZOOM_LEVELS.length - 1, index))
}

export function timeZoomAt(index: number): TimeZoomLevel {
  return TIME_ZOOM_LEVELS[clampTimeZoomIndex(index)]!
}

/** Modifier key for zoom-scroll: ⌘ on Apple, Ctrl elsewhere. */
export function isZoomScrollModifier(e: {
  metaKey: boolean
  ctrlKey: boolean
}): boolean {
  return e.metaKey || e.ctrlKey
}
