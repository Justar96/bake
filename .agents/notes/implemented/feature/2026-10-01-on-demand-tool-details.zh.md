# Agent Note：按需读取的工具详情

Status: implemented

[English](2026-10-01-on-demand-tool-details.md) | 中文

## 问题

每次请求都会重新发送每个可见工具的原生 schema。在 standard preset 的首次请求中，21 KB 的请求体里约 20 KB 是工具 schema，提示词和首条消息不到 1 KB。提示词缓存只在部分路由上掩盖这项成本：Anthropic 有 96% 到 98% 的输入来自缓存，OpenAI 约 65%，而经由随附网关的 Gemini 几乎没有缓存。最大的 schema 是 `workflow`，共 3.7 KB，其中大部分是脚本 API 参考。75 个会话的审计中没有任何会话调用过 `workflow`，因此每次请求都在为几乎无人使用的参考付费。

## 决策

`ToolDefinition` 新增可选的 `details` 字符串：一份不放进原生 schema 的用法参考。只要有可见工具声明了 `details`，注册表就会加入它保留的 `tool_help` 工具（`{ name }`），该工具返回调用方作用域可见的某个工具的 details。工具描述会提示模型在首次使用前调用 `tool_help`。在 `ptc` 模式下，SDK 本身就是提示词文本，因此 binding 的 details 会并入其文档，`tool_help` 也不是 binding。

`workflow` 把脚本 API 参考移入 `details`，其描述保留仅限明确请求的使用策略，以及先读取该参考的提示。它的 schema 从 3,757 字节降到 2,041 字节，`tool_help` 增加 290 字节。

`tool_help` 与 `run_code` 一样是保留名称：注册和 `restrict()` 都会拒绝该名称。读取工具跟随它所服务的工具：如果某个作用域的过滤器隐藏了所有带 details 的工具，该作用域也会失去读取工具。

## 为什么工具列表保持静态

读取工具是否出现只取决于哪些工具可见，因此它只在工具集合变化时才变化。加载参考只会向历史追加一次工具结果，不改变任何 schema，因此缓存的前缀得以保留。如果延迟加载的工具要等模型请求后才加入请求，工具块就会在会话中途改变。在 Anthropic 和 OpenAI 的缓存顺序中工具位于最前，因此这会使整个缓存前缀失效，而没有工具更新通道的路由还会开始新的请求序列。

## 曾考虑的替代方案

**用技能承载参考。** 技能目录和 `skill` 工具同样会随请求重发，而且技能按任务被发现，不与某个工具绑定。隐藏该工具后，还会留下一个描述智能体无法调用的 API 的技能。

**空的 `workflow` 脚本返回指南。** 这只适用于一个工具，而且会让失败或无意义的调用承载不同的含义。

**延迟 schema：只发送名称和一行摘要，按请求加载 schema。** 这样节省更多，但会在会话中途改变工具列表，如上所述会破坏缓存。

**只缩短描述。** 对 `list_agents`、`interrupt_agent` 和 `subagent_fork` 的原地精简约节省 580 字节。`workflow` 的参考无法这样缩短而不丢失约定。

## 后果

计入移除规划模式（`exit_plan_mode`，501 字节）后，standard preset 的原生工具 schema 每次请求约减少 2.4 KB。编写工作流脚本的模型要多花一次工具调用读取参考；跳过 `tool_help` 的模型只能依据参数描述编写脚本，这时 `workflow` 会以引擎自身的错误失败，错误会指出出错的钩子或选项。带有长篇参考的新工具应使用 `details`，而不是更长的描述。

## 验证

`packages/core/tools/tests/tool-help.spec.ts` 覆盖不含 details 的原生 schema、读取工具的可见性、执行、作用域视图与受限视图、保留名称，以及 PTC 模式的 SDK。`packages/workflow/tool-workflow/tests/tool-workflow.spec.ts` 固定缩短后的描述和 details。`packages/core/tools/tests/gen-tool-catalog.spec.ts` 显示 `tool_help` 只出现一次，位于注册表条目下。

一项配对真实评估在三个模型上比较了上一版构建与本构建，覆盖七个编辑任务和一个工作流任务，每项三次试验。首次请求缩小了 2,477 字节。在编辑任务上，总 token 在 Sonnet 上下降 10.3%，在 Gemini 上下降 13.8%，在 GPT 上下降 11.0%，请求次数不变。在工作流任务上，每个模型都在编写第一个脚本前调用了 `tool_help`，9 次运行中 9 次如此。该任务在 Sonnet 上多花 28% 的 token，原因是多出的一次请求以及其后一直携带的参考。Gemini 和 GPT 同样更贵，但只有两组匹配对，区间很宽。只要编写工作流脚本的会话仍然很少，这一取舍就有利于 details。
