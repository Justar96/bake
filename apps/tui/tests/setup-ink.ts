/**
 * Make the Node test environment capable of hosting Ink.
 *
 * Ink patches `console` on render so application logs cannot interleave with
 * the frame, and `patch-console` does that by constructing `console.Console`.
 * Vitest replaces the global console with its own reporter object, which has no
 * `Console` constructor, so `render()` throws `console.Console is not a
 * constructor` before any assertion runs.
 *
 * Restoring the real constructor fixes the environment, not the product.
 * Disabling Ink's console patching in the runner would let a stray log corrupt
 * a live frame in production to satisfy a test runner.
 */

import { Console } from 'node:console'

const patched = console as Console & { Console?: typeof Console }
patched.Console ??= Console
