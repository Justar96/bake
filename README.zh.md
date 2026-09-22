# Bake

[English](README.md) | 中文

Bake 是一个终端编程 Agent，支持流式响应、工具执行、持久会话和键盘交互。它使用 Ink 渲染界面，并采用源自 DeepSeek Harness 的 Cordis 插件运行时。

Bake 是独立项目。上游发布由维护者评估后选择性采用；Bake 不会自动跟踪或合并上游分支。

## 从源码运行

安装 `package.json` 中固定版本的 Bun、Node 24 或更新版本，以及 C/C++ 编译器和 Node 开发头文件。当前终端检查运行于 Linux 和 macOS。

```sh
bun install --frozen-lockfile
bun run build
bun run start
```

`start` 运行现有构建产物，默认将 Bake 数据保存在 `~/.bake`，可通过 `DSH_HOME` 覆盖。上游的 `~/.dsh` 数据不会被修改。在终端中使用 `/login`，或在环境中设置 `DEEPSEEK_API_KEY`。不要提交密钥或 `.env`。`bun run start --help` 显示终端应用的选项。

Bun 管理工作区和锁文件。构建后的 Agent 使用 Node 运行，因为启动加载器依赖 Node 内部功能。外部 profile 插件安装使用运行时独立的包管理配置。

## 开发

```sh
bun run dev                  # 热重载组件预览；不启动 Agent，不需要模型密钥
bun run dev:tui              # 重新构建并运行完整 Node Agent
bun run check                # 类型、测试、渲染器实例、布局和文档
bun run test:e2e              # 真实终端回放；需要已有运行时构建
bun run verify               # 构建、检查和无密钥终端场景
```

[开发指南](CONTRIBUTING.zh.md#选择开发流程) 介绍重新构建命令、场景筛选和故障排查。`bun tui/scripts/tui.ts help` 列出独立测试目标、录制和性能诊断命令。

## 仓库结构

- [`tui/`](tui/DESIGN.md)：终端应用、Ink 组件、测试数据和开发工具。
- [`apps/cli/`](apps/cli/README.zh.md)：启动 `tui` 和 `headless` profile 的 Node 启动器。
- [`packages/`](packages/README.zh.md)：共享 Agent、会话、模型、工具、沙箱和插件服务。
- [`native/`](native/README.zh.md) 和 [`vendor/`](vendor/README.md)：原生支持和固定版本的 Cordis 源码。
- [`snapshots/`](snapshots/AGENTS.md)：录制的会话验证数据，包括保留的历史代次。

[`CONTRIBUTING.md`](CONTRIBUTING.zh.md) 介绍开发与上游发布评估。TUI 的[限制](tui/DESIGN.md#10-limits) 与设计一同记录。

## 许可证

MIT。Bake 保留原始版权声明和 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)。共享运行时包继续使用 `@deepseek-ai/*` 名称，但这不代表 Bake 是 DeepSeek 官方产品。
