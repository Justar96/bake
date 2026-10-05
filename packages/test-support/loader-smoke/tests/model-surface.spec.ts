import { describe, expect, it } from 'vitest'
import { modelSurfaceSizes, normalizeModelSurface, renderModelSurface, type ModelSurfaceRequest } from '../src/model-surface.ts'

const messages = [{ role: 'user', content: 'Summarize the workspace rules.' }] as ModelSurfaceRequest['messages']

describe('model surface', () => {
  it('keeps, measures, and renders the system field, which adapters send ahead of the messages', () => {
    const request = normalizeModelSurface({ system: 'Answer from /tmp/work.', messages }, [['/tmp/work', '<workspace>']])

    expect(request.system).toBe('Answer from <workspace>.')
    expect(modelSurfaceSizes(request).systemChars).toBe('Answer from <workspace>.'.length)
    const rendered = renderModelSurface('fixture', request)
    expect(rendered).toContain('## System\n\n```text\nAnswer from <workspace>.\n```\n\n## Messages')
  })

  it('adds no system section when the request leaves the field unset', () => {
    const request = normalizeModelSurface({ messages }, [])

    expect('system' in request).toBe(false)
    expect(modelSurfaceSizes(request).systemChars).toBe(0)
    expect(renderModelSurface('fixture', request)).not.toContain('## System')
  })
})
