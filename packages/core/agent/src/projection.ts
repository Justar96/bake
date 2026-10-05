import type { TurnBoundaryProjection } from './types.ts'
import type {} from 'bake-session-projection'

declare module 'bake-session-projection/types' {
  interface SessionProjectionStateMap {
    /** The agent session's open/last turn and step boundary facts (whole value). */
    turnBoundary: TurnBoundaryProjection
  }
}

export {}
