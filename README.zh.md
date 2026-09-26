# Bake

[English](README.md) | 中文

Bake 是一个完全用键盘操作的终端编程 Agent。它随模型输出流式显示回答，在沙箱中运行 shell 与文件工具，并把每个会话保存为可恢复的追加式日志。

- **斜杠命令**：切换模型、登录、会话、目标、计划模式、上下文压缩与权限，并支持参数补全。
- **长时任务**：跨轮次持续推进的目标、任务清单、子 Agent 与并行工作流。
- **沙箱工具**：bash 或 PowerShell、文件读取与编辑、搜索和网页抓取，均受权限预设约束。
- **可保留的会话**：按 id 恢复或从列表中选择；上下文填满时自动压缩。
- **技能与附件**：以 `/name` 调用项目或用户技能，并为提示附加图片和文件。

Bake 构建于 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) Agent 运行时之上。参见[致谢](#acknowledgements)。

## 安装

Bake 需要 `PATH` 中有 **Node.js 24 或更新版本**。发行版覆盖 macOS（arm64、x64）、Linux（arm64、x64）和 Windows（x64）。

**macOS 与 Linux**

```sh
curl -fsSL https://bake.justar.dev/install.sh | sh
```

**Windows（PowerShell）**

```powershell
irm https://bake.justar.dev/install.ps1 | iex
```

安装器在安装任何内容之前，会校验发行清单的 Ed25519 签名和归档的 SHA-256。

| 平台 | 安装位置 | `bake` 命令 | 依赖 |
|---|---|---|---|
| macOS、Linux | `~/.local/share/bake` | 链接到 `~/.local/bin`（如有提示，请将其加入 `PATH`） | `curl`、`tar`、`shasum` 或 `sha256sum` |
| Windows | `%LOCALAPPDATA%\Bake` | `bin` 加入用户 `PATH` 和当前会话 | `tar.exe` |

<details>
<summary>安装选项</summary>

| 变量 | 作用 |
|---|---|
| `BAKE_INSTALL_ROOT` | 安装目录，替代 `~/.local/share/bake` 或 `%LOCALAPPDATA%\Bake` |
| `BAKE_BIN_DIR` | `bake` 命令所在目录，替代 `~/.local/bin` 或 `<安装目录>\bin` |
| `BAKE_RELEASE_BASE_URL` | 发行主机；设为 `https://github.com/Justar96/bake/releases/latest/download` 即从 GitHub Releases 安装 |
| `BAKE_SKIP_PATH_UPDATE=1` | Windows：不修改用户 `PATH` |

[发行清单](https://bake.justar.dev/latest.json)列出当前版本和平台。

</details>

## 开始使用

```sh
bake
```

使用 `/login` 登录，或在启动前导出 `DEEPSEEK_API_KEY`。输入 `/` 浏览命令，`/help` 查看列表，`@` 引用文件。`bake --help` 显示启动选项，例如 `--resume <id>`。

Bake 把会话、凭据和配置保存在 `~/.bake`。设置 `DSH_HOME` 可改用其他目录。Bake 从不读取或迁移 DeepSeek Harness 的 `~/.dsh`。

## 更新

```sh
bake update
```

`bake update` 下载最新发行版，以与安装器相同的方式校验，并在新版本成功启动后才切换。旧版本保留在磁盘上。`bake update --check` 只报告状态：已是最新时退出码为 0，有新版本时为 10，失败时为 1。状态栏也会提示可用更新：每小时检查一次，检查失败后十分钟再试；在终端中运行 `/update` 即可在不离开会话的情况下安装。设置 `BAKE_NO_UPDATE_CHECK=1` 可关闭该检查。

0.1.0 的安装没有 `bake update`；请再运行一次安装器，以迁移到可更新的布局。

## 卸载

删除安装目录和 `bake` 链接：`~/.local/share/bake` 与 `~/.local/bin/bake`，Windows 上为 `%LOCALAPPDATA%\Bake`。同时删除 `~/.bake` 即可移除会话和已保存的凭据。

## 文档

- [安全](SAFETY.zh.md)：沙箱能保护什么、不能保护什么。
- [终端应用](apps/tui/packages/app/README.zh.md)：命令、按键与会话导航。
- [TUI 设计](apps/tui/DESIGN.md)，包括其[已知限制](apps/tui/DESIGN.md#10-limits)。
- [开发指南](docs/development.zh.md)：从源码构建、测试循环与仓库结构。
- [贡献指南](CONTRIBUTING.zh.md)与[更新日志](CHANGELOG.md)。

<a id="acknowledgements"></a>

## 致谢

Bake 构建于 DeepSeek 以 MIT 许可发布的 Agent harness——[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness)——之上。它的 Agent 循环、会话日志、沙箱、工具和 Cordis 插件运行时是 Bake 运行的基础，我们感谢它的作者。Bake 在此之上增加了自己的终端界面、分发方式和改动；上游修复经评估后选择性移植，从不自动合并。

Bake 是独立项目，不是 DeepSeek 官方产品，也未获得 DeepSeek 的背书。“DeepSeek Harness”是 DeepSeek 的商标；参见其[品牌使用指南](BRAND_GUIDELINES.zh.md)。共享运行时包保留原有的 `@deepseek-ai/*` 名称，以便继续移植上游修复。

## 许可

[MIT](LICENSE)。Bake 保留原始版权声明，以及 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) 中的上游署名。
