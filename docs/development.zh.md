# 开发指南

[English](development.md) | 中文

本指南介绍如何从源码构建 Bake、日常开发循环、变更合入前应运行的检查，以及代码的组织方式。[`CONTRIBUTING.zh.md`](../CONTRIBUTING.zh.md) 说明变更如何评审、上游 DeepSeek Harness 修复如何移植。[`AGENTS.md`](../AGENTS.md) 列出每个变更都要遵循的工程规则。

<a id="prerequisites"></a>

## 前置条件

- **Bun**，版本以 [`package.json`](../package.json) 中的 `packageManager` 为准。Bun 负责依赖安装、`bun.lock`、工作区脚本、构建与 Git hook。不要添加 pnpm 或 npm 锁文件。
- **Node.js 24 或更新版本。** Agent 本身运行在 Node 上，因为其启动加载器依赖 Bun 引擎所缺少的 V8 内部接口。切勿用 `bun --bun` 代替 Node 进程。
- **带 Node 头文件的 C/C++ 工具链**，用于构建原生模块。
- **终端场景测试需要 Linux 或 macOS。** 构建、类型检查和单元测试也能在 Windows 上运行，但 PTY 场景（`bun run test:e2e`）需要 Linux 或 macOS。可以使用 WSL 2；请把检出目录放在 Linux 文件系统中，并在其中单独安装依赖。

<a id="first-build"></a>

## 首次构建

在仓库根目录执行：

```sh
bun install --frozen-lockfile
bun run build
bun run start
```

`bun install` 同时会安装 Lefthook Git hook。`start` 直接运行已有构建产物而不重新构建，需要交互式终端。`bun run start --help` 打印 TUI 参数而不启动会话。

源码运行与已安装的 `bake` 使用同一个主目录：`~/.bake`，或 `DSH_HOME` 指定的目录。覆盖后会选用该目录中已有的配置与会话，而不仅仅是其中的凭据。已有的 `~/.dsh` 数据不会被移动或修改。

发起真实模型请求时，使用 `/login` 登录，或在环境变量或仓库根目录下被 git 忽略的 `.env` 中设置 `DEEPSEEK_API_KEY`。`DEEPSEEK_BASE_URL` 可选，用于覆盖 API 地址。切勿提交密钥或 `.env`。

<a id="development-loops"></a>

## 开发循环

| 工作内容 | 命令 | 行为 |
|---|---|---|
| Ink 组件 | `bun run dev` | 热重载录制好的组件预览；不需要 Agent、网络或模型密钥。 |
| 流式预览 | `bun run dev --replay` | 将录制的行回放到预览中。 |
| 中文文案 | `bun run dev --locale zh` | 使用中文组件词典。 |
| 完整 Agent | `bun run dev:tui` | 构建运行时和 TUI，然后启动 Node Agent；不会自动重启。 |
| 仅改 TUI | `bun run build:tui && bun run start` | 重新打包终端代码；需要已有的运行时构建。 |
| 改共享运行时 | `bun run build && bun run start` | 重新构建运行时包和终端代码。 |

预览接受输入以便测试布局，但不会提交任务。在预览中按一次 Ctrl-C 即退出；真实 Agent 需要按两次。重新构建前请先停止真实 Agent。

`bun run dsh --help` 以同一 Bake 主目录提供构建好的配置与插件启动器。外部配置插件的安装与 Bun 的源码工作区相互独立。

<a id="checks"></a>

## 检查

只运行覆盖本次变更的检查，而不是默认运行全部套件。任何终端行为变更还需要运行 PTY 场景。

```sh
bun run check          # workspace, tsconfig paths, TUI types, tests, peer identity, layout, docs
bun run test           # TUI unit and spec tests
bun run test:runtime <file-or-dir>   # focused shared-runtime tests (Vitest on Node)
bun run test:e2e       # keyless PTY scenarios against the built profile
bun run verify         # build + check + PTY scenarios
bun run lint           # Oxlint over apps/tui and scripts
```

PTY 场景回放录制的模型响应，同时运行真实工具，再检查持久化的会话、屏幕内容和终端恢复情况。它们不需要模型密钥，也不会改动你的 Bake 主目录。

运行时构建完成后，可用以下命令缩短迭代：

```sh
bun run test:e2e --list
bun run test:e2e --only rendering
bun run test:e2e --no-build
bun apps/tui/scripts/tui.ts spec packages/ui/tests/placement.spec.tsx
bun run test:runtime apps/cli/tests/args.spec.ts
```

