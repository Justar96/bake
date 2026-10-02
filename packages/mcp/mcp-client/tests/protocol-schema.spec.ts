/** MCP 2025-11-25 schema checks and the field order they give recorded and model-visible values. */

import { describe, expect, it } from 'vitest'
import {
  parseCallToolResult,
  parseListedTool,
  parseReadResourceResult,
  resourceListResult,
  resourceTemplateListResult,
} from '../src/protocol.ts'

/** Key order is part of the contract, so compare serialized bytes. */
function bytes(value: unknown): string {
  return JSON.stringify(value)
}

describe('parseListedTool', () => {
  it('orders object schemas type, properties, required, then other keywords, and drops unknown tool fields', () => {
    const tool = parseListedTool({
      _meta: { origin: 'fixture' },
      inputSchema: {
        $schema: 'http://json-schema.org/draft-07/schema#',
        required: ['query'],
        title: 'SearchArgs',
        properties: { query: { type: 'string', description: 'Search text' } },
        type: 'object',
        additionalProperties: false,
      },
      outputSchema: { properties: { hits: { type: 'integer' } }, type: 'object' },
      vendorExtension: true,
      description: 'Search the index.',
      execution: { taskSupport: 'optional', vendor: 1 },
      name: 'search',
    }, 'tools[0]')
    expect(bytes(tool)).toBe(bytes({
      name: 'search',
      description: 'Search the index.',
      inputSchema: {
        type: 'object',
        properties: { query: { type: 'string', description: 'Search text' } },
        required: ['query'],
        $schema: 'http://json-schema.org/draft-07/schema#',
        title: 'SearchArgs',
        additionalProperties: false,
      },
      outputSchema: { type: 'object', properties: { hits: { type: 'integer' } } },
      execution: { taskSupport: 'optional' },
      _meta: { origin: 'fixture' },
    }))
  })

  it('keeps a __proto__ schema keyword as an own property', () => {
    const tool = parseListedTool(JSON.parse('{"name":"t","inputSchema":{"type":"object","__proto__":{"x":1}}}'), 'tools[0]')
    expect(Object.getPrototypeOf(tool.inputSchema)).toBe(Object.prototype)
    expect(Object.hasOwn(tool.inputSchema, '__proto__')).toBe(true)
  })

  it.each([
    [{ name: 3, inputSchema: { type: 'object' } }, 'tools[2].name must be a string'],
    [{ name: 't' }, 'tools[2].inputSchema is required'],
    [{ name: 't', inputSchema: { type: 'string' } }, 'tools[2].inputSchema.type must be "object"'],
    [{ name: 't', inputSchema: { type: 'object', required: 'q' } }, 'tools[2].inputSchema.required must be an array'],
    [{ name: 't', inputSchema: { type: 'object', properties: [] } }, 'tools[2].inputSchema.properties must be an object'],
    [{ name: 't', inputSchema: { type: 'object' }, outputSchema: { properties: {} } }, 'tools[2].outputSchema.type is required'],
    [
      { name: 't', inputSchema: { type: 'object' }, execution: { taskSupport: 'sometimes' } },
      'tools[2].execution.taskSupport must be one of "required", "optional", "forbidden"',
    ],
    [{ name: 't', inputSchema: { type: 'object' }, annotations: { readOnlyHint: 'yes' } }, 'tools[2].annotations.readOnlyHint must be a boolean'],
    [{ name: 't', inputSchema: { type: 'object' }, icons: [{ src: 'a', theme: 'blue' }] }, 'tools[2].icons[0].theme must be one of "light", "dark"'],
  ])('rejects %j', (value, message) => {
    expect(() => parseListedTool(value, 'tools[2]')).toThrow(message)
  })
})

