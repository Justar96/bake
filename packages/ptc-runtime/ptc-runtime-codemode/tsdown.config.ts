import { defineConfig } from 'tsdown'

/**
 * The worker entry is a separate bundle beside `lib/index.js`, which starts it
 * by URL; pi-codemode stays external so its own worker module and the QuickJS
 * wasm resolve from the installed package.
 */
export default defineConfig([
  { entry: ['lib/types/index.js'], outDir: 'lib', format: ['esm'], platform: 'node', target: 'es2024', fixedExtension: false, dts: false, clean: false },
  { entry: { worker: 'lib/types/worker.js' }, outDir: 'lib', format: ['esm'], platform: 'node', target: 'es2024', fixedExtension: false, dts: false, clean: false },
])
