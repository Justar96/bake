/**
 * Model-facing literal edit, unique-match by default, with one or several replacements per call.
 * It obtains an optional guard from the single intent slot, calls `ctx.fs.editText` without a
 * separate stat, then records the observed version; no policy means an unconditional atomic edit.
 * When an anchored edit lands on content the model had not seen, the result shows the edited lines.
 * @module bake-tool-fs/src/edit
 */

import type { Context } from '@deepseek-ai/cordis'
import { defineTool } from 'bake-tools'
import type { DiffCallView, DiffResultView, ToolResult } from 'bake-tools'
import type { FsEditBasis, FsEditRequest } from 'bake-fs'
import { computeHunkDiffs, diffsFromMeta } from './diff.ts'
import { remediateFsError } from './error.ts'
import { sessionResolveOptions } from './session-cwd.ts'
import type { FsSandboxController } from './sandbox.ts'

/** Validated `edit` arguments after defaulting. */
interface EditInput {
  filePath: string
  edits: FsEditRequest[]
}

/** One raw replacement, either top-level or an `edits` entry. */
interface RawReplacement {
  old_string: string
  new_string: string
  replace_all?: boolean
}

/**
 * The `edit` tool's raw arguments: a single top-level replacement or an
 * `edits` list, plus the two escalation fields advertised only under a
 * confining `ctx.fs` (absent from the schema otherwise, so the validator
 * rejects them before `execute`).
 */
interface EditToolArgs {
  file_path: string
  old_string?: string
  new_string?: string
  replace_all?: boolean
  edits?: RawReplacement[]
  sandbox_permissions?: string
  justification?: string
}

/** Most edited lines echoed back when the model had not seen the file's current content. */
export const EDITED_LINES_LIMIT = 40

/** Whether a raw `edits` entry has the replacement shape; replayed logged arguments are not schema-checked. */
function isReplacement(edit: unknown): edit is RawReplacement {
  if (typeof edit !== 'object' || edit === null) return false
  const { old_string: oldString, new_string: newString } = edit as Record<string, unknown>
  return typeof oldString === 'string' && typeof newString === 'string'
}

/** The requested replacements without validation, for rendering and replayed `presentCall` arguments. */
function rawReplacements(args: EditToolArgs): RawReplacement[] {
  return requestedReplacements(args).map(({ edit }) => edit)
}

/** Whether a replacement is an all-empty placeholder rather than a request. */
function isPlaceholder(edit: RawReplacement): boolean {
  return edit.old_string === '' && edit.new_string === ''
}

/**
 * Collect the requested replacements from either argument form. Some models
 * fill every optional field, sending `edits: []` beside a real pair, or an
 * empty pair beside real `edits`; all-empty placeholders are ignored instead of
 * refused, since a refusal only costs another request. When both forms carry
 * a real replacement they apply together, and the pair is dropped if it
 * repeats an `edits` entry.
 * @param args - the schema-validated raw tool arguments.
 * @returns the replacements, each labelled for diagnostics.
 */
function requestedReplacements(args: EditToolArgs): { edit: RawReplacement; label: string }[] {
  const listed = (Array.isArray(args.edits) ? args.edits : [])
    .map((edit, index) => ({ edit, label: `edits[${index}]` }))
    .filter((entry): entry is { edit: RawReplacement; label: string } => isReplacement(entry.edit) && !isPlaceholder(entry.edit))
  const pair = args.old_string === undefined && args.new_string === undefined
    ? undefined
    : { old_string: args.old_string ?? '', new_string: args.new_string ?? '', ...args.replace_all === undefined ? {} : { replace_all: args.replace_all } }
  if (pair === undefined || isPlaceholder(pair)) return listed
  const repeated = listed.some(({ edit }) => edit.old_string === pair.old_string && edit.new_string === pair.new_string)
  return repeated ? listed : [{ edit: pair, label: '' }, ...listed]
}

/**
 * Validate value constraints the schema DSL can't express: a non-blank
 * `file_path`, at least one replacement, a non-empty `old_string` per
 * replacement, and `old_string !== new_string` (an equal pair would be a
 * guaranteed no-op edit).
 * @param args - the schema-validated raw tool arguments.
 * @returns the camelCased input with each `replace_all` defaulted to false.
 */