`--only` 会包含场景的前置场景。E2E 通常只重新构建 TUI，不重建共享运行时；`--no-build` 原样使用已有产物。失败的场景会报告其等待条件，并把记录保留在 `apps/tui/.smoke/`。`bun apps/tui/scripts/tui.ts help` 列出监视模式、夹具录制和性能诊断选项。

使用 Cordis 或 Ink 的测试运行在 Node 上；纯模块和工具测试运行在 Bun 上。检查失败不是刷新所有快照或绕过 hook 的理由。请审阅预期输出的变化，保持 CI 检测不变，并且切勿覆盖 `snapshots/` 下已录制的会话代。

### Git hook

[`lefthook.yml`](../lefthook.yml) 让 hook 保持快速：

- `pre-commit` 用 Oxlint 检查暂存的 TypeScript 和 JavaScript（并应用修复），拒绝空白错误，并检查 `vendor/*/src` 下的变更是否同步更新了 [`vendor/README.md`](../vendor/README.md)。
- `pre-push` 运行 `bun run verify-workspace` 和 `bun run typecheck`。

hook 不运行测试或构建；请自行运行相关检查。未经维护者同意，切勿跳过 hook。

### CI

[`ci.yml`](../.github/workflows/ci.yml) 在推送到 `main` 以及拉取请求时运行无密钥检查。[`release.yml`](../.github/workflows/release.yml) 构建、签名并发布发行归档；流程见[发行指南](../distribution/README.zh.md)。

## 仓库结构

| 路径 | 内容 |
|---|---|
| [`apps/tui/`](../apps/tui/DESIGN.md) | 终端应用：`packages/app`（配置组合、Agent 控制、终端生命周期）、`packages/ui`（无副作用的 Ink 组件、投影、布局、本地化文案）、`packages/harness`（组件开发与录制）、夹具和开发工具。 |
| [`apps/cli/`](../apps/cli/README.zh.md) | `tui` 与 `headless` 配置的 Node 启动器，以及外部插件管理。 |
| [`packages/`](../packages/README.zh.md) | 共享 Agent 运行时：Agent 循环、会话、模型、工具、沙箱和插件服务。修改前请阅读[架构说明](architecture.zh.md)。 |
| [`native/`](../native/README.zh.md)、[`vendor/`](../vendor/README.md) | 原生支持与固定版本的 Cordis 源码。请保留其许可证和上游署名。 |
| [`distribution/`](../distribution/README.zh.md) | 发行打包、签名与下载服务。 |
| [`snapshots/`](../snapshots/AGENTS.md) | 录制的会话证据，包括保留的历史代。 |

在运行时中工作时有用的参考：

- [Cordis 入门](cordis-primer.zh.md)：插件、服务与事件。
- [防御性模式](defensive-patterns.zh.md)：涉及生命周期或并发的工作前请先阅读。
- [会话格式状态](session-format-status.zh.md)：任何持久化变更前请先阅读。

## 约定

- 全程使用 ESM 与严格 TypeScript。本地相对导入使用 `.ts` 后缀；跨包导入使用声明的包名。运行时包保留 `@deepseek-ai/*` 名称，以便顺利移植上游修复。
- 共享 Node 运行时通过 `tsconfig.host.json` 构建；各包的 `tsconfig.json` 引用其工作区依赖。增删包时，请更新其引用，并运行 `bun run gen-workspace` 和 `bun run gen-tsconfig-paths`。
- TUI 中显示的产品文案位于 [`apps/tui/packages/ui/src/copy.ts`](../apps/tui/packages/ui/src/copy.ts)，同时提供英文和中文。
- 文档描述当前行为。随代码一起更新所属的 README 或 JSDoc，并保持中英文页面一致。
- 按紧急程度标记已知问题：`FIXME` 会阻止发布，`TODO` 应尽快修复，`XXX` 留待日后。

<a id="documenting-types-verbatim"></a>

### 逐字记录类型

[子系统页面](subsystems/README.zh.md)按源码原样粘贴声明及其 JSDoc。此类粘贴使用 ` ```ts type-equiv ` (or ` ```ts public-api ` for a class shown without implementation bodies) and register it in [`scripts/type-equiv.manifest.json`](../scripts/type-equiv.manifest.json) with its source file and symbol:

```json
{ "doc": "docs/subsystems/session.md", "symbol": "SessionEvent", "source": "packages/core/session/src/types.ts" }
```

`bun run verify-type-equiv` 会把每个代码块及其中文对应块与源码声明比对。修改被记录的声明时，请同时更新两种语言的粘贴内容，然后重新运行该检查。
