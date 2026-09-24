/** The opening block a fresh session prints above its first prompt. */
import React from 'react'
import { Box, Text } from 'ink'
import stringWidth from 'string-width'
import type { TuiCopy } from './copy.ts'
import { FRAME_MIN_COLUMNS, type FrameStyle } from './layout.ts'
import { PALETTE } from './palette.ts'

/** Widest the block draws, so it reads as a card rather than a rule across a wide terminal. */
export const WELCOME_WIDTH = 64

/** Commands the block offers as a start; each is registered by this surface in every profile. */
const EXAMPLES = [
  { name: '/help', description: 'welcomeHelp' },
  { name: '/changelog', description: 'welcomeChangelog' },
] as const satisfies readonly { readonly name: string, readonly description: keyof TuiCopy }[]

/**
 * Product name and version, the session line it carries, and example commands.
 *
 * Printed once, as the first committed item of a session with no history, so
 * it scrolls away with the transcript like any other row and never costs the
 * dynamic region a row. It prints the session line itself, in the words the
 * heading uses when it prints alone, so a fresh session and a resumed one name
 * their id alike while only the fresh one is framed. The border follows the
 * composer's frame style and is dropped on the same narrow terminals, so a
 * terminal that cannot draw the rounded frame never sees it here either. It
 * adds no row of its own after the card: the chrome's gap is what separates
 * printed history from the input, and the session line inside the card gives
 * the block the row the heading would have taken. Box margins and columns own
 * spacing, not padded text. Every row truncates rather than wraps, keeping the
 * block's height fixed.
 *
 * @param props.version - the running Bake version, without a leading `v`.
 * @param props.heading - the session line, already localized and labeled.
 * @param props.copy - locale-owned labels.
 * @param props.frame - border style this terminal can draw.
 * @param props.columns - terminal width.
 * @returns the block.
 */
export function Welcome({ version, heading, copy, frame, columns }: {
  readonly version: string
  readonly heading: string
  readonly copy: TuiCopy
  readonly frame: FrameStyle
  readonly columns: number
}): React.ReactElement {
  const bordered = columns >= FRAME_MIN_COLUMNS
  const width = Math.max(1, Math.min(columns, WELCOME_WIDTH))
  const contentWidth = width - (bordered ? 4 : 0)
  const nameWidth = Math.min(contentWidth, Math.max(...EXAMPLES.map(example => stringWidth(example.name))))
  const descriptionWidth = Math.max(0, contentWidth - nameWidth - 2)
  return (
    <Box
      flexDirection="column" flexShrink={0} width={width}
      {...bordered ? { borderStyle: frame, borderDimColor: true, paddingX: 1 } : {}}
    >
      <Box flexDirection="column" flexShrink={0}>
        <Box columnGap={2} flexShrink={0}>
          <Box width={Math.min(4, contentWidth)} flexShrink={0}>
            <Text bold color={PALETTE.running} wrap="truncate-end">BAKE</Text>
          </Box>
          {contentWidth > 6 && <Box flexGrow={1} minWidth={0}>
            <Text dimColor wrap="truncate-end">v{version}</Text>
          </Box>}
        </Box>
        <Text dimColor wrap="truncate-end">{heading}</Text>
      </Box>
      <Box flexDirection="column" marginTop={1} flexShrink={0}>
        {EXAMPLES.map(example => (
          <Box key={example.name} columnGap={2} flexShrink={0}>
            <Box width={nameWidth} flexShrink={0}>
              <Text color={PALETTE.asking} wrap="truncate-end">{example.name}</Text>
            </Box>
            {descriptionWidth > 0 && <Box width={descriptionWidth} minWidth={0}>
              <Text dimColor wrap="truncate-end">{copy[example.description]}</Text>
            </Box>}
          </Box>
        ))}
      </Box>
    </Box>
  )
}
