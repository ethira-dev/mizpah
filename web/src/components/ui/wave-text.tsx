import type { CSSProperties } from "react"

import { cn } from "@/lib/utils"

type WaveTextProps = {
  children: string
  className?: string
  /** Vertical bob amplitude in pixels. */
  amplitude?: number
  /** Full wave cycle duration in seconds. */
  duration?: number
  /** Delay between adjacent characters in seconds. */
  stagger?: number
}

/** Per-character vertical wave for loading / status subtext. */
export function WaveText({
  children,
  className,
  amplitude = 3,
  duration = 1.2,
  stagger = 0.05,
}: WaveTextProps) {
  const chars = Array.from(children)

  return (
    <span
      className={cn("inline-flex flex-wrap whitespace-pre", className)}
      aria-label={children}
    >
      {chars.map((char, i) => (
        <span
          key={`${i}-${char}`}
          aria-hidden="true"
          className="inline-block motion-safe:animate-[wave-text-bob_var(--wave-duration)_ease-in-out_infinite]"
          style={
            {
              "--wave-duration": `${duration}s`,
              "--wave-amplitude": `${amplitude}px`,
              animationDelay: `${i * stagger}s`,
            } as CSSProperties
          }
        >
          {char === " " ? "\u00a0" : char}
        </span>
      ))}
    </span>
  )
}
