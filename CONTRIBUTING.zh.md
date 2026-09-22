# 参与 Bake 开发

[English](CONTRIBUTING.md) | 中文

## 首次构建与运行

安装 `package.json` 中固定版本的 Bun、Node 24 或更新版本，以及带有 Node 开发头文件的 C/C++ 编译器。在仓库根目录运行：

```sh
bun install --frozen-lockfile
bun run build
bun run start
```

`start` 运行构建产物，不会重新构建。它需要交互式终端；真实模型请求通过 `/login` 或 `DEEPSEEK_API_KEY` 配置凭据。`bun run start --help` 显示 TUI 参数，不会启动会话。

Bake 的 `start`、`dev:tui` 和 `dsh` 命令默认使用 `~/.bake`。设置 `DSH_HOME` 可指定其他目录。现有 `~/.dsh` 配置和会话不会被移动或修改。覆盖目录后会使用其中现有的配置，不仅仅是凭据。

## 选择开发流程

| 工作 | 命令 | 行为 |
| --- | --- | --- |
| Ink 组件 | `bun run dev` | 热重载录制内容的组件预览；不启动 Agent，不联网。 |
| 流式预览 | `bun run dev --replay` | 将录制的行逐步加入预览。 |
| 中文文案 | `bun run dev --locale zh` | 使用中文组件字典。 |
| 完整 Agent | `bun run dev:tui` | 构建运行时和 TUI，然后启动 Node Agent；不会自动重启。 |
| 仅修改 TUI | `bun run build:tui && bun run start` | 重新打包终端代码；需要已有运行时构建。 |
| 修改共享运行时 | `bun run build && bun run start` | 重新构建运行时包和终端代码。 |

预览接受输入以测试布局，但不会提交任务。Ctrl-C 退出预览；真实 Agent 需要按两次 Ctrl-C 才会退出。重新构建文件前请停止真实 Agent。不要用 `bun --bun` 替换 Node 进程：其加载器依赖 V8 内部接口。

`bun run dsh --help` 显示构建后的配置和插件启动器，它使用同一个 Bake 数据目录。外部配置的插件安装与 Bun 源码工作区互相独立。会话导航和终端命令见[应用 README](apps/tui/packages/app/README.zh.md)。

## 端到端检查

安装依赖后，可通过一个命令执行完整的无密钥开发检查：

```sh
bun run verify
```

该命令构建全部产物，运行工作区校验及 TUI 类型、测试、React 实例、布局和文档检查，然后通过真实 PTY 驱动构建后的 Node 配置。场景回放录制的模型响应，同时执行真实工具并检查持久化会话、屏幕内容和终端恢复。它们不需要模型密钥，也不会修改你的 Bake 数据目录。

已有运行时构建后，可使用较短的迭代命令：

```sh
bun run check
bun run test:e2e --list
bun run test:e2e --only rendering
bun run test:e2e --no-build
bun apps/tui/scripts/tui.ts spec packages/ui/tests/placement.spec.tsx
bun run test:runtime apps/cli/tests/args.spec.ts
```

`--only` 包含场景所需的前置场景。E2E 默认只重新构建 TUI，不构建共享运行时；`--no-build` 直接使用现有产物。失败场景会报告等待条件，并将终端记录保留在 `apps/tui/.smoke/`。`bun apps/tui/scripts/tui.ts help` 列出监听模式和其他诊断选项。运行 `bun run lint` 检查源码。

检查失败不意味着可以刷新全部快照或绕过钩子。组件快照使用固定尺寸的终端流，终端模拟器测试显式启用交互渲染；保留 CI 检测。保留会话代际文件并审阅预期输出变更。不要提交凭据。

## 上游发布

`origin` 指向 Bake。`upstream` 用于选择性移植 DeepSeek Harness 修复及其测试和许可证声明；不要自动合并或向其推送。裁剪依赖前检查包导入、TypeScript 引用和配置 YAML。采纳持久化变更前审阅会话格式迁移。

当前没有自动上游版本监控。发现新版本并不代表授权应用它。移植修复后，重新构建保留的运行时包，并运行相关定向测试和终端场景。
