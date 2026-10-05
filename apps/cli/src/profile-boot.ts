/**
 * Shared profile boot for every `dsh` surface: resolve the profile, stack its
 * patch layers (bundle layers in `dsh.profile.bundles` order, the profile's
 * own `cordis.patch.yml`, `--patch` overlays, the telemetry switch), mount the
 * tree over the profile's empty root config, and wire fail-loud until
 * readiness, late-rejection reporting after it, and bounded shutdown.
 *
 * App flags are not the launcher's business: the invocation's inner arguments
 * are provided to the tree through `ctx.cmdlineArgs`, where any injected app
 * plugin may read the same immutable snapshot.
 * @module @deepseek-ai/dsh/profile-boot
 */

import { randomBytes } from 'node:crypto'
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { FiberState, type Context } from '@deepseek-ai/cordis'
import type { PatchOptions } from '@deepseek-ai/cordis-plugin-include'
import {
  boot,
  readProfilePatches,
  createProfileResolutionGeneration,
  healProfilesModuleFallback,
  healIsolatedProfileModuleFallback,
  initProfile,
  installFailLoud,
  loadOverlayPatches,
  loadProfile,
  PluginPackages,
  PROFILE_PATCH_FILENAME,
  PROFILE_TEMPLATES,
  resolveProfileDir,
  type ProfileContext,
  type Profile,
  type ProfileResolutionGeneration,
  type ProfileResolutionMode,
} from '@deepseek-ai/dsh-app-boot'
import { resolveDshHome } from '@deepseek-ai/dsh-home-paths'
import { installProxyFromEnvironment } from '@deepseek-ai/dsh-http-proxy'
import { DSH_LAUNCH_ENVIRONMENT_KEY, type LaunchEnvironmentSnapshot } from '@deepseek-ai/dsh-launch-environment'
import type {} from 'bake-agent-loop'
import { provideCmdline, type AppReady } from '@deepseek-ai/dsh-cmdline'
import { createLateRejectionReporter } from './late-rejections.ts'
import { createProcessShutdown, type ProcessShutdown } from './process-shutdown.ts'
import { tolerateLostTerminal } from './terminal-hangup.ts'

const NAME = 'dsh'

/** Launcher-owned readiness signal committed only after boot and host setup succeed. */
function createAppReady(): { service: AppReady; commit(): void } {
  let ready = false
  const listeners = new Set<() => void>()
  return {
    service: {
      onReady(listener) {
        if (ready) {
          listener()
          return () => {}
        }
        listeners.add(listener)
        return () => { listeners.delete(listener) }
      },
    },
    commit() {
      if (ready) return
      ready = true
      for (const listener of [...listeners]) listener()
      listeners.clear()
    },
  }
}

/**
 * The home-level user patch layer (`$DSH_HOME/cordis.patch.yml`), applied
 * over every profile's own layer. Resolved per call, not at module load:
 * `$DSH_HOME` may be set by the test or launcher after import.
 * @returns the absolute patch-file path.
 */
export function homePatchPath(): string {
  return join(resolveDshHome(), PROFILE_PATCH_FILENAME)
}

/** Absolute path of this dsh installation's package.json (both anchors: src/ and lib/ sit one level under apps/cli). */
export const INSTALL_ANCHOR = fileURLToPath(new URL('../package.json', import.meta.url))

/** The empty root entry list every profile tree patches over. */
const PROFILE_ROOT_CONFIG = `# dsh profile root — an empty entry list. The tree is composed as patches:
# each bundle in package.json's dsh.profile.bundles, then cordis.patch.yml, then any
# --patch overlays. Edit cordis.patch.yml, not this file.
[]
`

/** Root config filename inside a profile directory. */
export const PROFILE_ROOT_FILENAME = 'cordis.yml'

/** Whether `path` already holds exactly the empty root config. */
function holdsProfileRootConfig(path: string): boolean {
  try {
    return readFileSync(path, 'utf8') === PROFILE_ROOT_CONFIG
  } catch {
    return false
  }
}

/**
 * Make the profile's `cordis.yml` the empty root config without a reader ever
 * seeing part of a file. Every launch calls this while another launch of the
 * same profile may be reading the file, and the Include root rejects empty
 * content, so an in-place rewrite, which truncates first, can fail that boot.
 * An unchanged file is left alone. Otherwise the content goes to a
 * random-suffix sibling created exclusively, which is renamed over the target.
 * @param dir - the profile directory.
 * @throws when the replacement fails and the file still holds other content.
 */
