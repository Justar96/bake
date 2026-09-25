# 开发指南

[English](development.md) | 中文

搭建教程引导新贡献者从准备前置条件开始，直到检出目录通过检查。后面的贡献者参考介绍仓库布局、日常工作流和 CI 组织方式。设计依据与实现细节属于链接的 Agent Note 和脚本。

<a id="setup-tutorial"></a>

## 搭建教程

### 前置条件

- Node.js 支持 22.19+ 与 24+。CI 覆盖 22.19、24 和 26；见 [Node 引擎下限 Agent Note](../.agents/notes/implemented/process/2026-07-06-node-engine-floor.zh.md)。
- 启用了 Corepack 的 pnpm。仓库在 `package.json` 中固定使用 `pnpm@11.7.0`；如果 `pnpm --version` 无法通过 Corepack 解析，请先运行 `corepack enable`。
- Git 2.26 或更高版本；钩子设置会启用 Git 的 worktree 专属配置扩展。
- 可选：一个 DeepSeek API key，用于 Web、headless 和 ACP（Agent Client Protocol）自动化 agent（智能体）演示以及真实 API 的 e2e 测试。

### Windows 与 WSL 2

在 Windows 上，可以使用原生工具开发，也可以通过 WSL 2 使用 Linux 环境。WSL 2 既可用于验证 Linux 行为，也可在原生依赖编译或文件系统权限阻碍 Windows 开发时提供使用 Linux 工具链的途径。每种环境都需要准备相应的运行时、编译工具和权限；WSL 是可选项。