export function parseEditArgs(args: EditToolArgs): EditInput {
  if (args.file_path.trim().length === 0) throw new Error('file_path must be a non-empty string')
  const replacements = requestedReplacements(args)
  if (replacements.length === 0) throw new Error('give old_string and new_string, or a non-empty edits list')
  const field = (label: string, name: string): string => label === '' ? name : `${label}.${name}`
  return {
    filePath: args.file_path,
    edits: replacements.map(({ edit, label }) => {
      if (edit.old_string.length === 0) throw new Error(`${field(label, 'old_string')} must be a non-empty string`)
      if (edit.old_string === edit.new_string) throw new Error(`${field(label, 'old_string')} and ${field(label, 'new_string')} must differ`)
      return { oldString: edit.old_string, newString: edit.new_string, replaceAll: edit.replace_all ?? false }
    }),
  }
}

/**
 * Number the lines each applied hunk introduced, capped at {@link EDITED_LINES_LIMIT}.
 * @param before - the LF-normalized content before the edit.
 * @param after - the LF-normalized content after the edit.
 * @returns `N: text` lines in file order, with a trailing count of omitted lines.
 */
function editedLines(before: string, after: string): string {
  const numbered = computeHunkDiffs('', before, after)
    .flatMap(hunk => hunk.newText.split('\n').map((text, index) => `${hunk.newStart + index}: ${text}`))
  const shown = numbered.slice(0, EDITED_LINES_LIMIT)
  const omitted = numbered.length - shown.length
  return omitted > 0 ? `${shown.join('\n')}\n(${omitted} more edited lines not shown)` : shown.join('\n')
}

/**
 * Format an edit success as a Claude-style model-facing message. When the model
 * had not seen the content it edited, the edited lines follow, so its view of
 * the file is current without another `read`.
 * @param displayPath - the backend-resolved path shown to the model.
 * @param replaceAll - selects the all-occurrences wording over the single-replacement one.
 * @param detail - the edit count, anchored basis, and before/after text; omitted by callers that only need the sentence.
 * @returns the confirmation text the model sees as the tool result.
 */
export function formatEditOutput(
  displayPath: string,
  replaceAll: boolean,
  detail?: { edits: number; basis: FsEditBasis | undefined; before: string; after: string },
): string {
  const edits = detail?.edits ?? 1
  const sentence = replaceAll
    ? `The file ${displayPath} has been updated. All occurrences were successfully replaced.`
    : edits > 1
      ? `The file ${displayPath} has been updated successfully (${edits} edits).`
      : `The file ${displayPath} has been updated successfully.`
  if (detail === undefined || detail.basis === undefined || detail.basis === 'observed') return sentence
  const reason = detail.basis === 'changed'
    ? 'It had changed since you read it; those changes were kept.'
    : 'You had not read it.'
  return `${sentence} ${reason} The edited lines now read:\n${editedLines(detail.before, detail.after)}`
}

/**
 * Register the `edit` tool. Its description and parameters carry all of its
 * model-facing guidance; the tool contributes no system-prompt section.
 * @param ctx - the plugin context; registrations are effects scoped to it, and execution uses its `fs` service.
 * @param sandbox - the shared sandbox-escalation API (advertisement, mode stamping, denial mapping).
 */