export function writeProfileRootConfig(dir: string): void {
  const path = join(dir, PROFILE_ROOT_FILENAME)
  if (holdsProfileRootConfig(path)) return
  const temp = `${path}.${randomBytes(6).toString('hex')}.tmp`
  try {
    writeFileSync(temp, PROFILE_ROOT_CONFIG, { flag: 'wx' })
    renameSync(temp, path)
  } catch (error) {
    rmSync(temp, { force: true })
    // Windows can refuse to replace a file another process holds open. A
    // concurrent launch that already wrote the same content leaves nothing to do.
    if (holdsProfileRootConfig(path)) return
    throw error
  }
}

/**
 * Initialize a missing profile from one shipped template. This copies only
 * the template's bundle list; local state from the
 * same-named shipped profile is not read, and no inheritance metadata is
 * persisted. Shipped profile names are reserved, and the target directory is
 * claimed exclusively so existing or concurrent state is never reused.
 * @param name - the new profile name.
 * @param fromDefaultProfile - shipped profile template to copy.
 * @param home - Harness home containing the profile directory.
 * @throws when the template is unknown, the target name is shipped, or the target directory exists.
 */
export function initializeProfileFromDefault(
  name: string,
  fromDefaultProfile: string,
  home: string = resolveDshHome(),
): void {
  const dir = resolveProfileDir(name, home)
  const template = Object.hasOwn(PROFILE_TEMPLATES, fromDefaultProfile)
    ? PROFILE_TEMPLATES[fromDefaultProfile]
    : undefined
  if (template === undefined) {
    const expected = Object.keys(PROFILE_TEMPLATES).sort().map(value => JSON.stringify(value)).join(', ')
    throw new Error(
      `${NAME}: unknown default profile ${JSON.stringify(fromDefaultProfile)}; expected one of ${expected}`,
    )
  }
  if (Object.hasOwn(PROFILE_TEMPLATES, name)) {
    throw new Error(
      `${NAME}: profile ${JSON.stringify(name)} is shipped and cannot be a custom profile target; `
      + 'omit --from-default-profile to use it',
    )
  }
  mkdirSync(dirname(dir), { recursive: true })
  try {
    mkdirSync(dir)
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error
    const manifestPath = join(dir, 'package.json')
    if (existsSync(manifestPath)) {
      throw new Error(
        `${NAME}: profile ${JSON.stringify(name)} already exists at ${manifestPath}; `
        + 'omit --from-default-profile to use it',
      )
    }
    throw new Error(
      `${NAME}: profile directory ${dir} already exists; choose an unused profile name`,
    )
  }
  try {
    initProfile(dir, template.bundles)
  } catch (error) {
    try {
      rmSync(dir, { recursive: true, force: true })
    } catch (cleanupError) {
      throw new AggregateError(
        [error, cleanupError],
        `${NAME}: profile initialization failed and ${dir} could not be removed`,
      )
    }
    throw error
  }
}
/**
 * Load a resolved profile for `name` and restore the empty root config. The
 * root is checked on every load: the whole composition is patch layers, and the
 * vendored Loader's tree write-back (a plugin self-disposing persists the
 * current tree) can bake composed rows into this file — which would duplicate
 * every bundle insert on the next boot. The file exists on disk only because
 * the Loader needs a real include root to anchor `baseUrl` at the profile
 * directory (the config dump anchors on the same file, so both compose over
 * the identical base).
 * @param name - the profile name.
 * @param userLayer - `false` skips parsing `cordis.patch.yml` (the default dump).
 * @param fromDefaultProfile - shipped template used once to initialize a missing profile.
 * @returns the loaded profile.
 * @throws when explicit initialization names an unknown template or an existing profile.
 */
export function prepareProfile(name: string, userLayer = true, fromDefaultProfile?: string): Profile {
  if (fromDefaultProfile !== undefined) initializeProfileFromDefault(name, fromDefaultProfile)
  const profile = loadProfile(NAME, name, INSTALL_ANCHOR, undefined, { userLayer })
  writeProfileRootConfig(profile.dir)
  return profile
}

/** One profile's patch layers, in application order. */
interface ComposedProfile {
  profile: Profile
  /** Immutable package fallback selected before any plugin imports. */
  resolution: ProfileResolutionGeneration
  /** Command-line overlay contents, frozen for this invocation. */
  overlays: PatchOptions[]
}

