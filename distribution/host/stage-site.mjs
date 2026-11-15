// The Worker's build step, which `wrangler deploy` runs first: stage into
// site/ exactly the files https://bake.justar.dev serves. It first runs the
// gate the service deploys behind: a staged native manifest must verify with
// the committed native key, so a key a local check may add in the environment
// is removed here. No archive is staged: the Worker redirects each one to its
// GitHub release.
import { execFileSync } from 'node:child_process'
import { copyFileSync, existsSync, mkdirSync, rmSync } from 'node:fs'
import { dirname, join } from 'node:path'

const root = import.meta.dirname
const env = { ...process.env }
delete env.BAKE_NATIVE_RELEASE_PUBLIC_KEY
execFileSync(process.execPath, [join(root, 'verify-native-manifest.mjs'), '--allow-missing-archives'], { cwd: root, env, stdio: 'inherit' })

const site = join(root, 'site')
rmSync(site, { recursive: true, force: true })
const files = [
  ['index.html', 'index.html'],
  ['install.sh', 'install.sh'],
  ['install.ps1', 'install.ps1'],
  ['public/native/stable.json', 'native/stable.json'],
  ['public/native/stable.json.sig', 'native/stable.json.sig'],
]
for (const [from, to] of files) {
  // The native route is staged once a native release is; the gate above
  // already refused a manifest without its signature.
  if (from.startsWith('public/native/') && !existsSync(join(root, from))) continue
  mkdirSync(dirname(join(site, to)), { recursive: true })
  copyFileSync(join(root, from), join(site, to))
}
console.log(`Staged the download site in ${site}`)
