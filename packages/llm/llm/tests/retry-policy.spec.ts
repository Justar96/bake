import { describe, expect, it } from 'vitest'
import {
  isRetryableFailureCode,
  resolveRetryPolicy,
  retryDelayMs,
  RetryPolicySchema,
} from '@deepseek-ai/dsh-llm'
import type { RetryPolicyConfig } from '@deepseek-ai/dsh-llm'
import { MAX_TIMER_DELAY_MS } from 'bake-timeout'

describe('provider retry policy', () => {
  it('resolves immutable normal defaults', () => {
    const policy = resolveRetryPolicy(undefined, 'provider.retryPolicy')

    expect(policy).toEqual({
      mode: 'normal',
      maxRetries: 5,
      retryableCodes: ['EMPTY_RESPONSE', 'RATE_LIMIT', 'SERVER', 'TIMEOUT', 'TRANSPORT'],
      initialDelayMs: 500,
      maxDelayMs: 10_000,
      jitterRatio: 0.1,
    })
    expect(Object.isFrozen(policy)).toBe(true)
    if (policy.mode !== 'normal') throw new Error('expected normal policy')
    expect(Object.isFrozen(policy.retryableCodes)).toBe(true)
  })

  it('resolves and detaches a configured normal policy', () => {
    const retryableCodes = ['BUSY']
    const config: RetryPolicyConfig = {
      mode: 'normal',
      maxRetries: 4,
      retryableCodes,
      backoff: {
        initialDelayMs: 25,
        maxDelayMs: 100,
        jitterRatio: 0,
      },
    }

    const policy = resolveRetryPolicy(config, 'provider.retryPolicy')
    retryableCodes.push('LATE')

    expect(policy).toEqual({
      mode: 'normal',
      maxRetries: 4,
      retryableCodes: ['BUSY'],
      initialDelayMs: 25,
      maxDelayMs: 100,
      jitterRatio: 0,
    })
  })

  it('resolves always mode with default backoff', () => {
    expect(resolveRetryPolicy({ mode: 'always' }, 'provider.retryPolicy')).toEqual({
      mode: 'always',
      initialDelayMs: 500,
      maxDelayMs: 10_000,
      jitterRatio: 0.1,
    })
    expect(RetryPolicySchema).toBeDefined()
  })

  it('ignores normal-only fields retained after switching to always mode', () => {
    const layered = {
      mode: 'always',
      maxRetries: 5,
      retryableCodes: ['SERVER'],
    } as unknown as RetryPolicyConfig

    expect(resolveRetryPolicy(layered, 'provider.retryPolicy')).toEqual({
      mode: 'always',
      initialDelayMs: 500,
      maxDelayMs: 10_000,
      jitterRatio: 0.1,
    })
  })

  it.each([
    [{ mode: 'normal', maxRetries: -1 }, /maxRetries/],
    [{ mode: 'normal', maxRetries: 1.5 }, /maxRetries/],
    [{ mode: 'normal', maxRetries: Number.MAX_SAFE_INTEGER + 1 }, /maxRetries/],
    [{ mode: 'always', backoff: { initialDelayMs: 0 } }, /initialDelayMs/],
    [{ mode: 'normal', backoff: { maxDelayMs: Number.POSITIVE_INFINITY } }, /maxDelayMs/],
    [{ mode: 'normal', backoff: { initialDelayMs: MAX_TIMER_DELAY_MS + 1 } }, /initialDelayMs/],
    [{ mode: 'always', backoff: { maxDelayMs: MAX_TIMER_DELAY_MS + 1 } }, /maxDelayMs/],
    [{ mode: 'normal', backoff: { initialDelayMs: 20, maxDelayMs: 10 } }, /less than or equal/],
    [{ mode: 'always', backoff: { jitterRatio: 1.1 } }, /jitterRatio/],
    [{ mode: 'normal', retryableCodes: [] }, /must not be empty/],
    [{ mode: 'normal', retryableCodes: ['SERVER', 'SERVER'] }, /duplicates/],
    [{ mode: 'normal', retryableCodes: [''] }, /non-empty strings/],
    [{ mode: 'normal', retryableCodes: [429] }, /non-empty strings/],
    [{ mode: 'normal', maxRetires: 1 }, /unknown key "maxRetires"/],
    [{ mode: 'always', backoff: { initialDelay: 1 } }, /unknown key "initialDelay"/],
    [{ mode: 'sometimes' }, /mode must be "normal" or "always"/],
  ] as const)('rejects invalid policy %#', (config, message) => {
    expect(() => {
      resolveRetryPolicy(config as unknown as RetryPolicyConfig, 'provider.retryPolicy')
    }).toThrow(message)
  })
})

describe('shared retry classification and backoff', () => {
  const normal = resolveRetryPolicy({
    mode: 'normal',
    retryableCodes: ['RATE_LIMIT'],
    backoff: { initialDelayMs: 100, maxDelayMs: 1_000, jitterRatio: 0.5 },
  }, 'normal')
  const always = resolveRetryPolicy({
    mode: 'always',
    backoff: { initialDelayMs: 100, maxDelayMs: 1_000, jitterRatio: 0 },
  }, 'always')

  it('classifies codes by the bounded list, and every code under an always policy', () => {
    expect(isRetryableFailureCode(normal, 'RATE_LIMIT')).toBe(true)
    expect(isRetryableFailureCode(normal, 'AUTH')).toBe(false)
    expect(isRetryableFailureCode(always, 'AUTH')).toBe(true)
  })

  it('backs off exponentially with symmetric jitter, capped at the ceiling', () => {
    expect(retryDelayMs(normal, 1, {}, () => 0)).toBe(50)
    expect(retryDelayMs(normal, 1, {}, () => 1)).toBe(150)
    expect(retryDelayMs(normal, 3, {}, () => 0.5)).toBe(400)
    expect(retryDelayMs(normal, 5, {}, () => 1)).toBe(1_000)
  })

  it('honors a provider delay within the ceiling and declines or replaces a longer one', () => {
    expect(retryDelayMs(normal, 1, { providerRetryAfterMs: 700 }, () => 0)).toBe(700)
    expect(retryDelayMs(normal, 1, { providerRetryAfterMs: 5_000 }, () => 0)).toBeUndefined()
    expect(retryDelayMs(always, 2, { providerRetryAfterMs: 5_000 }, () => 0)).toBe(200)
  })
})