/**
 * Load `name` and compose its effective patch stack: bundle layers in
 * `dsh.profile.bundles` order (a base-backed profile gets the base bundle's
 * platform-gated shell rows), the profile's user layer, the home-level user
 * layer (`$DSH_HOME/cordis.patch.yml` — machine-local preferences that apply
 * to every profile, so it outranks the per-profile layer), `--patch` overlays,
 * then the telemetry switch.
 * @param name - the profile name.
 * @param patchFiles - `--patch` overlay paths, in argv order.
 * @param resolutionMode - runtime lookup, disk links, or dual verification of both.
 * @param fromDefaultProfile - shipped template for a missing named profile.
 * @param resolvedProfile - application-owned profile and installation.
 * @returns the profile and its patch layers.
 */
async function composeProfile(
  name: string,
  patchFiles: readonly string[],
  resolutionMode: ProfileResolutionMode,
  fromDefaultProfile?: string,
  resolvedProfile?: ResolvedProfileRuntime,
): Promise<ComposedProfile> {
  const profile = resolvedProfile?.profile ?? prepareProfile(name, true, fromDefaultProfile)
  if (resolvedProfile !== undefined) writeProfileRootConfig(profile.dir)
  const resolutionOptions = { installAnchor: resolvedProfile?.installAnchor ?? INSTALL_ANCHOR, profile }
  if (resolvedProfile !== undefined && resolutionMode !== 'runtime') healIsolatedProfileModuleFallback(resolvedProfile)
  const resolution = resolutionMode === 'runtime' || resolvedProfile !== undefined
    ? await createProfileResolutionGeneration(resolutionOptions)
    : await healProfilesModuleFallback(resolutionOptions)
  const overlays = patchFiles.flatMap(file => loadOverlayPatches(NAME, resolve(file)))
  return { profile, resolution, overlays }
}

/** An application-owned profile and its independent installation fallback. */
export interface ResolvedProfileRuntime {
  /** Profile already loaded from the application's own directory. */
  profile: Profile
  /** Absolute package.json path of the application's dsh installation. */
  installAnchor: string
}

/** Options for {@link runProfile}. */
export interface RunProfileOptions {
  /** This run's frozen environment snapshot, provided before any entry mounts. */
  environment: LaunchEnvironmentSnapshot
  /** The profile name to boot. */
  profile: string
  /** Loaded application profile; bypasses named profile initialization when supplied. */
  resolvedProfile?: ResolvedProfileRuntime | undefined
  /** Shipped template used once to initialize a missing profile. */
  fromDefaultProfile?: string | undefined
  /** `--patch` overlay paths, in argv order. */
  patchFiles: readonly string[]
  /** The invocation's inner arguments, handed to the tree through `ctx.cmdlineArgs`. */
  args: readonly string[]
  /** Application-owned package runtime, scoped to plugin package operations. */
  packageManager?: ProfileContext['packageManager']
  /** Module fallback backend; defaults to runtime. Plain Node callers may override it; pkg executables always use runtime. */
  resolutionMode?: ProfileResolutionMode
}

/**
 * Boot one profile invocation end to end and leave process lifetime to the
 * mounted plugins (or to a one-shot runner the composition mounts).
 * @param options - environment snapshot, profile name, overlays, and the booted app's own arguments.
 * @returns the settled root context and the shutdown controller.
 * @throws after disposing startup resources; cleanup failures retain the original error.
 */
