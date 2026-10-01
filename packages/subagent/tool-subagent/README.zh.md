---
description: "面向模型的 subagent 委派工具，供用户与维护者配置、组合或排查基于 subagent 提供方的委派。"
kind: "package-reference"
---

# @deepseek-ai/dsh-tool-subagent

[English](README.md) | 中文

## 概述

使用本包可为 agent 提供一个具名工具，把工作委派给已配置的子 agent 后端。`one-shot` 模式下，调用默认等待子 agent；`continuable` 模式下，调用默认在后台启动持久化子 agent，并返回可用于后续消息的 id。受支持的后端还可公开获准的子级 LLM 提供方、模型与推理等级供模型选择。每个实例均可设置子 agent 的 persona、工具权限与深度限制，失败的运行会返回错误，而非部分成功。

## 目录

- [使用本包](#use-this-package)
- [理解实现](#understand-the-implementation)
- [进一步探索](#further-exploration)
- [模型体验](#model-experience)
- [已知限制与延期工作](#known-limitations-and-deferred-work)
- [开发备注](#dev-note)

-----

<a id="use-this-package"></a>
## 使用本包

每个委派目标挂载一个实例，且每个实例的 `toolName` 必须不同。工具与其提供方同时存在、同时消失，因此同级加载顺序与提供方重新加载都不会让工具悬空。

### 最小配置

先加载 subagent 服务、一个进程内或远程后端与本工具，然后指定提供方名称。此组合暴露一个委派给 `spawn` 后端的 `subagent` 工具：

```yaml
- name: '@deepseek-ai/dsh-subagent'
- name: '@deepseek-ai/dsh-subagent-spawn-in-process'
- name: '@deepseek-ai/dsh-tool-subagent'
  config:
    provider: spawn
    toolName: subagent
```

| 字段 | 默认值 | 含义 |
|---|---|---|
| `provider` | 必填 | `ctx.subagents` 上的提供方名称（如 `spawn`、`fork`、`acp`） |
| `toolName` | `subagent` | 面向模型的工具名称；每个已加载实例必须不同 |
| `modelSelectionSettings` | `false` | 为每个顶层 Session 读取宿主的精确路由授权偏好；常驻组合观察其 Agent（preset 自己的 Agent，或无作用域挂载时的所有 Agent），直接 Agent setup 则显式传入其 Session；要求提供方支持 `agentOptions` |
| `enableRunInBackground` | `true` | 公开 `run_in_background`；禁用时也会拒绝强制后台调用 |
| `backgroundMode` | `one-shot` | 后台策略：`one-shot` 默认前台调用；`continuable` 默认后台调用，并要求提供方具备 `prepareContinuable` 能力 |
| `agentOptions` | — | 配置的子级 `provider`、`model`、适配器所有的 `reasoningEffort` 与正整数 `maxTokens` 默认值；要求提供方支持 `agentOptions`，并会覆盖提供方持有的路由默认值 |
| `persona` | — | 每个子 agent 独立的 persona；要求提供方具备 `persona` 能力 |
| `toolFilter` | — | 每个子 agent 独立的全局工具限制；要求提供方具备 `toolFilter` 能力 |
| `maxDepth` | Host 设置（`1`） | 绝对委派深度上限（`0` 禁止委派）；`'provider-managed'` 不向进程外提供方发送上限 |

生成的[配置目录](../../../docs/config-catalog.zh.md#deepseek-aidsh-tool-subagent)是每个受支持字段及其 JSDoc 的穷尽式真源。

### 前台与后台模式

`one-shot` 策略下，省略 `run_in_background` 会在前台等待并返回子 agent 的最终文本；`run_in_background: true` 会启动一个归父级所有的普通后台任务，并返回 `started background subagent job <id>`，可用 `job_output` 收集、用 `job_kill` 停止。

`continuable` 策略下，省略或为 `true` 的 `run_in_background` 会启动一个持久化子 agent，并返回 `started subagent <childId>`，不等待结果；子 agent 的 Activation 结束时，运行时投递一条结算通知，可选的 `send_message` 工具会向它发送更多工作。把 `run_in_background` 设为 `false` 可在前台等待结果。

`maxDepth` 限制递归深度（`0` 禁止委派）；省略时，每次委派读取 Host 当前的 `subagent.maxDepth` 设置，初始值为 `1`。数值深度要求提供方具备 `depthLimit` 能力；`'provider-managed'` 把预算留给进程外提供方。当提供方支持时，`persona` 与 `toolFilter` 会配置每个子 agent；工具在达到上限时仍然可见——每次尝试启动都会检查调用 agent 的当前深度，被拒绝时返回出错的工具结果。

### 选择子级 LLM

设置 `modelSelectionSettings: true`，即可在组合每个全新顶层 Session 时读取宿主的 `subagent-model-selection` 偏好。没有已记录策略的恢复 Session 会保持禁用，包括显式为空的恢复。启用后，非空的精确 provider/model 路由列表会记录进 Session、由子 Session 继承，后续设置编辑不会改变它。工具随后公开可选的 `provider`、`model` 与 `reasoning_effort` 字段，并注册共享的 `list_subagent_models` 工具。此模式要求后端声明 `agentOptions`；两个进程内后端和 DSH SDK 支持该能力，而 ACP、Codex 与 Claude Code 会拒绝它，而不是忽略它。

一次调用需同时提供 `provider` 与 `model`；当配置值、父 agent 值或提供方持有的默认值能提供路由时，也可只提供推理等级。静态的 `provider.agentRouteDefaults` 在存在时构成提供方／模型基线；工具配置与模型字段会在路由相关强度合并和确切路由预检前覆盖它。没有这些默认值的提供方会使用父 agent 最新已记录请求中的兼容值，再使用父级首次请求前的创建选项，并保留配置的 `maxTokens`。更改路由但未显式提供推理等级时，会清除继承的路由自有等级，使所选模型解析自己的默认值。实时 LLM 适配器在创建子 agent 前校验有效路由。目录成员资格只提供建议，因此适配器接受时，模型可以使用未列出的 id。

该设置的 `router` 是一个任务路由器，为未指定路由的调用做选择。它是测试版，在设置 `router.enabled` 之前保持关闭。开启后，这类调用会先把其描述与提示以及 Session 允许的路由发送到 `<url>/v1/bake/select`，即 [ing](https://github.com/Justar96/ing) 路由器协议；`url` 默认是托管的 ing 路由器 `https://ing.gissx.org`。超过 20,000 个字符的任务只发送其开头的八分之三和结尾，中间以 `[…]` 连接，这正是路由器自身评判的摘录方式，因为粘贴日志的委派通常把要求放在最后。路由器只负责选择；每条路由仍在用户自己的提供方上运行。每条路由都附带其模型信息，因此即使路由器不认识某个模型名称，也能对其施加约束并排序：推理等级（模型不提供等级控制时为空）、默认等级、上下文窗口和输入类型。若用户在 `router.hints` 中为该路由设置了条目，也会一并发送：`sameAs` 指出该路由提供的、路由器已知的模型，例如网关别名背后的 `claude-opus-4.5`；`quality`（`low`、`medium`、`high`、`frontier`）在缺少基准数据时代替基准；`cost`（`free`、`low`、`medium`、`high`）取代路由器的价格。设置 `router.priority`（`quality`、`cost`、`speed`、`balanced`）后，它会取代路由器从任务文本推断的取舍。请求会携带 `Authorization: Bearer`，其值为 `router.tokenEnv`（默认 `ING_API_TOKEN`）所指定的凭据，每次请求都通过 `ctx.credentials` 重新解析，因此同名的已导出环境变量优先于已存储的值；未挂载凭据存储时只读取环境变量。只有当回答指定的是允许路由之一、且未标记 `fallback`（路由器在无法区分各路由时设置）时才会采用它。若该路由的模型信息列出了回答中的推理等级，就使用该等级；否则使用所列等级中强度最接近的一个，平局时对 `low` 取较低者，对其他等级取较高者。强度未知的等级 id 会被丢弃，模型默认值随之生效。请求失败、返回非 2xx 状态、回答格式错误或超出策略，或超过 `router.timeoutMs`（默认 5000）仍未返回时，会记录警告，调用随后使用默认路由。中止委派也会中止路由请求。在 `url` 为空时开启路由会被拒绝。路由器在每次委派时读取，因此编辑它会改变运行中 Session 之后的调用，但不会改变它们已记录的路由列表。`describeRoutes` 会把同样的路由描述（不含任务）发送到 `<url>/v1/bake/routes`，即使路由处于关闭状态也会发送。它为每个允许的模型返回路由器匹配到的已评测模型、是否已评估（有质量依据），以及路由器为其打分所用的质量和混合价格。`/settings` 将其显示为“模型校准”。若路由器对未携带令牌的请求返回 `401`，调用会失败并指出应设置的环境变量名。该服务还可通过邮箱登录 ing 路由器：`requestSignInCode` 让路由器向某个地址发送一次性验证码，`signIn` 在 `<url>/auth/email/verify` 用验证码换取标记为 `bake` 的令牌，并通过 `ctx.credentials.set` 存储在 `router.tokenEnv` 名下；`signOut` 在 `<url>/auth/logout` 撤销令牌并将其删除，即使无法连接路由器也会删除。首次用某个地址登录即注册该地址。`routerTokenStatus` 说明是否已存储令牌及其来源层级，从不返回令牌本身；`routerAccount` 向 `<url>/auth/me` 查询令牌所属的地址。当环境变量提供令牌或未挂载凭据存储时，登录与退出会在发送任何内容之前被拒绝。登录被拒时，错误信息使用路由器自己的原因，例如 `wrong or expired code`。

ing 可附带可选的 `routing` 评估，包含策略版本、`normal`、`cautious`、`needs_context` 或 `fallback` 状态、归一化难度与原因。缺失或格式错误的评估元数据不会使本来有效且获准的路由失效；标记为 `fallback` 的回答仍保留现有默认路由。记录前会删除说明中的终端控制字符并限制长度：主要原因为 500 个码点，策略为 64 个码点，最多八条原因，每条 240 个码点。模型校准也保留 `quality_source: inherited`，将前代模型依据与实测基准、用户提示区分开。

子 agent 成功接纳后，工具会在直接父 Session 上记录可忽略、仅用于日志的 `subagent/routing-decision` 事件。它记录子 agent 与工具调用、选择来源（`explicit`、`default`、`auto` 或 `fallback`）、已知的有效路由，以及可选的路由器依据。未知的推理等级会省略；被拒绝的路由器建议不会作为有效路由展示。`subagentRoutingDecisions` 投影公开以子 agent id 为键的 wire 记录；`SubagentRoutingDecision` 与 `SubagentRoutingAssessment` 是供消费者使用的导出类型。重放保留说明，fork 子 agent 则排除继承的父级决定。没有此记录的旧 Session 仍然有效。这些记录不会增加模型消息、工具参数、结果或 token 用量。记录失败时仅记录警告，已接纳子 agent 的执行与所有权保持不变。

```yaml
subagent-model-selection:
  enabled: true
  allowedModels:
    - { provider: cliproxyapi, model: claude-opus-5-5 }
    - { provider: cliproxyapi, model: glm-5.3-flash }
    - { provider: ollama, model: "llama3.1:70b" }
  router:
    enabled: true
    url: https://ing.gissx.org
    tokenEnv: ING_API_TOKEN
    timeoutMs: 5000
    hints:
      - { provider: ollama, model: "llama3.1:70b", quality: medium, cost: free }
```

-----

<a id="understand-the-implementation"></a>
## 理解实现

<details>
<summary>实现细节——点击展开</summary>

本节解释工具如何镜像提供方生命周期并结算运行；可观察行为已在[使用本包](#use-this-package)中说明。

### 设计理念

一个实例就是一个提供方加一个工具名称。插件镜像提供方生命周期：具名提供方出现时注册工具，提供方离开时释放工具，因此同级加载顺序与 HMR 替换不会让工具悬空。直接 Agent setup 显式传入尚未发布的 Session，并在发布前等待安装完成。由设置控制的常驻组合通过 `agent/created` 接收其每个 Agent，从其 Session 选择策略，并等待通过其 Context 发起的安装；安装失败会拒绝创建。preset 的组合覆盖在其作用域下组合的 Agent；无作用域的组合（例如一次性 profile 的组合）覆盖进程中的所有 Agent。提供方无法执行的数值型 `maxDepth` 或已配置 LLM 选择会在挂载时失败，而不是在首次委派时失败。每个工具作用域内最多一个实例可以拥有模型选择，因为 `list_subagent_models` 使用全局名称。

### 前台结算

前台调用会等待 `run.result`，把每个非完成终止原因映射为错误标题，追加提供方诊断与任何保留下来的部分 assistant 文本，并在返回前始终等待 `run.dispose()`；当结果收集与 dispose（资源释放）都 reject 时，出错结果会保留两项失败。

### 后台路由

一次性后台模式会注册一个归父级所有的普通 Task，其 done 通道结算启动，并在 detail 中保留终止原因与可选提供方诊断。可继续后台模式调用 `ctx.subagents.startContinuable()`，该调用在 inbox 接受时结算：子 agent 自此拥有自己的轮次，因此该调用既不等待也不收集结果。

### 随上下文变化的措辞

工具描述源自 `provider.inheritsParentContext`：全新子 agent 得到「It does not see this conversation」措辞，fork 子 agent 得到「inherits this conversation's completed turns, but not the current one」措辞，因此模型既不会复述、也不会省略并不存在的上下文。工具描述是委派指引的唯一归属：插件不注册任何系统提示词 section，因此隐藏 schema 的工具限制也会同时隐藏这段指引。

### UI 呈现

每个委派工具都声明一个纯函数 `presentCall`：UI 以简短的 `description` 为调用命名；description 为空时改用 prompt 的第一行，截断为 80 个字符。可能长达多段的 prompt 不进入卡片；它是子会话的第一条消息。`list_subagent_models` 按查询内容命名：`List subagent providers`、`List <provider> models` 或 `Show <provider>/<model>`。两个工具都不声明 `presentResult`，因此完成的调用保留 UI 对结果文本的通用渲染；过时的记录参数也会让调用回退到通用渲染。

### 源码地图

| 文件 | 职责 |
|---|---|
| [`src/index.ts`](src/index.ts) | 工具注册、生命周期镜像、模式解析、结果结算 |
| [`src/presentation.ts`](src/presentation.ts) | 委派工具与发现工具的纯函数调用标题 |
| [`src/model-selection.ts`](src/model-selection.ts) | 请求／配置合并与实时 LLM 路由预检 |
| [`src/model-selection-settings.ts`](src/model-selection-settings.ts) | 为新 Session 读取的宿主所有 opt-in 设置 |
| [`src/model-selection-state.ts`](src/model-selection-state.ts) | 记录并继承已读取决定的 Session 事件 |
| [`src/auto-route.ts`](src/auto-route.ts) | 为未指定路由的调用选择允许路由的任务路由器请求 |
| [`src/routing-state.ts`](src/routing-state.ts) | 父级所有的路由决定、投影与重放过滤 |
| [`src/types.ts`](src/types.ts) | 仅用于显示的路由决定与评估类型 |
| [`src/list-models.ts`](src/list-models.ts) | `list_subagent_models` 运行时发现工具 |

</details>

-----

<a id="further-exploration"></a>
## 进一步探索

当包级约定不够用时阅读以下页面；它们从工具运行时行为进入它所委派其上的 seam，以及相邻的子 agent 工具。

- [Subagent 子系统](../../../docs/subsystems/subagent.zh.md)——提供方、一次性启动请求、可继续子 agent 与 Activation。
- [dsh-tool-subagent-control](../tool-subagent-control/README.zh.md)——可继续子 agent 的消息、中断与列表工具。
- [生成工具目录](../../../docs/tool-catalog.zh.md#deepseek-aidsh-tool-subagent)——默认 schema 与各模式的措辞。
- [生成配置目录](../../../docs/config-catalog.zh.md#deepseek-aidsh-tool-subagent)——每个受支持配置字段。
- [后台优先的可继续委派](../../../.agents/notes/archived/feature/2026-08-11-background-first-continuable-delegation.md)——可继续工作为何默认在后台运行。
- [模型选择 subagent 路由](../../../.agents/notes/implemented/feature/2026-08-18-model-selected-subagent-routes.zh.md)——选择策略、继承、发现与 fork 限制。

-----

<a id="model-experience"></a>
## 模型体验

### 工具 schema

#### 模型看到什么

当提供方存在时，以当前实例配置的名称公开已生成的默认 [`subagent` schema](../../../docs/tool-catalog.zh.md#deepseek-aidsh-tool-subagent)。启用的 Session 策略会添加 `provider`、`model` 与 `reasoning_effort`，以及继承和选择指引；提供方必须支持 `agentOptions`。提供方是否继承上下文会改变工具描述和提示词描述。启用后台模式会添加 `run_in_background`：可继续模式会记录其默认值为 `true`、返回的 agent id、完成通知、用 `send_message` 发送后续消息、在同一条消息中启动相互独立的子 agent，以及显式前台覆盖；一次性模式会记录其默认值为 `false`，以及用 `job_output` 收集或用 `job_kill` 停止的 job id。委派描述还会说明部署策略限制子级深度，因此父 agent 必须给子 agent 完整任务，不能依赖递归委派；如果因深度被拒绝，则应在此处继续任务。本包不添加任何系统提示词 section，因此这些指引全部随 schema 一起出现。使用默认工具名 `subagent` 时，可继续模式的描述为：

##### 可继续模式描述

```markdown
Delegate a self-contained task, such as research, a scoped implementation, or an analysis, to a subagent that works in its own context, so the work does not fill this conversation. You get its result, not its intermediate steps. It does not see this conversation, so give it a complete, standalone prompt. It runs in the background by default and returns its agent id right away. Start independent subagents in the same message and keep working while they run. When one finishes, you get a notice with its outcome and closing message. It stays available afterward: `send_message` steers it while it is running and otherwise starts a new turn. Set `run_in_background: false` only when your next step needs the result.
```

#### Token 影响

每个父级请求支付固定的 schema 成本；模型选择会增加三个参数。每个提供方实例增加一个 schema，不向系统提示词添加任何内容。

#### KV Cache 影响

只要提供方实例及其配置不变，前缀就保持稳定。适配器目录变化不会改变定义；子级路由覆盖可能使 fork 子 agent 无法复用继承的父级前缀。

### 模型选择与发现

#### 模型看到什么

Session 携带策略的 settings 控制实例会公开子级 LLM 选择字段与 `list_subagent_models`。可选 `ctx.llm` 服务不可用时，调用会失败。发现只返回精确路由策略中的已注册提供方与已公布模型；未授权提供方会在调用其适配器目录前被拒绝，精确查询也必须先获准，才会解析模型的推理强度与默认值。执行阶段会独立强制同一策略。两个工具都把空白的 `provider`、`model` 或 `reasoning_effort` 视为省略，因为 GPT 模型会发送每个可选字段，并把不用的留空；三者都为空白的调用会使用默认路由委派，或向路由器请求路由。被拒绝的提供方或路由会列出该 Session 允许的路由，模型无需再次查询即可修正调用。

#### Token 影响

启用的组合中存在一个固定发现 schema。只有模型调用工具时，目录内容才进入 transcript。

#### KV Cache 影响

适配器注册与目录变化不会改变 schema 前缀。每个发现结果都追加在可复用前缀之后。

### 前台结果

#### 模型看到什么

调用会保留描述与提示词。成功时只包含子 agent 的最终文本；其他结果变为 `Error: <stop reason>`，随后在存在时附上安全的提供方诊断，再附上任何部分 assistant 文本。子 agent 中间步骤不会进入父级。

#### Token 影响

提示词与结果保留在父级历史中，直到上下文压缩（context compaction）；子 agent 工作上下文留在子 agent 中。

#### KV Cache 影响

仅追加；新增可见内容位于可复用请求前缀之后，不会使现有 KV Cache 条目失效。

### 后台结果

#### 模型看到什么

在配置的可继续模式下，启动时返回内容恰为 `started subagent <childId>`；在配置的一次性模式下，则返回 `started background subagent job <id>`。一次性模式下，通用 Task 接口提供后续状态、最终输出、取消响应与通知；若结果携带提供方诊断，失败状态的 detail 会包含它。可继续模式下，本工具不返回自己的结果：子 agent 的结算以服务负责的通知到达父级，独立加载的 `send_message` 工具投递后续消息，而通过其 id 查看子 agent 的 transcript（文本记录）即是其详细输出来源。

#### Token 影响

确认消息会被保留；一次性最终输出只在收集或注入时进入父级历史，而可继续子 agent 的输出绝不会通过本工具返回——其结算通知独立于任何工具结果到达。

#### KV Cache 影响

仅追加；新增可见内容位于可复用请求前缀之后，不会使现有 KV Cache 条目失效。

## 已知限制与延期工作

<a id="known-limitations-and-deferred-work"></a>


这些限制说明本工具不返回或不强制执行什么；它们是当前包约束。

- **后台运行不通过本工具公开结果**——一次性任务的最终输出通过通用 Task 接口收集，可继续子 agent 的输出留在其自身会话中，按其 agent id 读取。结算通知会说明该子 agent 如何结束，并携带其最终 assistant 输出中的非空文本，但它不是本次调用的返回值，也无法在此等待。
- **等待中的实例较晚才发现重复名称**（`TODO(subagent-dup-toolname)`）——两个 `toolName` 相同的实例只有在其提供方出现时才会冲突，而重复名称失败会回滚该提供方注册；若要更早失败，需要一份预期名称注册表。
- **随附 fork 工具不能选择子级 LLM 路由**——它们继承父级提供方与模型，使复制的对话前缀仍有资格复用 KV Cache。仅当路由变更能保留复用或公开有界重算成本时，才重新启用选择。
- **非路由子 agent 策略按实例固定**——另一个 persona、工具过滤器或深度上限需要另一个名称不同的工具。LLM 选择要求启用逐 Session 偏好，且提供方必须声明 `agentOptions`；两个进程内提供方和 DSH SDK 会声明该能力，而 ACP、Codex 与 Claude Code 会拒绝它，而不是忽略它。

<a id="dev-note"></a>
### 开发备注

<details>
<summary>维护者的工作上下文——点击展开</summary>

无。

</details>
