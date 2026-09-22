/** Bundle the Node runtime; terminal presentation is built separately by Bun. */
import { globSync } from 'node:fs'
import { dirname } from 'node:path'
import { defineConfig } from 'tsdown'
import { typertPlugin } from './packages/typert/generator/lib/types/tsdown-plugin.js'

export default defineConfig({
  workspace: globSync(['vendor/*/package.json', 'packages/*/*/package.json', 'apps/cli/package.json']).map(dirname),
  entry: ['lib/types/{index,invariant,startup}.js'],
  outDir: 'lib',
  format: ['esm'],
  platform: 'node',
  target: 'es2024',
  fixedExtension: false,
  dts: false,
  clean: false,
  plugins: [typertPlugin({ mode: 'workspace', faces: ['host'] })],
})