export async function runProfile(options: RunProfileOptions): Promise<{ ctx: Context; shutdown: ProcessShutdown }> {
  // Before the first plugin mounts and before anything can issue a request: Node's fetch ignores the
  // proxy environment on its own, so every profile would otherwise connect directly. Resolving from
  // the launcher's snapshot — not `process.env` — is what lets a proxy declared in a `.env` layer
  // work, which the NODE_USE_ENV_PROXY flag cannot do because Node samples the environment at start.
  const disposeProxy = await installProxyFromEnvironment(
    options.environment,
    (message) => { process.stderr.write(`${NAME}: ${message}\n`) },
  )

  const packaged = (process as NodeJS.Process & { pkg?: unknown }).pkg !== undefined
  const resolutionMode = packaged ? 'runtime' : options.resolutionMode ?? 'runtime'
  const app: { current?: Context } = {}
  let disposal: Promise<void> | undefined
  const dispose = (): Promise<void> => disposal ??= (async () => {
    const failures: unknown[] = []
    for (const release of [
      async () => { await app.current?.parallel('app/shutdown') },
      () => app.current?.fiber.dispose(),
      disposeProxy,
    ]) {
      try { await release() } catch (error) { failures.push(error) }
    }
    if (failures.length === 1) throw failures[0]
    if (failures.length > 1) throw new AggregateError(failures, 'dsh: profile cleanup failed')
  })()
  try {
    const composed = await composeProfile(
      options.profile, options.patchFiles, resolutionMode, options.fromDefaultProfile, options.resolvedProfile,
    )
    const appReady = createAppReady()
    const shutdown = createProcessShutdown(dispose)
    const signalShutdown = new AbortController()
    const interrupt = (code: number): void => {
      signalShutdown.abort()
      shutdown.interrupt(code)
    }
    // Signals own teardown throughout the startup window, not only after boot()
    // settles: an inserted provider can publish before sibling rows finish mounting.
    // SIGTERM is a supervisor's ordinary stop request and exits 0 on every
    // surface — the launcher does not know whether the app considered its work
    // complete; SIGINT is a user interrupt and reports 130.
    process.on('SIGTERM', () => { interrupt(0) })
    process.on('SIGINT', () => { interrupt(130) })
    // SIGHUP means the terminal is gone (on Windows, its console closed): the
    // same bounded disposal, reporting 129. Without this listener Node dies of
    // the signal and skips disposal and the exit phase, which flush the session
    // and stop managed subprocesses. Writes to the lost terminal now fail, so
    // stdio errors are dropped before disposal writes anything. A repeated
    // SIGHUP joins the running disposal instead of forcing exit.
    process.on('SIGHUP', () => {
      tolerateLostTerminal()
      signalShutdown.abort()
      shutdown.hangup(129)
    })
    const failLoud = installFailLoud(NAME, process, async () => {
      await dispose()
    })
    // Readiness ends the startup window: from then until exit, disposal
    // included, a rejection is recorded and shown instead of being fatal, so
    // one that arrives during a clean shutdown cannot turn its status into 1.
    // Registered before the tree can subscribe, so no ready work runs under
    // the fatal rule.
    appReady.service.onReady(() => {
      failLoud.tolerateRejections(createLateRejectionReporter({
        directory: join(resolveDshHome(), 'diagnostics'),
        present: rejection => app.current?.bail('app/unhandled-rejection', rejection) === true,
        warn: (message) => { app.current?.logger(NAME).warn('%s', message) },
        stderr: process.stderr,
      }))
    })

    const rootConfig = join(composed.profile.dir, PROFILE_ROOT_FILENAME)
    const profileContext: ProfileContext = {
      name: options.profile,
      ...(options.packageManager === undefined ? {} : { packageManager: options.packageManager }),
      dir: composed.profile.dir, patchPath: composed.profile.patchPath,
      installAnchor: options.resolvedProfile?.installAnchor ?? INSTALL_ANCHOR,
      startedBundles: composed.profile.layers.map(layer => layer.packageName),
      cwd: process.cwd(), home: resolveDshHome(),
      overlays: composed.overlays, telemetryDisabledEnv: process.env.DSH_TELEMETRY_DISABLED,
    }
    const ctx = await boot(NAME, rootConfig, readProfilePatches(NAME, profileContext, composed.profile), async (hostCtx) => {
      app.current = hostCtx
      hostCtx.provide('profileContext', profileContext)
      // Before any config-tree entry mounts, so plugins resolve all launch-time
      // environment values from the same immutable launch snapshot.
      hostCtx.provide(DSH_LAUNCH_ENVIRONMENT_KEY, options.environment)
      await hostCtx.plugin(PluginPackages, resolutionMode === 'link' ? {} : {
        generation: composed.resolution,
        behavior: resolutionMode === 'dual' ? 'verify' : 'enforce',
      })
      // The command line and bounded exit request are launcher facts available
      // to every app plugin that injects the argument snapshot.
      provideCmdline(hostCtx, {
        args: options.args,
        exit: code => void shutdown.shutdown(code),
        ready: appReady.service,
      })
    })
    app.current = ctx
    if (!signalShutdown.signal.aborted
      && ctx.fiber.state === FiberState.ACTIVE
      && ctx.get('loader') !== undefined) {
      appReady.commit()
    }
    return { ctx, shutdown }
  } catch (error) {
    try { await dispose() } catch (cleanupError) {
      throw new AggregateError([error, cleanupError], 'dsh: profile startup and cleanup failed')
    }
    throw error
  }
}
