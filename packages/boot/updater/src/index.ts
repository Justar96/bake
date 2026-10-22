/**
 * Bake's self-updater: check the signed release manifest, install a newer
 * release beside the running one, and roll back to an earlier one.
 *
 * Only a managed install — one the installers laid out under
 * `<root>/versions/` — is updated. The running release keeps running from its
 * own directory; the next launch starts the new one.
 *
 * @module bake-updater
 */

export { compareVersions, isReleaseVersion, VERSION_PATTERN } from './version.ts'
export {
  archiveUrl, fetchRelease, parseManifest, RELEASE_TARGETS, UpdateError, verifySignature,
  type ReleaseArtifact, type ReleaseManifest, type ReleaseSource, type ReleaseTarget,
} from './manifest.ts'
export { DEFAULT_RELEASE_BASE_URL, GITHUB_RELEASE_BASE_URL, RELEASE_PUBLIC_KEYS, releaseSource } from './keys.ts'
export { CURRENT_POINTER, currentOf, currentVersion, detectInstall, directoryFor, versionDirectory, type InstallLayout, type ManagedInstall } from './layout.ts'
export {
  acquireLock, addDshAlias, installRelease, LAUNCH_MARKER, markLaunched, pointAt, PRUNE_AFTER_MS, windowsLauncher,
  type InstallOptions, type InstallProgress, type InstallResult,
} from './install.ts'
export {
  cachedUpdate, CHECK_CACHE, CHECK_INTERVAL_MS, checksDisabled, FAILED_CHECK_RETRY_MS, hostTarget, recordCheck, refreshCheck,
  statusOf, type RefreshOptions, type ReleaseStatus,
} from './check.ts'
export { selfUpdate, type SelfUpdateOptions, type UpdateOutcome } from './update.ts'
export { rollbackRelease, rollbackTarget, type RollbackOptions, type RollbackOutcome } from './rollback.ts'
export {
  failureLine, LAST_RELEASE_WITHOUT_SELF_CHECK, LAUNCH_CHECK_TIMEOUT_MS, launchProblem, RELEASE_COMMAND, type LaunchCheck,
} from './verify.ts'
