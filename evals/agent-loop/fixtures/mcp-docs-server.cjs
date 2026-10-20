'use strict'
/**
 * The stdio MCP server of the `mcp_docs` eval scenario: newline-delimited
 * JSON-RPC 2.0 on standard input and output, with no dependency but Node.
 * `scenarios.ts` copies this file and writes the `documents.json` beside it
 * that holds the documents it serves, and the evaluator points a sample's
 * `$BAKE_HOME/mcp.json` at this copy.
 *
 * It answers `initialize` with its instructions and the `tools` and
 * `resources` capabilities, `tools/list` with one `get_document` tool,
 * `resources/list` with one resource per document, `resources/templates/list`
 * with none, `resources/read`, and `tools/call`. Every document is reachable
 * both as a resource and through the tool, so an agent without the resource
 * tools can still finish the task. Nothing but JSON-RPC messages reaches
 * standard output, and the server writes no file.
 */
const { readFileSync } = require('node:fs')
const { join } = require('node:path')

/** The handshake revisions bake-mcp accepts (`bake-mcp`'s `ProtocolVersion::SUPPORTED`), newest first. */
const VERSIONS = ['2025-11-25', '2025-06-18', '2025-03-26', '2024-11-05']
/** The server's name, instructions, tool name, and documents, as the fixture wrote them. */
const FIXTURE = JSON.parse(readFileSync(join(__dirname, 'documents.json'), 'utf8'))
const NAMES = FIXTURE.documents.map(document => document.name).join(', ')

const send = message => process.stdout.write(JSON.stringify(message) + '\n')
const answer = (id, result) => send({ jsonrpc: '2.0', id: id, result: result })
const failed = (id, code, message) => send({ jsonrpc: '2.0', id: id, error: { code: code, message: message } })
const text = value => ({ content: [{ type: 'text', text: value }] })

function handle(message) {
  const id = message.id
  // A notification, such as notifications/initialized, is answered by nothing.
  if (id === undefined || id === null) return
  const params = message.params === undefined || message.params === null ? {} : message.params
  switch (message.method) {
    case 'initialize':
      answer(id, {
        protocolVersion: VERSIONS.includes(params.protocolVersion) ? params.protocolVersion : VERSIONS[1],
        capabilities: { tools: { listChanged: false }, resources: { listChanged: false } },
        serverInfo: { name: FIXTURE.server, version: '1.0.0' },
        instructions: FIXTURE.instructions,
      })
      return
    case 'ping':
      answer(id, {})
      return
    case 'tools/list':
      answer(id, { tools: [{
        name: FIXTURE.tool,
        description: 'Return one project document in full. The documents are: ' + NAMES + '.',
        inputSchema: {
          type: 'object',
          properties: { name: { type: 'string', description: 'The document to return, one of: ' + NAMES + '.' } },
          required: ['name'],
          additionalProperties: false,
        },
      }] })
      return
    case 'resources/list':
      answer(id, { resources: FIXTURE.documents.map(document => ({
        uri: document.uri, name: document.name, title: document.title, description: document.description, mimeType: 'text/plain',
      })) })
      return
    case 'resources/templates/list':
      answer(id, { resourceTemplates: [] })
      return
    case 'resources/read': {
      const found = FIXTURE.documents.find(document => document.uri === params.uri)
      if (found === undefined) {
        failed(id, -32602, 'unknown resource ' + String(params.uri))
        return
      }
      answer(id, { contents: [{ uri: found.uri, mimeType: 'text/plain', text: found.text }] })
      return
    }
    case 'tools/call': {
      if (params.name !== FIXTURE.tool) {
        failed(id, -32602, 'unknown tool ' + String(params.name))
        return
      }
      const wanted = params.arguments === undefined || params.arguments === null ? undefined : params.arguments.name
      const found = FIXTURE.documents.find(document => document.name === wanted)
      // A tool that failed reports isError in a successful response, as MCP requires.
      if (found === undefined) {
        answer(id, Object.assign(text('There is no document named ' + String(wanted) + '. The documents are: ' + NAMES + '.'), { isError: true }))
        return
      }
      answer(id, text(found.text))
      return
    }
    default:
      failed(id, -32601, 'unknown method ' + String(message.method))
  }
}

let pending = ''
process.stdin.setEncoding('utf8')
process.stdin.on('data', chunk => {
  pending += chunk
  const lines = pending.split('\n')
  pending = lines.pop()
  for (const line of lines) {
    if (line.trim() === '') continue
    let message
    try {
      message = JSON.parse(line)
    } catch {
      continue
    }
    handle(message)
  }
})
// The stdio shutdown closes the server's standard input; nothing is left to answer.
process.stdin.on('end', () => process.exit(0))