将检出目录、已安装的依赖和工具链放在同一操作系统环境中。使用 WSL 2 时，将检出目录放在 Linux 文件系统中；使用 Windows 原生工具时，则使用 Windows 文件系统。跨两种文件系统访问会给 Git、依赖安装和构建等 I/O 密集型操作增加开销。参见微软的[文件存储与性能指南](https://learn.microsoft.com/en-us/windows/wsl/filesystems#file-storage-and-performance-across-file-systems)。

在每种环境中分别安装依赖，因为不同操作系统使用的原生二进制和链接可能不同。测试结果适用于执行测试的环境；Windows 特有行为仍需在原生 Windows 上验证。

### 首次搭建

在仓库根目录安装依赖：

```sh
pnpm install
```

安装过程还会通过 `scripts/install-lefthook.mjs` 配置 worktree 本地的 Lefthook 钩子和 `dsh-translation-pairing` Git 合并驱动。[worktree 本地钩子 Agent Note](../.agents/notes/implemented/process/2026-07-27-worktree-local-lefthook.zh.md) 负责钩子路径的安全约定；[自动配对合并 Agent Note](../.agents/notes/implemented/process/2026-08-08-automatic-translation-pairing-merges.zh.md) 负责合并驱动。

如果依赖是从缓存恢复或 `postinstall` 被跳过而导致任一集成缺失，请手动安装：

```sh
node scripts/install-lefthook.mjs
```

如果包装脚本拒绝现有 Git 配置或报告陈旧锁，请遵循其诊断和所链接的 Agent Note，不要凭猜测编辑 worktree 元数据。移动检出目录后，请重新运行包装脚本以重新生成自有路径。

新克隆后请先运行一次类型检查：

```sh
pnpm run typecheck
```

`pnpm run typecheck` 成功退出即表示搭建完成。

## 贡献者参考

<a id="typescript-project-layout"></a>

### TypeScript 项目布局

共享 Node 运行时使用 `tsconfig.host.json`。TUI 类型由 `bun apps/tui/scripts/tui.ts check types` 检查。包级 `tsconfig.json` 引用工作区依赖；根目录 `tsconfig.json` 是编辑器与 Project Reference 的入口。

`bun run verify-tsconfig-paths` 检查生成的源码路径映射。新增或删除包时，同时检查 package 引用、聚合配置与工作区清单。

### 环境变量

真实的 DeepSeek 适配器和需要密钥的 agent 演示从环境变量或仓库根目录一个被 gitignore 的 `.env` 文件读取凭证：

```sh
DEEPSEEK_API_KEY=sk-...
DEEPSEEK_BASE_URL=https://... # optional
```

`DEEPSEEK_BASE_URL` 可选，默认为公开 API。请勿提交真实凭证。未设置 `DEEPSEEK_API_KEY` 时，真实 API 的 e2e 套件会自动跳过。

### Git 集成

当两种语言的文件都使用 Git 默认文本策略且能干净合并时，配对合并驱动会根据已确认的祖先、当前和另一侧的配对文档 blob，推导出发生冲突的 `.i18n.yaml` 记录。配对文档发生冲突、存在非文本合并配置或记录无效时，它会拒绝处理并保留冲突；如果合并已经因冲突而停止，请运行 `pnpm run resolve-translation-pairing-conflicts`，该命令会暂存每份可安全生成的配对记录；如果其他配对冲突仍需手工处理，则以非零状态退出。[双语文档约定](i18n/README.zh.md#the-pairing-contract)列出该驱动接受的确切文件和状态。

安装脚本在发布 worktree 配置前，会探测确切的 Node/tsx 驱动入口点。如果该运行时之后变得不可用，不依赖 Node 的启动器会写入 Git 的普通文本合并结果、让伴随文件保持未解决状态，并打印恢复路径；请恢复依赖后运行 `pnpm run resolve-translation-pairing-conflicts`，或运行 `git merge --abort`。如果 `pre-merge-commit` 拒绝原本能干净完成的合并，Git 会把完整结果留在暂存区但不创建提交；请修复失败后运行 `git commit`，或中止合并。确切的索引与 `MERGE_HEAD` 状态由[自动配对合并 Agent Note](../.agents/notes/implemented/process/2026-08-08-automatic-translation-pairing-merges.zh.md#failure-contract)负责记录。

lefthook 在 `lefthook.yml` 中配置，作为快速的本地检查点：

- `pre-commit` 对照暂存的配对文档 blob 校验暂存的配对记录，使用不加载项目的 `.oxlintrc.staged.json` 配置验证暂存文件，并通过一次有界重试应用 Oxlint 修复，在暂存文件属于 `THIRD_PARTY_NOTICES.md` 的输入时重新生成该文件，然后检查暂存 diff 中的空白错误，并运行 vendor manifest（元数据清单）守卫；
- `pre-merge-commit` 在 Git 创建自动合并提交前执行同样以索引为准的配对检查；
- `pre-push` 运行 `pnpm run typecheck`；该命令会先完成包含 Typert 约定生成的完整 Host lib 阶段，再运行 Client TypeScript 检查。

vendor manifest 守卫检查 `vendor/*/src` 下的改动是否连同对应的 `vendor/README.md` manifest 更新一起暂存。请在编辑 vendor 代码前先阅读 `vendor/README.md`。

除限定范围的暂存记录校验外，这些钩子有意不运行测试、快照、文档检查、构建或 `hygiene`。贡献者只运行一次[与改动行为相关的检查](../AGENTS.md#run-relevant-checks-locally)；CI 负责全量覆盖率门禁、构建产物冒烟测试，以及 Node 22.19、24 和 26 兼容性矩阵。

贡献者可以选择运行 `pnpm run check:all`，执行全面的本地门禁集。该命令独立于 Git 钩子，也不是对 agent 的指令。

### CI 门禁

keyless [CI 工作流](../.github/workflows/ci.yml) 将独立门禁分组到若干宽粒度 lane，并在受支持的 Node 版本上运行一组较小的兼容性检查。产物消费方在各自 lane 内等待一次 build。必需 benchmark 在标准 GitHub 托管 Linux 上独立运行；[benchmark 运行器决策](../.agents/notes/implemented/testing/2026-09-06-standard-hosted-benchmark-runner.zh.md)拥有路由及 job 超时。单独的真实 API 工作流按其配置的 worker 上限运行 `pnpm run test:e2e`。当前门禁和 job 清单以 [scripts/run-gates.ts](../scripts/run-gates.ts) 和工作流文件为准。

不带凭据的 dsh 依赖布局检查与 dsh/vendor 打包演练仅在 `DSH_CI_FAILOVER_LINUX=selfhosted`，且事件为受信任的 master 推送或同仓库、非 fork、非 Dependabot 拉取请求时使用现有 Linux 自托管池。其余情况（包括手动触发）均使用 `ubuntu-24.04`；手动发布仍使用托管运行器。持久化存储隔离与回退限制见[发布演练运行器决策](../.agents/notes/implemented/process/2026-09-06-release-rehearsal-selfhosted.zh.md)。

### 日常命令

根目录的[贡献者说明](../AGENTS.md#commands)概述常用命令，[`package.json`](../package.json) 与 [scripts/run-gates.ts](../scripts/run-gates.ts) 则负责当前脚本和门禁清单。请选择覆盖变更表面的最小检查集。文档变更使用 `pnpm run doc-sync`；包公开行为变更还需更新所属 README 或 JSDoc，而基于构建产物的检查需要先运行 `pnpm run build`。

### Profile 运行

从源码 checkout 运行这些演示前，请单独执行仓库构建：

```sh
pnpm run build
```

单次运行的 Headless coding agent 需要环境变量或仓库根目录 `.env` 中的 `DEEPSEEK_API_KEY`：

```sh
pnpm dsh --profile headless "summarize this workspace"
```

PTC mode 演示启用代码式工具展示，并运行同一个 headless profile：

```sh
pnpm run demo:ptc -- "summarize this workspace"
```

### TODO 标记

请使用以下三种注释标签之一标记代码中的已知问题，按紧急程度排序：

- `FIXME`：应当阻塞新版本发布的问题。除非评审者明确同意该更改可以合并，否则发布版本不应包含未解决的 `FIXME`；
- `TODO`：应当尽快修复的问题，等资源到位即可处理；
- `XXX`：也许某天会修复的问题，优先级最低，不作承诺。

请选择与紧急程度匹配的标签，让浏览代码的人一眼分清「发布阻塞」和「有空再说」。

<a id="documenting-types-verbatim-ts-type-equiv"></a>

### 逐字记录类型定义（`ts type-equiv`）

[子系统](subsystems/README.zh.md)页面会把与源码等价的声明及其原始 JSDoc 一并粘贴，让读者看到确切类型定义和源码约定。为防止粘贴内容在源码变化时漂移，请将其围栏为 ` ```ts type-equiv `（而不是 ` ```ts `），并在 `scripts/type-equiv.manifest.json` 中登记它镜像的源文件和符号：

```json
{ "doc": "docs/subsystems/session.md", "symbol": "SessionEvent", "source": "packages/core/session/src/types.ts" }
```

`pnpm run verify-type-equiv`（`doc-sync` 的一环）随后通过 TypeScript 解析器从源码提取该符号的声明及其附带的 JSDoc，并断言代码块同时匹配两者。对于不应把实现体写进目录的类，请使用 ` ```ts public-api ` 并设置 `"projection": "public-api"`；门禁检查的投影会保留公共字段、构造函数、访问器、方法以及类和成员的原始 JSDoc，同时省略实现体和私有或受保护成员。比对会忽略空白和非 JSDoc 注释，但要求保留每条原始 JSDoc（包括成员文档），让读者同时看到源码约定和确切类型定义。该门禁按文档、符号和投影，在主块与 manifest 条目之间强制 1:1 对应；只有当配对 `.zh.md` 块的完整受跟踪围栏序列与其无后缀兄弟文件按字节一致且顺序相同时，才会复用后者的条目。`doc-typecheck` 对可编译围栏应用同一派生规则，同时跳过两种源码等价围栏的编译，并将其排除在 opt-out 比例的计算之外。当你改动一个已记录的类型声明或其 JSDoc 时，门禁会失败直到你更新粘贴内容；当你增删一个主块时，请在同一个变更里更新 manifest。
