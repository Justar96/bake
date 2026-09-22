# Bake 架构

[English](architecture.md) | 中文

Bake 是基于 Cordis 插件运行时的终端应用。修改共享包前应阅读本概览；[Cordis 入门](cordis-primer.zh.md) 介绍服务注入、类型化事件和可撤销副作用。

## 应用启动

Bun 管理源码工作区并构建应用。受支持的 Node 应用通过 `dsh` profile 启动。随附的 profile 包括用于交互终端会话的 `tui`，以及用于单个任务的 `headless`。自定义组合仍使用 profile 和有序补丁，而不是新增可执行文件或内联应用树。

终端 profile 依次加载 [`dsh-base`](../packages/bundle/base/README.zh.md) 和 [`@dsh-tui/app`](../tui/packages/app/README.zh.md)。Base 提供模型、工具、持久化、沙箱策略、设置和凭据；TUI 提供 Agent 选择、终端所有权、用户输入和呈现。[启动器](../apps/cli/README.zh.md) 负责 profile 初始化和参数转发。

每个 profile 在 `dsh.profile.bundles` 中列出组合包。配置依次应用组合包补丁、profile 补丁、home 补丁和调用补丁。YAML 控制 HMR。现有 profile 清单保留用户选择的组合包。

## 共享运行时

| 所属包 | 职责 |
|---|---|
| [`core/session`](../packages/core/session/README.zh.md) | 仅追加的会话事件和模型历史投影 |
| [`core/agent`](../packages/core/agent/README.zh.md) | Agent 句柄、注册表和类型化生命周期事件 |
| [`core/agent-loop`](../packages/core/agent-loop/README.zh.md) | 回合执行与工具调度 |
| [`core/tools`](../packages/core/tools/README.zh.md) | 作用域内的工具注册与执行 |
| [`core/system-prompt`](../packages/core/system-prompt/README.zh.md) | 提示词与工具 schema 组装 |
| [`llm/llm`](../packages/llm/llm/README.zh.md) | 提供器无关的模型请求与流 |
| [`session/session-projection`](../packages/session/session-projection/README.zh.md) | 增量且权威的会话状态 |
| [`boot/app-boot`](../packages/boot/app-boot/README.zh.md) | Profile 加载、Node 初始化和启动失败处理 |

插件通过副作用注册服务和监听器。能力由服务定义、提供器和消费者组成，各角色须保持完整。应通过所属插件或类型化事件增加行为，而非为单个工具修改循环。

## 事件与回合

会话事件记录持久事实；Agent 事件描述活动中的工作；能力事件连接提供器、工具和策略。Waterfall 监听器委托处理时必须调用 `next()`，直接返回会终止处理链。

回合领取排队输入、组装请求、运行模型步骤、执行工具，并在仍有工作时重复。`agent/pre-step` 可以拒绝或重写已领取输入。请求准备在提交系统与用户消息前确定路由；准入前取消不会提交两者。[Agent-loop README](../packages/core/agent-loop/README.zh.md) 定义顺序、重试、取消和拆卸细节。

Assistant 流在执行期间实时可见。完成的消息和已结束的失败尝试将紧凑流数据保存在会话日志中。TUI 渲染已提交历史和瞬时流状态，不组装模型请求，也不维护独立消息历史。

## 会话与持久化

所有模型可见内容必须能从会话日志重建。新的模型可见输入需要持久事件。会话消费者使用当前逻辑格式；JSONL 提供器选择存储代次，并在返回事件前执行受支持的相邻迁移。

不得移动、覆盖或删除已提交代次。迁移写入会在不改变前序文件的情况下发布带版本名称的后继。未来或不支持的格式明确失败，而非静默回退。[会话格式状态](session-format-status.zh.md) 与 [JSONL 持久化](../packages/session/session-persistence-jsonl/README.zh.md) 定义数据规则。

TUI 从 `agent.status` 读取活动状态，从 inbox 投影读取待处理输入，从运行时 context-pressure 投影读取上下文用量。不得从回合事件推断这些事实，也不得维护竞争性 reducer。参见 [TUI 接线](../tui/DESIGN.md)。

## 终端所有权

应用运行器持有一个显示中的会话和终端释放路径。Ink 管理原始模式、括号粘贴和光标恢复。正常退出、Cordis 拆卸和致命启动失败都会释放终端资源。拆卸等待所持有的工作结束。

实时区域保持在终端行数预算内；已提交的转录行保留在滚动历史中。纯 Ink 组件接收属性和本地化文本。Agent 访问、文件系统读取等副作用留在应用包中。[布局设计](../tui/DESIGN-LAYOUT.md) 定义终端几何和渲染规则。

## 采用上游变更

Bake 保留共享运行时包名称和会话验证数据，以支持选择性移植上游修复。[贡献指南](../CONTRIBUTING.zh.md#上游发布) 定义发布评估流程。上游的 Web、Desktop、SDK 和发布工作流不是 Bake 应用的要求。
