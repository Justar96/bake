/** DeepSeek account balance lookup through the configured provider origin. */
import type { DeepSeekProtocol } from './types.ts'

/** Provider-reported available credit for one currency. Amounts retain the API's decimal strings. */
export interface DeepSeekBalanceInfo {
  readonly currency: string
  readonly totalBalance: string
  readonly grantedBalance: string
  readonly toppedUpBalance: string
}

/** Current account availability and remaining credit, not historical token usage. */
export interface DeepSeekBalance {
  readonly isAvailable: boolean
  readonly balanceInfos: readonly DeepSeekBalanceInfo[]
}

function balanceURL(baseURL: string, protocol: DeepSeekProtocol): string {
  const url = new URL(baseURL)
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.search || url.hash) {
    throw new Error('DeepSeek balance needs an HTTP(S) API root without credentials, query, or fragment.')
  }
  const parts = url.pathname.split('/').filter(Boolean)
  if (parts.at(-1) === 'v1') parts.pop()
  if (protocol === 'messages' && parts.at(-1) === 'anthropic') parts.pop()
  url.pathname = `/${[...parts, 'user', 'balance'].join('/')}`
  return url.href
}

function parseBalance(value: unknown): DeepSeekBalance {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) throw new Error('DeepSeek returned an invalid balance response.')
  const wire = value as Record<string, unknown>
  if (typeof wire.is_available !== 'boolean' || !Array.isArray(wire.balance_infos)) {
    throw new Error('DeepSeek returned an invalid balance response.')
  }
  const balanceInfos = wire.balance_infos.map((entry: unknown): DeepSeekBalanceInfo => {
    if (entry === null || typeof entry !== 'object' || Array.isArray(entry)) {
      throw new Error('DeepSeek returned an invalid balance response.')
    }
    const item = entry as Record<string, unknown>
    if (typeof item.currency !== 'string' || !/^[A-Z]{3}$/u.test(item.currency)
      || ![item.total_balance, item.granted_balance, item.topped_up_balance]
        .every(amount => typeof amount === 'string' && /^\d+(?:\.\d+)?$/u.test(amount))) {
      throw new Error('DeepSeek returned an invalid balance response.')
    }
    return {
      currency: item.currency,
      totalBalance: item.total_balance as string,
      grantedBalance: item.granted_balance as string,
      toppedUpBalance: item.topped_up_balance as string,
    }
  })
  return { isAvailable: wire.is_available, balanceInfos }
}

/**
 * Read the provider's remaining credit with the same key and endpoint snapshot as model calls.
 * Redirects are refused so a gateway cannot forward the key to another origin.
 */
export async function fetchDeepSeekBalance(connection: {
  readonly baseURL: string
  readonly protocol: DeepSeekProtocol
  readonly apiKey: string
}, signal: AbortSignal, fetchImpl: typeof fetch = globalThis.fetch): Promise<DeepSeekBalance> {
  signal.throwIfAborted()
  const url = balanceURL(connection.baseURL, connection.protocol)
  const response = await fetchImpl(url, {
    method: 'GET',
    headers: { authorization: `Bearer ${connection.apiKey}` },
    redirect: 'error',
    signal,
  })
  if (!response.ok) throw new Error(`DeepSeek balance request failed (HTTP ${response.status}).`)
  let wire: unknown
  try { wire = await response.json() }
  catch { throw new Error('DeepSeek returned an invalid balance response.') }
  signal.throwIfAborted()
  return parseBalance(wire)
}

/** Format the balance without rounding or combining different currencies. */
export function formatDeepSeekBalance(balance: DeepSeekBalance): string {
  return [
    `DeepSeek API balance · API calls ${balance.isAvailable ? 'available' : 'unavailable'}`,
    ...balance.balanceInfos.map(info =>
      `${info.currency}: ${info.totalBalance} remaining (${info.grantedBalance} granted, ${info.toppedUpBalance} topped up)`),
    ...(balance.balanceInfos.length === 0 ? ['No balance details reported.'] : []),
  ].join('\n')
}
