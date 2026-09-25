/**
 * The public keys a Bake release manifest must be signed by.
 * @module @deepseek-ai/dsh-updater/keys
 */

/**
 * Release signing keys, base64 DER SubjectPublicKeyInfo (Ed25519).
 *
 * More than one entry lets a new key be trusted for a release before the old
 * one stops signing. The private halves never enter the repository;
 * `release:assemble` reads one from `BAKE_RELEASE_SIGNING_KEY_FILE`. The
 * installers and `distribution/host/release-key.pub` carry the same first
 * key, and a test holds every copy to this list.
 */
export const RELEASE_PUBLIC_KEYS: readonly string[] = [
  'MCowBQYDK2VwAyEAG1Z33Mis04XnFsekLrnPDBwSgn26uNeLz3XtGal+xC4=',
]

/** Default release host, as the installers name it. */
export const DEFAULT_RELEASE_BASE_URL = 'https://bake.justar.dev'

/**
 * The repository's GitHub releases as a release host: the manifest from the
 * release marked latest, each archive from its own tag (see `archiveUrl`).
 * Every release the workflow publishes carries both, signed the same way, so
 * `BAKE_RELEASE_BASE_URL` may name either host.
 */
export const GITHUB_RELEASE_BASE_URL = 'https://github.com/Justar96/bake/releases/latest/download'

/**
 * The release host and trusted keys for this environment.
 *
 * `BAKE_RELEASE_BASE_URL` points at another host, as the installers allow,
 * such as {@link GITHUB_RELEASE_BASE_URL}.
 * `BAKE_RELEASE_PUBLIC_KEY` adds one trusted key, for a release signed with a
 * throwaway key in a local or CI check. Whoever can set the environment can
 * already replace the command, so trusting it grants nothing new.
 *
 * @param env - the process environment.
 * @returns the host origin, without a trailing slash, and the keys to trust.
 */
export function releaseSource(
  env: Record<string, string | undefined> = process.env,
): { readonly base: string; readonly keys: readonly string[] } {
  const base = (env['BAKE_RELEASE_BASE_URL']?.trim() || DEFAULT_RELEASE_BASE_URL).replace(/\/+$/, '')
  const extra = env['BAKE_RELEASE_PUBLIC_KEY']?.trim()
  return { base, keys: extra === undefined || extra === '' ? RELEASE_PUBLIC_KEYS : [...RELEASE_PUBLIC_KEYS, extra] }
}