export function applyEditTool(ctx: Context, sandbox: FsSandboxController): void {
  ctx.tools.register(defineTool({
    name: 'edit',
    // Recovery guidance lives in the error text, paid only on failure; the
    // description is resent with every request. Under the shipped
    // content-anchored policy the exact unique match is the precondition, so
    // the file need not have been read with `read`.
    description: 'Replace exact text in a UTF-8 file; each old_string must match once unless replace_all. '
      + 'Batch one file\'s changes in `edits`, each matched against the original.',
    parameters: {
      file_path: { type: 'string', required: true, description: 'Absolute, or relative to the working directory.' },
      old_string: {
        type: 'string',
        description: 'Exact, with whitespace; no read line numbers.',
      },
      new_string: { type: 'string' },
      replace_all: { type: 'boolean' },
      edits: {
        type: 'array',
        description: 'Instead of old_string/new_string.',
        items: {
          type: 'object',
          additionalProperties: false,
          properties: {
            old_string: { type: 'string', required: true },
            new_string: { type: 'string', required: true },
            replace_all: { type: 'boolean' },
          },
        },
      },
      ...sandbox.escalationModes.length > 0 ? sandbox.schemaFields() : {},
    },
    output: {
      schema: {
        type: 'object',
        additionalProperties: false,
        properties: {
          path: { type: 'string', required: true },
          before: { type: 'string', required: true },
          after: { type: 'string', required: true },
          basis: { type: 'string', enum: ['observed', 'changed', 'unobserved'] },
        },
      },
      render: (args: EditToolArgs, value) => {
        const edits = rawReplacements(args)
        return [{
          type: 'text',
          text: formatEditOutput(value.path, edits.length === 1 && (edits[0]?.replace_all ?? false), {
            edits: edits.length,
            basis: value.basis as FsEditBasis | undefined,
            before: value.before,
            after: value.after,
          }),
        }]
      },
      presentationMeta: (args, value) => ({
        diffs: computeHunkDiffs(args.file_path, value.before, value.after)
          .map(({ path, oldText, newText, oldStart, newStart }) => ({ path, oldText, newText, oldStart, newStart })),
      }),
    },
    async execute(args: EditToolArgs, exec) {
      const input = parseEditArgs(args)
      // Resolve the per-call sandbox policy (approved mode > session override
      // > backend default, plus the session cwd root) BEFORE anything executes.
      const sandboxPolicy = await sandbox.resolvePolicy('edit', args, exec)
      const target = await ctx.fs.resolve(input.filePath, sessionResolveOptions(exec, sandboxPolicy?.workspaceRoot))
      // Single-slot decision: the policy plugin returns { version: vObserved } or
      // throws FS_NOT_OBSERVED; the bare default is undefined (unconditional edit).
      // No stat — the bare default never manufactures a version basis. The intent
      // slot itself can throw FS_NOT_OBSERVED for an unread target, so it sits
      // inside the try: both that refusal and the provider's guarded-mutation
      // failure get the model-facing remedy below.
      let outcome
      try {
        const intent = await ctx.waterfall('fs/edit-intent', target, exec, () => undefined)
        outcome = await ctx.fs.editText(
          target,
          input.edits.length === 1 && input.edits[0] !== undefined ? input.edits[0] : input.edits,
          intent,
          exec.signal,
          sandboxPolicy,
        )
      } catch (error: unknown) {
        // A sandbox denial becomes the shared [sandbox: …] marker (the model
        // recognizes it from bash); guarded mutation failures receive their
        // stable model-facing diagnostic; anything else passes through.
        throw remediateFsError(sandbox.mapError(error, sandboxPolicy), target.displayPath)
      }
      ctx.emit('fs/observed', target, { kind: 'present', version: outcome.version }, exec)
      return {
        path: target.displayPath,
        before: outcome.before,
        after: outcome.after,
        ...outcome.basis === undefined ? {} : { basis: outcome.basis },
      }
    },
    // Pure display: a diff card of the literal replacement (old_string → new_string), derived
    // from the call args. `oldText: old_string || null` matches claude-agent-acp's Edit arm;
    // new_string is a required arg here, so it maps straight to newText.
    presentCall(args: EditToolArgs): DiffCallView {
      return {
        card: 'diff',
        title: `Edit ${args.file_path}`,
        diffs: rawReplacements(args).map(edit => ({ path: args.file_path, oldText: edit.old_string || null, newText: edit.new_string })),
        locations: [{ path: args.file_path }],
      }
    },
    // Applied metadata replaces the call-time snippet; errors or malformed replay metadata use
    // the generic result rendering.
    presentResult(args, result: ToolResult): DiffResultView | undefined {
      if (result.isError) return undefined
      const diffs = diffsFromMeta(result.meta)
      if (diffs === undefined) return undefined
      return { card: 'diff', title: `Edit ${args.file_path}`, diffs }
    },
  }))
}
