// The download service at https://bake.justar.dev, as a Cloudflare Worker.
// It serves the files stage-site.mjs staged into site/ through the ASSETS
// binding, which holds no archive, so every archive redirects to its GitHub
// release. wrangler.jsonc configures it; distribution/README.md describes it.
import { createHandler } from './handler.mjs'

/** One handler per isolate, so its daily activity reports stay deduplicated there. */
let handle

export default {
  /**
   * @param {Request} request - the client's request.
   * @param {{ ASSETS: { fetch: (input: Request | URL | string) => Promise<Response> }, BAKE_ARCHIVE_REDIRECT?: string, GISSX_ACTIVITY_URL?: string, GISSX_ACTIVITY_TOKEN?: string, BAKE_ACTIVITY_SALT?: string }} env - the Worker's bindings, variables, and secrets.
   * @param {{ waitUntil: (work: Promise<unknown>) => void }} ctx - the request's execution context.
   * @returns {Promise<Response>} the answer.
   */
  async fetch(request, env, ctx) {
    handle ??= createHandler({
      archiveRedirect: env.BAKE_ARCHIVE_REDIRECT,
      activityUrl: env.GISSX_ACTIVITY_URL,
      activityToken: env.GISSX_ACTIVITY_TOKEN,
      activitySalt: env.BAKE_ACTIVITY_SALT,
      async read(path) {
        // The assets binding ignores the host; only the path names the file.
        const asset = await env.ASSETS.fetch(new URL(`/${path}`, 'https://assets.invalid'))
        if (asset.status !== 200) {
          await asset.body?.cancel()
          return undefined
        }
        const body = await asset.arrayBuffer()
        return { body, size: body.byteLength }
      },
    })
    return handle(request, {
      address: request.headers.get('cf-connecting-ip') ?? '',
      waitUntil: work => ctx.waitUntil(work),
    })
  },
}
