# 测试策略

[English](testing.md) | 中文

Bake 在能够观察到行为的层级测试共享运行时、终端应用与构建后的 Profile。当前命令以根目录 [AGENTS.md](../AGENTS.md) 为准。

## 选择检查

- 使用 `bun run test:runtime <file>` 运行聚焦的运行时 spec。Cordis 与 Ink 测试在 Node 上执行；纯模块与工具脚本测试在 Bun 上执行。
- 使用 `bun run test` 运行共享的纯模块与 Node 集成套件。
- 使用 `bun apps/tui/scripts/tui.ts check docs` 检查终端文档，使用 `bun apps/tui/scripts/tui.ts check types` 检查 TUI 类型。
- 终端行为变更时，使用 `bun run test:e2e` 运行无需密钥的构建后 Profile PTY 场景。
- 需要完整构建、检查与 PTY 路径时，使用 `bun run verify`。

报告实际运行的检查，包括失败与跳过的工作。未经明确批准，不要绕过钩子。

## 测试真实行为

产品可见的插件需要非单元的组合测试，经 Loader 与应用或进程启动仅用于测试的 `cordis.yml`。只 mock 模型、网络与时钟等外部非确定性输入；被测试的运行时保持真实。通过公开入口断言模型可见输出、持久状态或用户可见行为。端到端断言应重新读取文件或运行命令，不依赖 agent 的自述。

测试须自行管理临时路径、端口、进程全局变更与子进程。清理过程恢复共享状态，并等待所属工作结束。只有单独运行才通过的 spec 是该 spec 的缺陷。

## 保留会话证据

模型可见输入必须能够从会话日志重建。不得为使测试通过而覆盖、移动或删除已提交的会话代际。[会话格式状态](session-format-status.zh.md)负责已发布格式与迁移规则。
