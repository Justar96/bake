# Bake

[English](README.md) | 中文

Bake 是一个终端编程 Agent。它流式输出模型响应，在沙箱中执行工具，把每个会话持久化为可恢复的追加式日志，并完全通过键盘操作。界面基于 Ink 构建；Agent 运行在源自 DeepSeek Harness 的 Cordis 插件运行时上。

Bake 是独立项目。上游发布经评估后选择性移植；Bake 从不自动跟踪或合并上游分支。

## 安装

Bake 需要 `PATH` 中有 Node.js 24 或更新版本。发行归档覆盖 macOS（arm64、x64）、Linux（arm64、x64）和 Windows（x64）；[发行清单](https://bake.justar.dev/latest.json)列出当前版本和平台。

```sh
curl -fsSL https://bake.justar.dev/install.sh | sh
```

```powershell
irm https://bake.justar.dev/install.ps1 | iex
```

安装脚本在安装任何内容之前，先验证清单的 Ed25519 签名和归档的 SHA-256。在 macOS 和 Linux 上，它把发行版解压到 `~/.local/share/bake`，并把 `bake` 链接到 `~/.local/bin`；如果安装脚本提示，请把该目录加入 `PATH`。在 Windows 上，它安装到 `%LOCALAPPDATA%\Bake`，并把其 `bin` 目录加入用户 `PATH` 和当前 PowerShell 会话。Unix 安装脚本需要 `curl`、`tar`，以及 `shasum` 或 `sha256sum`；Windows 安装脚本需要 `tar.exe`。

运行 `bake` 开始会话。使用 `/login` 登录，或在启动前导出 `DEEPSEEK_API_KEY`。Bake 把会话、凭据和 profile 保存在 `~/.bake`；设置 `DSH_HOME` 可改用其他目录。它从不读取或迁移上游的 `~/.dsh`。`bake --help` 列出终端选项，例如 `--resume <id>`。

### 更新与卸载

`bake update` 下载最新发行版，以与安装脚本相同的方式验证，并且只在新版本能够启动后才切换过去。旧版本保留在磁盘上。`bake update --check` 只报告状态：已是最新时退出码为 0，有更新版本时为 10，失败时为 1。状态栏也会提示可用更新；设置 `BAKE_NO_UPDATE_CHECK=1` 可关闭该检查。0.1.0 安装没有 `bake update` 命令；重新运行一次安装脚本即可切换到可更新的布局。

| 变量 | 作用 |
|---|---|
| `BAKE_INSTALL_ROOT` | 安装目录，替代 `~/.local/share/bake` 或 `%LOCALAPPDATA%\Bake` |
| `BAKE_BIN_DIR` | `bake` 命令所在目录，替代 `~/.local/bin` 或 `<安装目录>\bin` |
| `BAKE_RELEASE_BASE_URL` | 发行主机；`https://github.com/Justar96/bake/releases/latest/download` 从 GitHub 发行版安装 |
| `BAKE_SKIP_PATH_UPDATE=1` | Windows：不修改用户 `PATH` |

卸载时，删除安装目录和 `bake` 链接（`~/.local/share/bake` 与 `~/.local/bin/bake`，或 `%LOCALAPPDATA%\Bake`）。如需同时删除会话和已保存的凭据，再删除 `~/.bake`。

## 从源码运行

安装 `package.json` 中固定版本的 Bun、Node 24 或更新版本，以及用于原生模块的 C/C++ 工具链和 Node 头文件。终端测试套件运行于 Linux 和 macOS。

```sh
bun install --frozen-lockfile
bun run build
bun run start
```

`start` 运行现有构建产物，使用与已安装 `bake` 相同的 `~/.bake` 主目录和凭据。不要提交密钥或 `.env`。Bun 管理工作区、锁文件和构建；Agent 本身在 Node 上运行，因为其启动加载器依赖 Bun 引擎所没有的 V8 内部功能。

## 开发

```sh
bun run dev                  # 热重载组件预览；不启动 Agent，不需要模型密钥
bun run dev:tui              # 重新构建并运行完整 Node Agent
bun run check                # 类型、测试、渲染器实例、布局和文档
bun run test:e2e              # 真实终端回放；需要已有运行时构建
bun run verify               # 构建、检查和无密钥终端场景
```

[开发指南](CONTRIBUTING.zh.md#选择开发流程)介绍重新构建流程、场景筛选和故障排查；`bun apps/tui/scripts/tui.ts help` 列出独立测试目标、录制和性能诊断命令。[发行指南](distribution/README.zh.md)介绍发行版的构建、签名和发布。

## 仓库结构

- [`apps/tui/`](apps/tui/DESIGN.md)：终端应用、Ink 组件、测试数据和开发工具。
- [`apps/cli/`](apps/cli/README.zh.md)：启动 `tui` 和 `headless` profile 的 Node 启动器。
- [`packages/`](packages/README.zh.md)：共享 Agent、会话、模型、工具、沙箱和插件服务。
- [`native/`](native/README.zh.md) 和 [`vendor/`](vendor/README.md)：原生支持和固定版本的 Cordis 源码。
- [`snapshots/`](snapshots/AGENTS.md)：录制的会话验证数据，包括保留的历史代次。

[`CONTRIBUTING.md`](CONTRIBUTING.zh.md) 介绍开发与上游发布评估流程。TUI 的[限制](apps/tui/DESIGN.md#10-limits)与设计一同记录。

## 许可证

MIT。Bake 保留原始版权声明和 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)。共享运行时包继续使用 `@deepseek-ai/*` 名称，但这不代表 Bake 是 DeepSeek 官方产品。
