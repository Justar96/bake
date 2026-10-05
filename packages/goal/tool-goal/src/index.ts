/**
 * Model-facing `get_goal`, `create_goal`, and `update_goal` tools over the
 * persisted same-session goal domain.
 * @module @deepseek-ai/dsh-tool-goal
 */

import type { Context } from '@deepseek-ai/cordis'
import z from '@deepseek-ai/schemastery'
import { GoalId } from '@deepseek-ai/dsh-goal'
import type { GoalRef, GoalView } from '@deepseek-ai/dsh-goal'
import { boundContextSummary, createUserMessage, HarnessError } from '@deepseek-ai/dsh-llm'
import { defineTool } from 'bake-tools'
import type { GenericCallView } from 'bake-tools'
import {
  completionAuthority,
  goalToolExecution,
  requireDirectHuman,
} from './authority.ts'
import { presentCreateResult, presentGetResult, presentUpdateResult } from './presentation.ts'
import { renderWrapupContext } from './wrapup.ts'

export const name = 'tool-goal'
export const inject = ['agents', 'goals', 'tools', 'sessionProjections']

/** Model policy and hard lower bounds for goal-state updates. */
export interface Config {
  /** Minimum admitted goal rounds before the model may self-report `blocked`. */
  blockedAfterConsecutiveRounds?: number
}

/** Schemastery config for the goal-tool policy. */
export const Config: z<Config> = z.object({
  blockedAfterConsecutiveRounds: z.number().step(1).min(1).default(3),
})

/** Fully materialized tool policy. */
interface ResolvedConfig {
  readonly blockedAfterConsecutiveRounds: number
}

type UpdateAction = 'edit' | 'pause' | 'resume' | 'complete' | 'blocked'

const UPDATE_ACTIONS: UpdateAction[] = ['edit', 'pause', 'resume', 'complete', 'blocked']

/** The user owns the decision to start a goal; execution can only check that the user spoke this turn. */
const CREATE_DESCRIPTION =
  'Create the session\'s single goal, pursued across automatic rounds. Only when the user explicitly '
  + 'asks for one this turn. Subagents cannot.'

const GET_DESCRIPTION =
  'Return the session\'s current goal, or null.'

/**
 * The `update_goal` description, carrying the whole goal policy with its deployment-selected blocked
 * threshold, so the rules travel with the tool that applies them.
 * @param blockedAfter - consecutive goal rounds required before `blocked` is accepted.
 * @returns the composed description.
 */
function updateDescription(blockedAfter: number): string {
  return 'Change the goal; first get_goal for its exact goal_id and revision. '
    + 'edit/pause/resume need a user request this turn; complete/blocked also work in automatic rounds. '
    + 'A reopened or forked session\'s goal stays paused until the user asks to continue; then resume. '
    + 'complete only when the objective is met; blocked only after the same concrete obstacle persisted '
    + `at least ${blockedAfter} consecutive goal rounds (difficulty or remaining work is not one).`
}

/** Canonical goal-tool output, matching the existing compact Native JSON. */
type GoalToolValue =
  | { goal: null }
  | {
    goal: {
      id: string
      revision: number
      objective: string
      phase: GoalView['phase']
      roundsStarted: number
      maxGoalRounds: number
      blockedReason?: { code: string; message: string }
    }
    activation: GoalView['activation']
  }

const GOAL_VALUE_SCHEMA = {
  oneOf: [
    {
      type: 'object',
      additionalProperties: false,
      properties: {
        goal: { type: 'null', required: true },
      },
    },
    {
      type: 'object',
      additionalProperties: false,
      properties: {
        goal: {
          type: 'object',
          additionalProperties: false,
          required: true,
          properties: {
            id: { type: 'string', required: true },
            revision: { type: 'integer', required: true },
            objective: { type: 'string', required: true },
            phase: { type: 'string', required: true, enum: ['active', 'paused', 'blocked', 'complete'] },
            roundsStarted: { type: 'integer', required: true },
            maxGoalRounds: { type: 'integer', required: true },
            blockedReason: {
              type: 'object',
              additionalProperties: false,
              properties: {
                code: { type: 'string', required: true },
                message: { type: 'string', required: true },
              },
            },
          },
        },
        activation: { type: 'string', required: true, enum: ['armed', 'disarmed'] },
      },
    },
  ],
} as const

/** Validate config even when apply is called directly outside Loader normalization. */
function resolveConfig(config: Config): ResolvedConfig {
  const blockedAfter = config.blockedAfterConsecutiveRounds ?? 3
  if (!Number.isSafeInteger(blockedAfter) || blockedAfter < 1) {
    throw new TypeError('blockedAfterConsecutiveRounds must be a positive safe integer')
  }
  return { blockedAfterConsecutiveRounds: blockedAfter }
}