describe('parseCallToolResult', () => {
  it('keeps the consumed fields and rebuilds content blocks in schema order', () => {
    const result = parseCallToolResult({
      _meta: { trace: 'x' },
      vendor: 1,
      isError: false,
      structuredContent: { answer: 42 },
      content: [
        { text: 'hello', type: 'text', vendor: 2, annotations: { priority: 0.5, vendor: 3, audience: ['user'] }, _meta: { q: 1 } },
        { mimeType: 'image/png', data: 'AAAA', type: 'image' },
        { type: 'resource_link', uri: 'memo://a', name: 'a', size: 3, title: 'A', icons: [{ theme: 'dark', src: 's', vendor: 1 }] },
        { type: 'resource', resource: { text: 't', uri: 'memo://b', vendor: 1, mimeType: 'text/plain' } },
        { type: 'resource', resource: { uri: 'memo://c', text: 'wins', blob: 'AA==' } },
        { type: 'resource', resource: { uri: 'memo://d', blob: 'AA==' } },
      ],
    })
    expect(bytes(result)).toBe(bytes({
      content: [
        { type: 'text', text: 'hello', annotations: { audience: ['user'], priority: 0.5 }, _meta: { q: 1 } },
        { type: 'image', data: 'AAAA', mimeType: 'image/png' },
        { name: 'a', title: 'A', icons: [{ src: 's', theme: 'dark' }], uri: 'memo://a', size: 3, type: 'resource_link' },
        { type: 'resource', resource: { uri: 'memo://b', mimeType: 'text/plain', text: 't' } },
        { type: 'resource', resource: { uri: 'memo://c', text: 'wins' } },
        { type: 'resource', resource: { uri: 'memo://d', blob: 'AA==' } },
      ],
      structuredContent: { answer: 42 },
      isError: false,
    }))
  })

  it('treats an absent content list as empty', () => {
    expect(parseCallToolResult({ structuredContent: { ok: true } })).toEqual({ content: [], structuredContent: { ok: true } })
  })

  it.each([
    [null, 'result must be an object'],
    [{ content: 'x' }, 'content must be an array'],
    [{ content: [{ type: 'bogus' }] }, 'content[0].type must be one of "text", "image", "audio", "resource_link", "resource"'],
    [{ content: [{ type: 'text' }] }, 'content[0].text is required'],
    [{ content: [{ type: 'image', mimeType: 'image/png', data: '!!!' }] }, 'content[0].data must be base64'],
    [{ content: [{ type: 'audio', data: 'AA', mimeType: 'audio/wav', annotations: { audience: ['bot'] } }] }, 'content[0].annotations.audience[0] must be one of "user", "assistant"'],
    [{ content: [{ type: 'text', text: 'a', annotations: { priority: 2 } }] }, 'content[0].annotations.priority must be between 0 and 1'],
    [{ content: [{ type: 'text', text: 'a', _meta: [] }] }, 'content[0]._meta must be an object'],
    [{ content: [], isError: 'yes' }, 'isError must be a boolean'],
  ])('rejects %j', (value, message) => {
    expect(() => parseCallToolResult(value)).toThrow(message)
  })

  it.each([
    ['2025-01-01T00:00:00Z', true],
    ['2025-01-01T00:00:00.123+05:30', true],
    ['2024-02-29T23:59:59-08:00', true],
    ['2025-02-29T00:00:00Z', false],
    ['2025-01-01T00:00Z', false],
    ['2025-01-01T00:00:00', false],
    ['yesterday', false],
  ])('checks annotation date-time %s', (lastModified, valid) => {
    const parse = () => parseCallToolResult({ content: [{ type: 'text', text: 'a', annotations: { lastModified } }] })
    if (valid) expect(parse).not.toThrow()
    else expect(parse).toThrow('content[0].annotations.lastModified must be an RFC 3339 date-time with an offset')
  })
})

describe('resource results', () => {
  it('rebuilds list pages with the cursor first and entries in schema order', () => {
    expect(bytes(resourceListResult(
      [{ uri: 'memo://a', vendor: 1, mimeType: 'text/plain', name: 'a', _meta: { z: 1, a: 2 } }],
      'next-page',
    ))).toBe(bytes({
      nextCursor: 'next-page',
      resources: [{ name: 'a', uri: 'memo://a', mimeType: 'text/plain', _meta: { z: 1, a: 2 } }],
    }))
    expect(bytes(resourceListResult([]))).toBe(bytes({ resources: [] }))
    expect(bytes(resourceTemplateListResult([{ uriTemplate: 'memo://{id}', name: 'memo', vendor: 1 }])))
      .toBe(bytes({ resourceTemplates: [{ name: 'memo', uriTemplate: 'memo://{id}' }] }))
  })

  it('names the failing entry of a resource list', () => {
    expect(() => resourceListResult([{ name: 'a', uri: 'memo://a', size: 'big' }])).toThrow('resources[0].size must be a finite number')
  })

  it('keeps unknown top-level read fields after the schema fields', () => {
    expect(bytes(parseReadResourceResult({
      vendor: 1,
      contents: [{ text: 'memo', uri: 'memo://a', vendor: 2 }],
      _meta: { trace: 'x' },
    }))).toBe(bytes({
      _meta: { trace: 'x' },
      contents: [{ uri: 'memo://a', text: 'memo' }],
      vendor: 1,
    }))
    expect(() => parseReadResourceResult({ contents: [{ uri: 'memo://a', blob: '!!!' }] })).toThrow('contents[0].blob must be base64')
  })
})
