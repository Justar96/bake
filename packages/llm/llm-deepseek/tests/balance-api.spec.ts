import { describe, expect, it, vi } from 'vitest'
import { fetchDeepSeekBalance, formatDeepSeekBalance } from '../src/common/balance-api.ts'

const BALANCE = {
  is_available: true,
  balance_infos: [
    { currency: 'CNY', total_balance: '110.00', granted_balance: '10.00', topped_up_balance: '100.00' },
    { currency: 'USD', total_balance: '0.05', granted_balance: '0.00', topped_up_balance: '0.05' },
  ],
}

describe('DeepSeek balance API', () => {
  it.each([
    ['chat-completions', 'https://api.deepseek.com', 'https://api.deepseek.com/user/balance'],
    ['messages', 'https://api.deepseek.com/anthropic', 'https://api.deepseek.com/user/balance'],
    ['messages', 'https://gateway.example/proxy/anthropic/v1', 'https://gateway.example/proxy/user/balance'],
  ] as const)('reads %s balance on the configured origin', async (protocol, baseURL, endpoint) => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(new Response(JSON.stringify(BALANCE), { status: 200 }))
    const balance = await fetchDeepSeekBalance({ protocol, baseURL, apiKey: 'private-key' },
      new AbortController().signal, fetcher)
    expect(fetcher).toHaveBeenCalledExactlyOnceWith(endpoint, {
      method: 'GET', headers: { authorization: 'Bearer private-key' },
      redirect: 'error', signal: expect.any(AbortSignal),
    })
    expect(formatDeepSeekBalance(balance)).toBe([
      'DeepSeek API balance · API calls available',
      'CNY: 110.00 remaining (10.00 granted, 100.00 topped up)',
      'USD: 0.05 remaining (0.00 granted, 0.05 topped up)',
    ].join('\n'))
  })

  it('preserves unavailable and empty balances without inventing credit', async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValue(new Response(JSON.stringify({ is_available: false, balance_infos: [] })))
    const balance = await fetchDeepSeekBalance({ protocol: 'chat-completions', baseURL: 'https://api.deepseek.com', apiKey: 'key' },
      new AbortController().signal, fetcher)
    expect(formatDeepSeekBalance(balance)).toBe('DeepSeek API balance · API calls unavailable\nNo balance details reported.')
  })

  it('rejects redirects, invalid responses, and credential-bearing URLs', async () => {
    const fetcher = vi.fn<typeof fetch>().mockResolvedValueOnce(new Response('{}', { status: 302 }))
      .mockResolvedValueOnce(new Response(JSON.stringify({ is_available: true,
        balance_infos: [{ currency: 'CNY\u001b[31m', total_balance: '1', granted_balance: '0', topped_up_balance: '1' }] })))
    const connection = { protocol: 'messages' as const, baseURL: 'https://api.deepseek.com/anthropic', apiKey: 'private-key' }
    await expect(fetchDeepSeekBalance(connection, new AbortController().signal, fetcher))
      .rejects.toThrow('DeepSeek balance request failed (HTTP 302).')
    await expect(fetchDeepSeekBalance(connection, new AbortController().signal, fetcher))
      .rejects.toThrow('DeepSeek returned an invalid balance response.')
    await expect(fetchDeepSeekBalance({ ...connection, baseURL: 'https://key@api.deepseek.com/anthropic' },
      new AbortController().signal, fetcher)).rejects.toThrow('without credentials')
    expect(fetcher).toHaveBeenCalledTimes(2)
  })

  it('does not send a request after cancellation', async () => {
    const abort = new AbortController()
    abort.abort()
    const fetcher = vi.fn<typeof fetch>()
    await expect(fetchDeepSeekBalance({ protocol: 'chat-completions', baseURL: 'https://api.deepseek.com', apiKey: 'key' },
      abort.signal, fetcher)).rejects.toBeDefined()
    expect(fetcher).not.toHaveBeenCalled()
  })
})
