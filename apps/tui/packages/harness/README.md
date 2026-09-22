# Component preview

Run `bun run dev` from the repository root to hot-reload the Ink preview without an agent, network access, or a model key. `--replay` animates recorded rows; `--locale zh` selects Chinese. A positional fixture path is resolved relative to `dev.tsx`; absolute paths and paths containing spaces are supported. Quote paths in the shell.

The opening screen labels itself as a component preview and shows `bun run dev:tui` for real chat. Enter adds your message to the local preview transcript with a notice that no agent is connected. Up recalls it for editing. Preview messages exist only in memory and do not execute commands, call a model, or change the recording.

The preview renders interactively when input and output are TTY streams, including under CI. Resizing keeps the composer at the terminal bottom; short terminals give its input priority over the preview notice and status. Ctrl-C exits both Ink and Bun's hot watcher.

The [development guide](../../../../CONTRIBUTING.md#choose-a-development-loop) owns build and run instructions. [The preview smoke](../app/tests/dev.test.ts) checks submission, history recall, resizing, fixture paths, and process exit with CI detection enabled in English and Chinese.
