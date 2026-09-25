# 为 Bake 做贡献

[English](CONTRIBUTING.md) | 中文

感谢你帮助改进 Bake。[开发指南](docs/development.zh.md)介绍从源码构建、开发循环、检查和仓库结构；[`AGENTS.md`](AGENTS.md) 列出每个变更都要遵循的工程规则。

## 提交变更

1. 从 `develop` 创建分支，每个变更只聚焦一种行为。
2. 随代码一起更新所属的 README 或 JSDoc，并保持中英文页面一致。TUI 中显示的产品文案放在 [`copy.ts`](apps/tui/packages/ui/src/copy.ts) 中，同时提供两种语言。
3. 运行覆盖本次变更的检查（参见[检查](docs/development.zh.md#checks)）。终端行为变更还需要运行 PTY 场景。如实报告运行了什么，包括失败和跳过的项目。
4. 向 `develop` 发起拉取请求，说明行为变化以及你的验证方式。

切勿提交凭据或 `.env`，切勿覆盖 `snapshots/` 下已录制的会话代，也不要绕过 Git hook。检查失败时请修复根因，而不是刷新所有快照。

<a id="upstream-deepseek-harness"></a>

## 上游 DeepSeek Harness

Bake 构建于 [DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness) 之上。检出目录可以把它添加为只读的 `upstream` 远程；`origin` 是 Bake。

- 评估上游发布并选择性移植修复，同时带上其测试和许可声明。切勿自动从 `upstream` 合并或向其推送，也不要代表 Bake 向上游发起拉取请求。
- 保留共享运行时的 `@deepseek-ai/*` 包名，使移植的修复能够干净地应用。
- 精简依赖前，请检查包导入、TypeScript 引用和配置 YAML。采纳持久化变更前，请审阅会话格式迁移。
- 移植修复后，重新构建受影响的运行时包，并重新运行其针对性测试和终端场景。

目前没有自动的上游发布监控；看到某个发布并不意味着可以应用它。

Bake 的问题请报告到 [Bake 的问题跟踪器](https://github.com/Justar96/bake/issues)，而不是 DeepSeek Harness，除非你已在上游本身复现。