/** Whether optional text is meaningful rather than a strict-schema empty filler. */
function hasText(value: string | undefined): value is string {
  return value !== undefined && value !== ''
}

/** Whether an optional round cap is meaningful rather than a strict-schema zero filler. */
function hasRoundCap(value: number | undefined): value is number {
  return value !== undefined && value !== 0
}

/** The fields `update_goal` limits to one action, with the strict-schema filler each accepts. */
const ACTION_FIELDS = [
  { name: 'objective', action: 'edit', filler: '""' },
  { name: 'max_goal_rounds', action: 'edit', filler: '0' },
  { name: 'blocked_reason', action: 'blocked', filler: '""' },
] as const

/**
 * Reject meaningful values in fields the selected action does not use. The
 * message names each such field and its filler, because a model that copies
 * values from `get_goal` otherwise retries the same arguments.
 *
 * A strict-schema model must send every field, and the value it most often
 * has at hand is the one `get_goal` just returned. An `objective` or
 * `max_goal_rounds` equal to the addressed goal's current value changes
 * nothing, so it counts as a filler rather than costing a rejected step;
 * any other value is still rejected.
 * @param action - the selected update action.
 * @param args - the raw model arguments.
 * @param echo - the addressed goal's current values, when the ref names the current goal.
 */
function rejectUnusedFields(action: string, args: {
  objective?: string | undefined
  max_goal_rounds?: number | undefined
  blocked_reason?: string | undefined
}, echo?: Pick<GoalView, 'objective' | 'maxGoalRounds'>): void {
  const unused = ACTION_FIELDS.filter((field) => {
    if (field.action === action) return false
    if (field.name === 'max_goal_rounds') {
      return hasRoundCap(args.max_goal_rounds) && args.max_goal_rounds !== echo?.maxGoalRounds
    }
    if (field.name === 'objective') return hasText(args.objective) && args.objective !== echo?.objective
    return hasText(args[field.name])
  })
  if (unused.length === 0) return
  const names = unused.map(field => field.name).join(' and ')
  const fillers = unused.map(field => `${field.name}: ${field.filler}`).join(', ')
  throw new HarnessError(
    `${names} ${unused.length === 1 ? 'is' : 'are'} not used by action ${action}; `
      + `omit ${unused.length === 1 ? 'it' : 'them'} or send ${fillers}. `
      + 'objective and max_goal_rounds apply only to action edit; blocked_reason applies only to action blocked.',
    'GOAL_TOOL_INVALID_UPDATE',
  )
}

/** Build the exact compare-and-set ref from model arguments. */
function goalRef(goalId: string, revision: number): GoalRef {
  if (goalId.length === 0 || goalId !== goalId.trim()
    || !Number.isSafeInteger(revision) || revision < 1) {
    throw new HarnessError(
      'goal_id must be non-empty and revision must be a positive safe integer',
      'GOAL_TOOL_INVALID_UPDATE',
    )
  }
  return { id: GoalId(goalId), revision }
}

/** Stable compact model result; activation is an observation, not replay state. */
function goalValue(goal: GoalView | undefined): GoalToolValue {
  if (goal === undefined) return { goal: null }
  return {
    goal: {
      id: goal.id,
      revision: goal.revision,
      objective: goal.objective,
      phase: goal.phase,
      roundsStarted: goal.roundsStarted,
      maxGoalRounds: goal.maxGoalRounds,
      ...goal.blockedReason === undefined ? {} : {
        blockedReason: { code: goal.blockedReason.code, message: goal.blockedReason.message },
      },
    },
    activation: goal.activation,
  }
}

/** Reusable canonical output declaration for all three goal controls. */
const GOAL_OUTPUT = {
  schema: GOAL_VALUE_SCHEMA,
  render: (_args: unknown, value: GoalToolValue) => [{ type: 'text' as const, text: JSON.stringify(value) }],
}

/** Generic, args-only pending presentation shared by the goal tools. */
function present(title: string, kind: 'read' | 'other', rawInput?: unknown): GenericCallView {
  return { card: 'generic', title, kind, ...rawInput === undefined ? {} : { rawInput } }
}

