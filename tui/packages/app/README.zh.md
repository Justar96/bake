---
description: "交互式终端配置，支持持久会话恢复、按代理限定的人工决策和 Harness 命令。"
kind: "package-bundle"
---

# @dsh-tui/app

[English](README.md) | 中文

## 概要

此私有工作区包为基于 base 的 `dsh` 配置添加终端运行器和命令行参数提供器。它记录会话组成，恢复指定的持久会话，并通过 Harness 服务处理人工输入。运行时为 Node，终端模式由 Ink 管理。此包在当前代码库内开发，不单独发布。

## 目录

- [使用此包](#use-this-package)

- [了解实现](#understand-the-implementation)

- [模型体验](#model-experience)

- [已知限制](#known-limitations)

<a id="use-this-package"></a>

## 使用此包

在仓库根目录构建本地包，并使用已准备的 `tui` 配置启动。配置必须包含 `dsh-base`；提供的补丁会禁用 headless 运行器并挂载 standard 预设。

```sh

./tui/scripts/build.sh

node apps/cli/lib/bin.js --profile tui --patch ./tui/packages/app/cordis.built.patch.yml

```

Enter 提交输入；运行期间输入会引导下一步。Escape 取消当前问题、活动命令或运行中的代理。按两次 Ctrl-C 退出。粘贴只插入文字，不会提交。`/login` 列出登录目标；`/login <target>` 打开掩码输入或授权流程。其他斜杠命令使用组合后的命令注册表。

请在会话记录的工作目录中使用 `--resume <id>`。已保存的预设为唯一依据；冲突的 `--preset` 会被拒绝。未记录预设的旧会话必须显式指定 `--preset`。未知 ID 不会创建替代会话。新建空会话遵循 Harness 的持久化策略，退出后不一定保留。

运行器的[配置表](../../DESIGN.md#8-configuration)包含语言、退出间隔和凭据引用。[无密钥 PTY 检查](../../scripts/pty-smoke.py)会创建独立的临时配置和工作区，无需修改用户配置。

<a id="understand-the-implementation"></a>

## 了解实现

<details>

<summary>会话和终端所有权</summary>

`session.ts` 负责注册表设置和记录的会话组成。`controller.ts` 合并查询历史与实时事件，并分发命令。`interactions.ts` 排队处理限定范围的请求，在中止或释放时结束请求。`runner.ts` 在释放代理句柄和等待命令结束之前同步释放 Ink。启动器的致命错误释放钩子会释放同一个 Cordis effect。

此包不声明新的持久化类型或运行时不变量：会话存储、收件箱状态和代理生命周期仍由对应的 Harness 服务管理。参见[连接说明](../../DESIGN.md)和[依赖文档](../../DEPENDENCIES.md)。

</details>

<a id="model-experience"></a>

## 模型体验

普通人工输入成为已记录的用户消息。命令通过 Harness 注册表负责其所有模型可见影响。批准和问题答案返回请求它们的服务；登录密钥不会进入模型输入或会话历史。

### KV Cache 影响

TUI 不构建模型请求，也不修改缓存设置。

<a id="known-limitations"></a>

## 已知限制与后续工作

- 每个进程只有一个根会话；不提供交互式会话或模型选择器。

- 工具执行验证使用构建后的配置；源码启动的模块重复问题见[运行手册](../../PLAN.md#132-build-from-a-clean-checkout)。

- 上下文压缩后，终端回滚区保留原始输出。

### 开发备注

无。