/** Register the three Codex-shaped goal tools; their descriptions carry the whole goal policy. */
export function apply(ctx: Context, config: Config): void {
  const resolved = resolveConfig(config)

  ctx.tools.register(defineTool({
    name: 'get_goal',
    description: GET_DESCRIPTION,
    parameters: {},
    output: GOAL_OUTPUT,
    execute(_args, exec) {
      const execution = goalToolExecution(ctx, exec)
      return Promise.resolve(goalValue(ctx.goals.get(execution.agent)))
    },
    presentCall: () => present('Read current goal', 'read'),
    presentResult: (_args, result) => presentGetResult(result),
  }))

  ctx.tools.register(defineTool({
    name: 'create_goal',
    description: CREATE_DESCRIPTION,
    parameters: {
      objective: {
        type: 'string',
        required: true,
        description: 'From the user\'s request.',
      },
      max_goal_rounds: {
        type: 'number',
        description: 'Positive integer.',
      },
    },
    output: GOAL_OUTPUT,
    execute(args, exec) {
      const execution = goalToolExecution(ctx, exec)
      requireDirectHuman(ctx, execution)
      const goal = ctx.goals.create(execution.agent, {
        objective: args.objective,
        ...args.max_goal_rounds === undefined ? {} : { maxGoalRounds: args.max_goal_rounds },
      })
      return Promise.resolve(goalValue(goal))
    },
    presentCall: args => present('Create goal', 'other', args.objective),
    presentResult: (_args, result) => presentCreateResult(result),
  }))

  ctx.tools.register(defineTool({
    name: 'update_goal',
    description: updateDescription(resolved.blockedAfterConsecutiveRounds),
    parameters: {
      goal_id: { type: 'string', required: true },
      revision: { type: 'number', required: true },
      action: {
        type: 'string',
        required: true,
        enum: UPDATE_ACTIONS,
      },
      objective: { type: 'string', description: 'edit only.' },
      max_goal_rounds: { type: 'number', description: 'edit only.' },
      blocked_reason: {
        type: 'string',
        description: 'Required for blocked.',
      },
    },
    output: GOAL_OUTPUT,
    execute(args, exec) {
      const execution = goalToolExecution(ctx, exec)
      const ref = goalRef(args.goal_id, args.revision)
      const addressed = ctx.goals.get(execution.agent)
      const echo = addressed?.id === ref.id ? addressed : undefined
      const replacements = {
        ...hasText(args.objective) ? { objective: args.objective } : {},
        ...hasRoundCap(args.max_goal_rounds) ? { maxGoalRounds: args.max_goal_rounds } : {},
      }
      if (args.action === 'edit') {
        requireDirectHuman(ctx, execution)
        rejectUnusedFields(args.action, args)
        const goal = ctx.goals.edit(execution.agent, ref, replacements)
        return Promise.resolve(goalValue(goal))
      }
      if (args.action === 'pause' || args.action === 'resume') {
        requireDirectHuman(ctx, execution)
        rejectUnusedFields(args.action, args, echo)
        const current = addressed
        if (args.action === 'resume' && current?.id === ref.id && current.revision === ref.revision
          && current.phase === 'paused') {
          throw new HarnessError(
            'only the user can resume a paused goal',
            'GOAL_TOOL_RESUME_PAUSED',
          )
        }
        const goal = args.action === 'pause'
          ? ctx.goals.pause(execution.agent, ref)
          : ctx.goals.resume(execution.agent, ref)
        return Promise.resolve(goalValue(goal))
      }
      const authority = completionAuthority(ctx, execution)
      rejectUnusedFields(args.action, args, echo)
      if (args.action === 'blocked'
        && (args.blocked_reason === undefined || args.blocked_reason.trim().length === 0)) {
        throw new HarnessError('blocked_reason is required with action blocked', 'GOAL_TOOL_INVALID_UPDATE')
      }
      if (args.action === 'blocked' && authority.kind === 'goal-round'
        && authority.goal.roundsStarted < resolved.blockedAfterConsecutiveRounds) {
        throw new HarnessError(
          `blocked requires at least ${resolved.blockedAfterConsecutiveRounds} consecutive goal rounds; `
          + `current round is ${authority.goal.roundsStarted}`,
          'GOAL_TOOL_BLOCK_THRESHOLD',
        )
      }
      const goal = args.action === 'complete'
        ? ctx.goals.complete(execution.agent, ref)
        : ctx.goals.block(execution.agent, ref, {
          code: 'model-reported',
          message: args.blocked_reason as string,
        })
      if (authority.kind === 'goal-round') {
        exec.deferContext(createUserMessage({
          content: args.action === 'complete'
            ? renderWrapupContext(goal.objective)
            : renderWrapupContext(goal.objective, args.blocked_reason as string),
          source: {
            kind: 'plugin',
            plugin: 'tool-goal',
            form: 'notice',
            summary: boundContextSummary(`${args.action as string}: ${goal.objective}`),
          },
        }))
      }
      return Promise.resolve(goalValue(goal))
    },
    presentCall: args => present(
      `${args.action === 'blocked' ? 'Mark' : args.action.charAt(0).toUpperCase() + args.action.slice(1)} goal`,
      'other',
      hasText(args.blocked_reason)
        ? args.blocked_reason
        : hasText(args.objective)
          ? args.objective
          : hasRoundCap(args.max_goal_rounds) ? args.max_goal_rounds : args.goal_id,
    ),
    presentResult: (args, result) => presentUpdateResult(args.action, result),
  }))
}
