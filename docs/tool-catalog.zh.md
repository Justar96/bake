<!-- 英文源文件由 scripts/gen-tool-catalog.ts 生成；本中文文件是通过双语配对维护的经评审对侧。
     更新时先运行 `bun run gen-tool-catalog` 更新英文，再更新本文件并运行 `bun run verify-translation-pairing --write docs/tool-catalog.md` 重新记录配对。 -->

# 工具 Schema 目录

[English](tool-catalog.md) | 中文

已发布插件向 `ctx.tools` 提供的所有面向模型的工具：模型通过系统提示词组装获得的 `name`、`description` 和 JSON Schema `parameters`。本目录是[子系统页面](subsystems/core.zh.md)（类型及每页生成的 `cordis-surface` 接线区域）的补充；本页列出的是向 agent（智能体）提供的*工具*。

英文源文件由系统**生成**，并通过 `bun run verify-tool-catalog`（`doc-sync`（文档同步门禁）的一部分）验证新鲜度；本中文文件作为经评审对侧通过双语配对维护。与 Cordis 目录（纯源码 AST 处理）不同，英文生成器会在真实上下文中**启动**每个工具插件并读取 `ctx.tools.schemas()`，因为工具 schema 无法通过静态分析完全确定，例如运行时展开的枚举、拼接的描述、由配置决定的名称以及使用原始 JSON Schema 的 MCP 工具。完整性守卫会 glob 匹配 `packages/*/tool-*`；如果生成器的启动 manifest（元数据清单）遗漏任何包，检查就会失败，因此新工具不会在无人察觉的情况下缺少文档。

范围：`packages/*/tool-*` 下已发布的产品工具，每个工具均使用其**默认**配置启动；但如果某个 Config 字段是**必填项**且没有默认值，生成器就必须作出选择，对应包的说明会记录本页展示的是哪个分支。注册的工具**名称**可以是加载时配置，例如 `tool-subagent` 的 `toolName`，因此部署可能以不同名称或额外名称提供某个包；如果存在随产品发布的别名，对应包的说明会予以记录。`examples/` 中的演示工具（例如 `echo`）不在范围内，这与 Cordis 目录仅涵盖包的范围一致。

<a id="tool-package-map"></a>

## 工具包映射

下表将模型可见的工具名称与其背后的插件包和服务 seam 对应起来。各包章节随后给出确切的 JSON Schema。

| 工具包 | 模型可见名称 | 依赖 | 写入／影响 | 随产品发布的别名 | 部署说明 |
| --- | --- | --- | --- | --- | --- |
| `@deepseek-ai/dsh-plugin-manager` | `plugin_manager` | `ctx.tools`, `ctx.pluginManager`, `ctx.sandboxPolicy` | `tool/call`, `tool/result`, `user/message` | - | - |
| `@deepseek-ai/dsh-mcp-resources` | `list_mcp_resource_templates`, `list_mcp_resources`, `read_mcp_resource` | `ctx.tools`, `ctx.mcpResources` | `tool/call`, `tool/result` | - | - |
| `@deepseek-ai/dsh-tool-ask-user` | `ask_user_question` | `ctx.tools`、`ctx.userQuestions` | `tool/call`、`tool/result after a UI/provider answers the question` | - | ask_user_question 会暂停工具调用，直到当前 UI 提供方返回人类答案。 |
| `@deepseek-ai/dsh-tools` | `run_code`、`tool_help` | `ctx.tools`、`ctx.ptcRuntime (execution time)`、`ctx.systemPrompt` | `tool/call`、`one tool/ptc-dispatch-start + tool/ptc-dispatch pair per bridged sub-call`、`tool/result` | - | 在 `mode: ptc`／`mode: both` 下，它由工具注册表所有，作为可过滤能力层之外的保留传输机制（参见 PTC mode Agent Note）。在 `ptc` 下，它是注册表对协议格式（wire format）的唯一贡献；其他可见能力在使用已加载运行时语言生成的 SDK 章节中声明。程序通过 binding 调用这些能力，调用按照原生并发约定调度：启动顺序和策略遵循提交顺序，并发安全的函数体最多重叠执行 `maxParallelSubCalls` 个。调用会重新进入完整且受守卫保护的工具流水线，并将每个嵌套执行关联到此外层结果。`tool_help` 恰在某个可见工具声明 `details` 时可见；`details` 是不放进原生 schema 的用法参考，`tool_help` 返回这份参考，而在 `ptc` 下，这份参考改为并入该工具的 SDK 文档。 |
| `@deepseek-ai/dsh-tool-bash` | `bash` | `ctx.tools`、`ctx.shell`、`ctx.shellEnv`、`ctx.jobs at call time for run_in_background` | `tool/call`、`tool/result` | - | bash 工具是 bash 执行器 seam 面向模型的消费方。使用 `run_in_background` 的运行会注册到通用 `ctx.jobs` 运行时，并通过 `job_*` 工具（来自 `@deepseek-ai/dsh-tool-jobs`）收集／停止；禁用 `enableRunInBackground` 配置（默认为 true）后，该参数会被完全移除。 |
| `@deepseek-ai/dsh-tool-present` | `present` | `ctx.tools`, `ctx.fs`, `ctx.sessionProjections` | `tool/call`, `deliverables/presented 在成功的最终结果之后`, `tool/result` | - | 交付归调用方 Session 所有；Web ui-deliverables 提供源文件打开与卡片。 |
| `@deepseek-ai/dsh-tool-pwsh` | `pwsh` | `ctx.tools`、`ctx.shell`、`ctx.shellEnv`、`ctx.jobs at call time for run_in_background` | `tool/call`、`tool/result` | - | pwsh 工具是 Windows 组合中 bash 执行器 seam 的 PowerShell 方言消费方（由 `@deepseek-ai/dsh-pwsh-local` 等 PowerShell 执行器为 `ctx.shell` 提供后端）；除沙箱接口外，它逐项对应 bash 工具调用。使用 `run_in_background` 的运行会注册到通用 `ctx.jobs` 运行时，并通过 `job_*` 工具收集／停止；托管的 `DSH_*` 环境来自 `@deepseek-ai/dsh-shell-env`。每次调用都在新进程中运行，不使用持久 PTY 会话。路径采用原生 `C:\...` 形式，变量采用 `$env:NAME`。 |
| `@deepseek-ai/dsh-tool-cordis` | `cordis_inspect_list`, `cordis_inspect_query` | `ctx.tools`, `ctx.cordisInspect` | `tool/call`, `tool/result` | - | 创造模式提供两个只读运行时检查工具。Cordis host runner 提供检查注册表；Client 查询需要已连接页面。持久化变更编写为组合包，再通过 plugin_manager 安装。 |
| `@deepseek-ai/dsh-tool-bash-persistent` | `bash` | `ctx.tools`、`ctx.terminals`、`an owning Agent at execution time` | `tool/call`、`PTY shell state`、`tool/result` | - | 一个按所有者隔离的持久 bash 工具；部署组合提供 PTY 后端，并可覆盖面向模型的环境描述。 |
| `@deepseek-ai/dsh-tool-pwsh-persistent` | `pwsh` | `ctx.tools`、`ctx.terminals`、`an owning Agent at execution time` | `tool/call`、`PTY shell state`、`tool/result` | - | 一个按所有者隔离的持久 pwsh 工具，持久 bash 工具的 Windows 对应物；部署组合提供 pwsh 方言的 PTY 后端，并可覆盖面向模型的环境描述。 |
| `@deepseek-ai/dsh-tool-str-replace-editor` | `str_replace_editor` | `ctx.tools`、`ctx.fs` | `tool/call`、`fs/observed after view presence/absence, edit absence, or successful mutation`、`tool/result` | - | 基于文件系统 seam 的独立查看／创建／唯一字面量替换／按行插入工具；可与任何 shell 或终端接口组合。 |
| `@deepseek-ai/dsh-tool-fs` | `edit`、`read`、`read_image`、`write` | `ctx.tools`、`ctx.fs`、`ctx.attachments (image-tool registration)`、`ctx.llm + an image-capable route (image-tool execution)` | `tool/call`、`fs/write-intent or fs/edit-intent for mutations`、`fs/observed after read presence/absence or successful file operation`、`durable attachment (read_image)`、`tool/result` | - | 先读后写／编辑策略由 `@deepseek-ai/dsh-fs-observation-policy` 添加；它是一个 `fs/*` 事件门禁插件，不会改变 schema。加载这些工具的部署按预期也应加载该插件。没有 `ctx.attachments` 时图片工具不会注册；其 schema 与路由无关，执行时除非确切路由的模型声明图片输入，否则拒绝。 |
| `@deepseek-ai/dsh-tool-fs-search` | `glob`、`grep` | `ctx.tools`、`ctx.subprocess` | `tool/call`、`tool/result` | - | glob 和 grep 是无条件可用的发现工具，通过 ctx.subprocess spawn 随包提供的 ripgrep 二进制文件（`@vscode/ripgrep`），并作为普通前台调用运行，绝不作为后台任务；无需在宿主机安装 `rg`，也不经过 shell 层。本目录使用 `sampleOverCapGlobResults: true`；部署必须显式选择该行为。结果超过上限时，会通过可选的 ctx.spillStore 后端保存完整的格式化列表；在共置部署中，如果后端公开本地路径，返回的定位信息可供后续读取／搜索。 |
| `@deepseek-ai/dsh-tool-goal` | `create_goal`、`get_goal`、`update_goal` | `ctx.tools`、`ctx.agents`、`ctx.goals`、`a calling Agent in an authorized open turn` | `tool/call`、`goal/change for mutations`、`tool/result` | - | create、edit、pause 和 resume 要求直接来自人类的根权限；complete 和 blocked 也接受确切的当前 Goal Round。blocked 的默认下限是 3 个获准的 Round。 |
| `@deepseek-ai/dsh-schedule` | `schedule_create`、`schedule_delete`、`schedule_list` | `ctx.tools`、`ctx.sessions`、Session 持久化、未来创建的 live 根 Agent | `tool/call`、`schedule/change create or delete`、`tool/result` | - | 仅在选择启用的 Schedule 插件加载后创建的 live 根 Agent scope 内注册。版本 1 接受 after_seconds、显式绝对 at 和有界固定速率 every_seconds，并披露 session-local 交付；管理读取与变更必须通过共享的 Session 持久化 barrier。 |
| `@deepseek-ai/dsh-tool-ralph` | `ralph` | `ctx.tools`、`ctx.workflowEngine`、`ctx.subagents`、`ctx.systemPrompt`、`a calling Agent (exec.agent parents every fresh round)` | `tool/call`、`tool/result`、`workflow and child session events during execution` | - | 固定的前台工作流会在每个 Round 启动一个全新的结构化子级；模型只能选择不可变目标和可选的 Round 上限。 |
| `@deepseek-ai/dsh-tool-skill` | `skill` | `ctx.tools`、`ctx.agents`、`ctx.skills` | `tool/call`、`tool/result`、`user/message replacement catalogs via agent.inject()` | - | - |
| `@deepseek-ai/dsh-tool-subagent` | `list_subagent_models`、`subagent` | `ctx.tools`、`ctx.subagents`、`用于模型发现和所选路由校验的 ctx.llm` | `tool/call`、`tool/result`、`child session events through the chosen provider` | `subagent`、`subagent_fork` | 注册的委派工具名称取决于加载时 `toolName` 配置（默认为 `subagent`）；上述默认 schema 关闭模型选择，而发现 schema 则展示为已启用 Session 中可用的固定配套工具。Web preset 会在每个新顶层 Session 创建时读取插件页偏好，并为其子 Session 保留该决定；`subagent_fork` 始终使用固定路由。每个实例通过 `modelSelectionSettings`、`backgroundMode` 与 `enableRunInBackground` 独立控制是否读取模型选择设置及其后台行为。 |
| `@deepseek-ai/dsh-tool-subagent-control` | `interrupt_agent`、`list_agents`、`send_message` | `ctx.tools`、`ctx.subagents`、`ctx.agents and ctx.sessionProjections (list_agents only)` | `tool/call`、`tool/result`、`child session events through ctx.subagents` | - | 这些是控制可继续后台 subagent 的全局命名工具：绑定提供方的 `tool-subagent` 实例注册不同的委派工具；本包注册一次 `send_message` 和 `interrupt_agent`，另由 `list_agents` 通过单独加载的 `/list-agents` 插件提供，其目录行使用 sessionProjections 和实时 Agent 注册表。 |
| `@deepseek-ai/dsh-tool-jobs` | `job_kill`、`job_list`、`job_output` | `ctx.tools`、`ctx.jobs` | `tool/call`、`tool/result`、`user/message via agent.inject() for background completion notices` | - | 与任务种类无关的后台任务控制器：后台 bash 命令、PTY 发送和 subagent 都通过相同的 3 个工具读取、列出和终止。加载该插件会挂接控制器，从而启用生产方的 `ctx.jobs.start()`。 |
| `@deepseek-ai/dsh-tool-todo` | `todo_write` | `ctx.tools`、`owning Agent session` | `tool/call`、`todo/write`、`tool/result` | - | todo_write 是会话所有的状态；UI 将最新的 todo/write 事件渲染为检查清单。`allowParallelInProgress` 是没有默认值的必填项，因此本目录明确选择 `true`，对应描述允许同时存在多个 `in_progress` 项。选择 `false` 的部署会获得同一工具，但描述会要求只能有 1 个活动任务。 |
| `@deepseek-ai/dsh-tool-workflow` | `workflow` | `ctx.tools`、`ctx.workflowEngine`、`a calling Agent (exec.agent parents the script children)` | `tool/call`、`tool/result` | - | - |
| `@deepseek-ai/dsh-tool-web` | `web_fetch`、`web_search` | `ctx.tools`、`ctx.web` | `tool/call`、`tool/result` | - | web_search 和 web_fetch 将提供方选择置于 ctx.web 之后，使模型可见 schema 在更换后端时保持稳定。 |

<a id="deepseek-aidsh-plugin-manager"></a>

## `@deepseek-ai/dsh-plugin-manager`

### `plugin_manager`

列出当前 profile 中的插件或组合包，启用或禁用它们，安装组合包或移除已安装的组合包。每项操作都要求 danger-full-access 权限或本次调用的批准。批准不改变会话权限模式。变更影响该 profile 的所有会话。先列出条目以获取准确标识。包安装可能运行已获批准的构建脚本。支持热更新的 profile 立即应用变更；仅启动时加载的 profile 需要重启。

```json
{
  "type": "object",
  "properties": {
    "action": {
      "type": "string",
      "description": "Management operation.",
      "enum": [
        "list_plugins",
        "list_bundles",
        "set_plugin",
        "set_bundle",
        "install_bundle",
        "remove_bundle"
      ]
    },
    "target": {
      "type": "string",
      "description": "Plugin entry id, bundle package name, or installation spec, according to action."
    },
    "enabled": {
      "type": "boolean",
      "description": "Required for set operations; defaults to true for installation."
    },
    "approvedBuilds": {
      "type": "array",
      "description": "For install_bundle: pass names from pendingBuilds only after the user explicitly approves running their install scripts in the conversation. This grants persistent permission for this profile.",
      "items": {
        "type": "string"
      }
    },
    "offset": {
      "type": "number",
      "description": "Zero-based list offset; defaults to 0."
    },
    "limit": {
      "type": "number",
      "description": "List page size, from 1 to 100; defaults to 25."
    }
  },
  "required": [
    "action"
  ]
}
```

来源： [`packages/boot/plugin-manager/src/tools.ts`](../packages/boot/plugin-manager/src/tools.ts)

<a id="deepseek-aidsh-mcp-resources"></a>

## `@deepseek-ai/dsh-mcp-resources`

### `list_mcp_resource_templates`

列出 MCP 服务器提供的参数化资源 URI 模板。

```json
{
  "type": "object",
  "properties": {
    "server": {
      "type": "string",
      "description": "Configured MCP server name."
    },
    "cursor": {
      "type": "string",
      "description": "Continuation cursor returned by this server."
    }
  },
  "required": [
    "server"
  ]
}
```

来源： [`packages/mcp/mcp-resources/src/tools.ts`](../packages/mcp/mcp-resources/src/tools.ts)

### `list_mcp_resources`

列出 MCP 服务器提供的资源。

```json
{
  "type": "object",
  "properties": {
    "server": {
      "type": "string",
      "description": "Configured MCP server name."
    },
    "cursor": {
      "type": "string",
      "description": "Continuation cursor returned by this server."
    }
  },
  "required": [
    "server"
  ]
}
```

来源： [`packages/mcp/mcp-resources/src/tools.ts`](../packages/mcp/mcp-resources/src/tools.ts)

### `read_mcp_resource`

按 URI 从指定服务器读取 MCP 资源。使用已列出的 URI 或展开后的资源模板。

```json
{
  "type": "object",
  "properties": {
    "server": {
      "type": "string",
      "description": "Configured MCP server name."
    },
    "uri": {
      "type": "string",
      "description": "Resource URI to read."
    }
  },
  "required": [
    "server",
    "uri"
  ]
}
```

来源： [`packages/mcp/mcp-resources/src/tools.ts`](../packages/mcp/mcp-resources/src/tools.ts)

## `@deepseek-ai/dsh-tool-ask-user`

### `ask_user_question`

继续操作前，如果需要确认、选择或缺失的信息，请向用户提出简明问题。发送一个或多个问题，每个问题都带一个稳定 id，该 id 会在答案中原样返回。

```json
{
  "type": "object",
  "properties": {
    "questions": {
      "type": "array",
      "description": "Questions to ask the user before continuing.",
      "items": {
        "type": "object",
        "additionalProperties": true,
        "properties": {
          "id": {
            "type": "string",
            "description": "Stable id for this question; echoed in the answer."
          },
          "question": {
            "type": "string",
            "description": "The question to ask, as one short sentence ending with a question mark."
          },
          "header": {
            "type": "string",
            "description": "Optional heading of at most about 12 characters, such as \"Confirm\" or \"Choose Mode\"."
          },
          "options": {
            "type": "array",
            "description": "Optional choices to show the user. If you recommend one, put it first and append \"(Recommended)\" to that label.",
            "items": {
              "type": "object",
              "additionalProperties": true,
              "properties": {
                "label": {
                  "type": "string",
                  "description": "User-facing option label of 1-5 words."
                },
                "description": {
                  "type": "string",
                  "description": "One short sentence explaining the tradeoff or impact."
                }
              },
              "required": [
                "label"
              ]
            }
          },
          "multi_select": {
            "type": "boolean",
            "description": "Whether the user may select more than one option. Defaults to false."
          }
        },
        "required": [
          "id",
          "question"
        ]
      }
    }
  },
  "required": [
    "questions"
  ]
}
```

来源：[`packages/interaction/tool-ask-user/src/index.ts`](../packages/interaction/tool-ask-user/src/index.ts)

ask_user_question 会暂停工具调用，直到当前 UI 提供方返回人类答案。

<a id="deepseek-aidsh-tools"></a>

## `@deepseek-ai/dsh-tools`

### `run_code`

针对可用工具执行 TypeScript 程序。接受两个必填参数：`code`，即异步函数的**函数体**（仅使用可擦除语法；支持顶层 `await` 和 `return`）；以及 `description`，简要说明该程序做什么。请根据系统提示词中的声明，以 `await tools.name(args)` 形式调用工具。只有打印或返回的内容属于程序输出，请谨慎筛选。含图片的子工具结果会在运行结束后附加。

```json
{
  "type": "object",
  "properties": {
    "code": {
      "type": "string",
      "description": "The program: the body of an async TypeScript function."
    },
    "description": {
      "type": "string",
      "description": "Clear, concise description of what this program does in active voice, 5-10 words (shown in the UI). Examples: \"Count TODO markers across packages\"; \"Read failing test and its fixture\"; \"Rename config key in every cordis.yml\"."
    },
    "timeoutMs": {
      "type": "number",
      "description": "Positive elapsed-time budget in milliseconds, capped by the deployment maximum."
    },
    "sandbox_permissions": {
      "type": "string",
      "description": "Wider sandbox mode for this complete program execution; requires justification and approval.",
      "enum": [
        "workspace-write",
        "danger-full-access"
      ]
    },
    "justification": {
      "type": "string",
      "description": "Reason this complete program needs wider access, shown to the user for approval."
    }
  },
  "required": [
    "code",
    "description"
  ]
}
```

来源：[`packages/core/tools/src/ptc.ts`](../packages/core/tools/src/ptc.ts)

### `tool_help`

返回某个工具的完整用法参考；该工具的描述会说明首次使用前先用 tool_help 读取。

```json
{
  "type": "object",
  "properties": {
    "name": {
      "type": "string",
      "description": "Name of the tool."
    }
  },
  "required": [
    "name"
  ]
}
```

来源：[`packages/core/tools/src/tool-help.ts`](../packages/core/tools/src/tool-help.ts)

在 `mode: ptc`／`mode: both` 下，它由工具注册表所有，作为可过滤能力层之外的保留传输机制（参见 PTC mode Agent Note）。在 `ptc` 下，它是注册表对协议格式的唯一贡献；其他可见能力在使用已加载运行时语言生成的 SDK 章节中声明。程序通过 binding 调用这些能力，调用按照原生并发约定调度：启动顺序和策略遵循提交顺序，并发安全的函数体最多重叠执行 `maxParallelSubCalls` 个。调用会重新进入完整且受守卫保护的工具流水线，并将每个嵌套执行关联到此外层结果。`tool_help` 恰在某个可见工具声明 `details` 时可见；`details` 是不放进原生 schema 的用法参考，`tool_help` 返回这份参考，而在 `ptc` 下，这份参考改为并入该工具的 SDK 文档。

<a id="deepseek-aidsh-tool-bash"></a>

## `@deepseek-ai/dsh-tool-bash`

### `bash`

使用 `bash -c` 运行命令，并返回其 stdout 和 stderr。每次调用都会启动新 shell，因此目录切换和变量不会延续到后续调用。避免覆盖整个文件系统的 `find` 扫描。非零退出会在结果中报告为 `[exit code: N]`，而不是作为工具错误。较长的输出会截断为尾部；如有可能，完整输出会保存到结果中注明的文件。`$DSH_HOME` 是 harness 主目录，`$DSH_SESSION_ID` 是本会话的 id。耗时的构建和测试应使用 `run_in_background` 运行，它会立即返回 job id；使用 `job_output` 读取输出，使用 `job_kill` 停止。

```json
{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "description": "The bash command to execute."
    },
    "description": {
      "type": "string",
      "description": "Short summary of what the command does, shown to the user."
    },
    "timeoutMs": {
      "type": "number",
      "description": "Timeout in milliseconds, capped at the maximum; the command is killed when it expires."
    },
    "workdir": {
      "type": "string",
      "description": "Directory to run this command in. Defaults to your working directory; a relative path resolves against it."
    },
    "run_in_background": {
      "type": "boolean",
      "description": "Run in the background, with no timeout."
    }
  },
  "required": [
    "command"
  ]
}
```

来源：[`packages/shell/tool-bash/src/index.ts`](../packages/shell/tool-bash/src/index.ts)

bash 工具是 bash 执行器 seam 面向模型的消费方。使用 `run_in_background` 的运行会注册到通用 `ctx.jobs` 运行时，并通过 `job_*` 工具（来自 `@deepseek-ai/dsh-tool-jobs`）收集／停止；禁用 `enableRunInBackground` 配置（默认为 true）后，该参数会被完全移除。

<a id="deepseek-aidsh-tool-present"></a>

## `@deepseek-ai/dsh-tool-present`

### `present`

交付用户要求接收的文件。创建或更新此类文件后，无论以何种方式生成，都要在最终回复前调用 present；仅提到其路径是不够的。文件必须已存在于你的文件工具能够访问的位置。用户会在原位置打开这些文件，因此请将它们留在原处。

```json
{
  "type": "object",
  "properties": {
    "files": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
          "path": {
            "type": "string",
            "description": "Path of an existing regular file, absolute or relative to the working directory."
          },
          "description": {
            "type": "string",
            "description": "Brief description for the user."
          }
        },
        "required": [
          "path"
        ]
      }
    }
  },
  "required": [
    "files"
  ]
}
```

来源： [`packages/deliverables/tool-present/src/index.ts`](../packages/deliverables/tool-present/src/index.ts)

交付归调用方 Session 所有；Web ui-deliverables 提供源文件打开与卡片。

<a id="deepseek-aidsh-tool-pwsh"></a>

## `@deepseek-ai/dsh-tool-pwsh`

### `pwsh`

使用 `pwsh -Command` 运行 PowerShell 命令，并返回其 stdout 和 stderr。每次调用都会启动新的 pwsh 进程，因此目录切换和变量不会延续到后续调用。路径采用 Windows 原生形式（`C:\...`），环境变量通过 `$env:NAME` 读取。非零退出会在结果中报告为 `[exit code: N]`，而不是作为工具错误。在 Windows 上，被强制终止的命令同样以 `[exit code: 1]` 结尾，且不带信号标记，因此中断后出现的这种退出表示终止，而不是命令失败。较长的输出会截断为尾部；如有可能，完整输出会保存到结果中注明的文件。`$env:DSH_HOME` 是 harness 主目录，`$env:DSH_SESSION_ID` 是本会话的 id。使用 `run_in_background` 运行的命令会立即返回 job id；使用 `job_output` 读取其输出，使用 `job_kill` 停止它。

```json
{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "description": "The PowerShell command to execute."
    },
    "description": {
      "type": "string",
      "description": "Short summary of what the command does, shown to the user."
    },
    "timeoutMs": {
      "type": "number",
      "description": "Timeout in milliseconds, capped at the maximum; the command is killed when it expires."
    },
    "workdir": {
      "type": "string",
      "description": "Directory to run this command in. Defaults to your working directory; a relative path resolves against it."
    },
    "run_in_background": {
      "type": "boolean",
      "description": "Run in the background, with no timeout."
    }
  },
  "required": [
    "command"
  ]
}
```

来源：[`packages/shell/tool-pwsh/src/index.ts`](../packages/shell/tool-pwsh/src/index.ts)

pwsh 工具是 Windows 组合中 bash 执行器 seam 的 PowerShell 方言消费方（由 `@deepseek-ai/dsh-pwsh-local` 等 PowerShell 执行器为 `ctx.shell` 提供后端）；除沙箱接口外，它逐项对应 bash 工具调用。使用 `run_in_background` 的运行会注册到通用 `ctx.jobs` 运行时，并通过 `job_*` 工具收集／停止；托管的 `DSH_*` 环境来自 `@deepseek-ai/dsh-shell-env`。每次调用都在新进程中运行，不使用持久 PTY 会话。路径采用原生 `C:\...` 形式，变量采用 `$env:NAME`。

<a id="deepseek-aidsh-tool-cordis"></a>

## `@deepseek-ai/dsh-tool-cordis`

### `cordis_inspect_list`

列出 Host 当前已知的所有 Cordis Inspect Provider，包括本地 Host Provider 和 Client 同步的最新清单。每项包含平台、用途、只读方法以及输入输出 schema。编写或配置插件前先调用本工具，再从结果选择 cordis_inspect_query 的 provider 和方法。不要猜测名称，也不要把 Inspect 方法当作插件代码可调用的业务 Service。

```json
{
  "type": "object",
  "properties": {}
}
```

来源： [`packages/extensions/tool-cordis/src/index.ts`](../packages/extensions/tool-cordis/src/index.ts)

### `cordis_inspect_query`

执行 Inspect Provider 明确声明的只读查询。platform、provider 和 method 必须来自 cordis_inspect_list，input 必须符合该方法的 schema。编写插件代码前，用本工具读取准确的 Service 方法、Event 模式、插件 Config schema、Tool schema、主题 token，或实时 Slot 树与 props。Host 查询在本地运行。Client 查询等待页面首个有效响应，直到页面回应或工具取消。本工具不能调用业务 Service 方法或修改运行时。对于 Service.listService 和 Event.listEvents，不传 input 可浏览精简签名目录，再查询准确服务或事件以获得完整约定及引用类型。对于 Slots.listSubTree，不传 root 可浏览精简树；查询准确的 Slot root 可获得完整注册约定和 props，而查询准确的 Factory root 只返回 identity、scope 与 registrant。

```json
{
  "type": "object",
  "properties": {
    "platform": {
      "type": "string",
      "description": "Runtime platform that owns the Provider.",
      "enum": [
        "host",
        "client"
      ]
    },
    "provider": {
      "type": "string",
      "description": "Exact Provider ID returned by cordis_inspect_list."
    },
    "method": {
      "type": "string",
      "description": "Exact method name declared by the Provider manifest."
    },
    "input": {
      "description": "Optional query input; it must satisfy the method input schema."
    }
  },
  "required": [
    "platform",
    "provider",
    "method"
  ]
}
```

来源： [`packages/extensions/tool-cordis/src/index.ts`](../packages/extensions/tool-cordis/src/index.ts)

创造模式提供两个只读运行时检查工具。Cordis host runner 提供检查注册表；Client 查询需要已连接页面。持久化变更编写为组合包，再通过 plugin_manager 安装。

<a id="deepseek-aidsh-tool-bash-persistent"></a>

## `@deepseek-ai/dsh-tool-bash-persistent`

### `bash`

在持久 bash shell 中运行命令。包括当前目录和已导出环境变量在内的状态会在此 agent 的多次调用之间保留。

```json
{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "description": "The bash command to run. Relative path is preferred in the command."
    }
  },
  "required": [
    "command"
  ]
}
```

来源：[`packages/shell/tool-bash-persistent/src/index.ts`](../packages/shell/tool-bash-persistent/src/index.ts)

一个按所有者隔离的持久 bash 工具；部署组合提供 PTY 后端，并可覆盖面向模型的环境描述。

<a id="deepseek-aidsh-tool-pwsh-persistent"></a>

## `@deepseek-ai/dsh-tool-pwsh-persistent`

### `pwsh`

在持久 PowerShell shell 中运行命令。包括当前目录和已导出环境变量在内的状态会在此 agent 的多次调用之间保留。

```json
{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "description": "The PowerShell command to run. Relative path is preferred in the command."
    }
  },
  "required": [
    "command"
  ]
}
```

来源：[`packages/shell/tool-pwsh-persistent/src/index.ts`](../packages/shell/tool-pwsh-persistent/src/index.ts)

一个按所有者隔离的持久 pwsh 工具，持久 bash 工具的 Windows 对应物；部署组合提供 pwsh 方言的 PTY 后端，并可覆盖面向模型的环境描述。

<a id="deepseek-aidsh-tool-str-replace-editor"></a>

## `@deepseek-ai/dsh-tool-str-replace-editor`

### `str_replace_editor`

用于查看、创建和编辑文件的自定义编辑工具：

* 状态会在命令调用以及与用户的讨论之间持久保留
* 如果 `path` 是文件，`view` 会显示应用 `cat -n` 后的结果。如果 `path` 是目录，`view` 会列出最多向下 2 层的非隐藏文件和目录
* 如果指定的 `create` 命令目标 `path` 已作为文件存在，则不能使用该命令
* 如果 `command` 产生较长输出，输出会被截断并标记为 `<response clipped>`
* 当前命令不使用某个参数时，值为 `null` 的占位参数视为未提供。必填参数仍须提供值；删除匹配内容时应省略 `str_replace.new_str`，而不是将其设为 `null`

使用 `str_replace` 命令时请注意：

* `old_str` 参数应与原文件中一行或多行连续内容**完全**匹配。请留意空白字符！
* 如果 `old_str` 参数在文件中不唯一，则不会执行替换。请确保在 `old_str` 中包含足够的上下文，使其唯一
* `new_str` 参数应包含用于替换 `old_str` 的已编辑行

```json
{
  "type": "object",
  "properties": {
    "command": {
      "type": "string",
      "description": "The commands to run. Allowed options are: `view`, `create`, `str_replace`, `insert`.",
      "enum": [
        "view",
        "create",
        "str_replace",
        "insert"
      ]
    },
    "path": {
      "type": "string",
      "description": "Absolute path to file or directory, e.g. `/repo/file.py` or `/repo`."
    },
    "file_text": {
      "oneOf": [
        {
          "type": "string"
        },
        {
          "type": "null"
        }
      ],
      "description": "Required string parameter of `create` command, with the content of the file to be created. A null placeholder is treated as omitted by commands that do not use this parameter."
    },
    "insert_line": {
      "oneOf": [
        {
          "type": "integer"
        },
        {
          "type": "null"
        }
      ],
      "description": "Required integer parameter of `insert` command. The `new_str` will be inserted AFTER the line `insert_line` of `path`. A null placeholder is treated as omitted by commands that do not use this parameter."
    },
    "new_str": {
      "oneOf": [
        {
          "type": "string"
        },
        {
          "type": "null"
        }
      ],
      "description": "Optional string parameter of `str_replace` command containing the new string (if omitted, no string will be added). Required string parameter of `insert` command containing the string to insert. A null placeholder is accepted only by commands that do not use this parameter."
    },
    "old_str": {
      "oneOf": [
        {
          "type": "string"
        },
        {
          "type": "null"
        }
      ],
      "description": "Required string parameter of `str_replace` command containing the string in `path` to replace. A null placeholder is treated as omitted by commands that do not use this parameter."
    },
    "view_range": {
      "oneOf": [
        {
          "type": "array",
          "items": {
            "type": "integer"
          }
        },
        {
          "type": "null"
        }
      ],
      "description": "Optional parameter of `view` command when `path` points to a file. If omitted or null, the full file is shown. If provided, the file will be shown in the indicated line number range, e.g. [11, 12] will show lines 11 and 12. Indexing at 1 to start. Setting `[start_line, -1]` shows all lines from `start_line` to the end of the file."
    }
  },
  "required": [
    "command",
    "path"
  ]
}
```

来源：[`packages/fs/tool-str-replace-editor/src/index.ts`](../packages/fs/tool-str-replace-editor/src/index.ts)

基于文件系统 seam 的独立查看／创建／唯一字面量替换／按行插入工具；可与任何 shell 或终端接口组合。

<a id="deepseek-aidsh-tool-fs"></a>

## `@deepseek-ai/dsh-tool-fs`

### `edit`

替换现有 UTF-8 文本文件中的字面量文本。除非设置了 replace_all，否则每个 old_string 都必须与当前文件恰好匹配一次。对同一文件的多处修改放进 `edits`，每项都与原始内容匹配。修改文件请使用此工具，而不是 shell 脚本。

```json
{
  "type": "object",
  "properties": {
    "file_path": {
      "type": "string",
      "description": "Absolute path, or relative to the working directory."
    },
    "old_string": {
      "type": "string",
      "description": "Exact text to replace, including whitespace, without read's line numbers."
    },
    "new_string": {
      "type": "string",
      "description": "Replacement text; empty deletes the match."
    },
    "replace_all": {
      "type": "boolean",
      "description": "Replace every occurrence."
    },
    "edits": {
      "type": "array",
      "description": "Several replacements, instead of old_string and new_string.",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
          "old_string": {
            "type": "string"
          },
          "new_string": {
            "type": "string"
          },
          "replace_all": {
            "type": "boolean"
          }
        },
        "required": [
          "old_string",
          "new_string"
        ]
      }
    }
  },
  "required": [
    "file_path"
  ]
}
```

来源：[`packages/fs/tool-fs/src/index.ts`](../packages/fs/tool-fs/src/index.ts)

### `read`

以带行号的形式读取 UTF-8 文本文件，长文件会分页返回。与 shell 中的 cat 不同，它会被视为后续 `write` 调用所需的读取。

```json
{
  "type": "object",
  "properties": {
    "file_path": {
      "type": "string",
      "description": "Absolute path, or relative to the working directory."
    },
    "offset": {
      "type": "number",
      "description": "1-based first line to return. Defaults to 1."
    },
    "limit": {
      "type": "number",
      "description": "Maximum number of lines to return. Defaults to 2000."
    }
  },
  "required": [
    "file_path"
  ]
}
```

来源：[`packages/fs/tool-fs/src/index.ts`](../packages/fs/tool-fs/src/index.ts)

### `read_image`

读取 PNG、JPEG、WebP 或 GIF 文件并返回图像。无扩展名的路径也可以使用；其格式按文件内容检测。大图会自动缩小，因此不要仅为查看图片而安装图片工具或创建缩略图。要求使用接受图像输入的模型。

```json
{
  "type": "object",
  "properties": {
    "file_path": {
      "type": "string",
      "description": "Absolute path, or relative to the working directory."
    }
  },
  "required": [
    "file_path"
  ]
}
```

来源：[`packages/fs/tool-fs/src/index.ts`](../packages/fs/tool-fs/src/index.ts)

### `write`

创建 UTF-8 文本文件，或替换其全部内容。替换现有文件需要对它的当前读取。只做局部修改时，使用 edit 可避免重新发送整个文件。

```json
{
  "type": "object",
  "properties": {
    "file_path": {
      "type": "string",
      "description": "Absolute path, or relative to the working directory."
    },
    "content": {
      "type": "string",
      "description": "Full UTF-8 text content to write."
    }
  },
  "required": [
    "file_path",
    "content"
  ]
}
```

来源：[`packages/fs/tool-fs/src/index.ts`](../packages/fs/tool-fs/src/index.ts)

先读后写／编辑策略由 `@deepseek-ai/dsh-fs-observation-policy` 添加；它是一个 `fs/*` 事件门禁插件，不会改变 schema。加载这些工具的部署按预期也应加载该插件。没有 `ctx.attachments` 时图片工具不会注册；其 schema 与路由无关，执行时除非确切路由的模型声明图片输入，否则拒绝。

<a id="deepseek-aidsh-tool-fs-search"></a>

## `@deepseek-ai/dsh-tool-fs-search`

### `glob`

查找路径匹配 glob 模式的文件，不包括目录。它是 shell 中 find 的有界替代方案，结果按最新优先排序：包括隐藏文件和被忽略的文件，但不包括 VCS 元数据。将 `path` 限制在仓库内，不要使用 `/` 或 `$HOME`。如果结果超过 100 条路径，则显示从顶层条目中抽样的 100 条，说明已抽样，并报告完整列表的保存位置。

```json
{
  "type": "object",
  "properties": {
    "pattern": {
      "type": "string",
      "description": "Glob pattern, e.g. \"**/*.ts\" or \"src/**/*.test.js\". A pattern without \"/\" matches file names at any depth, so \"*.ts\" searches the whole tree."
    },
    "path": {
      "type": "string",
      "description": "Directory to search. Defaults to the working directory; relative paths resolve against it."
    }
  },
  "required": [
    "pattern"
  ]
}
```

来源：[`packages/fs/tool-fs-search/src/index.ts`](../packages/fs/tool-fs-search/src/index.ts)

### `grep`

使用 ripgrep 正则表达式搜索文件内容，可作为 shell 中 grep 或 rg 的有界替代方案。会跳过隐藏文件和被忽略的文件，除非 path 指向它们。将 `path` 限制在仓库内，不要使用 `/` 或 `$HOME`。只返回匹配行，带行号并按文件分组；如需上下文，请读取匹配的文件。最多显示 250 条匹配；如果结果更多，会予以说明，并报告完整列表的保存位置。

```json
{
  "type": "object",
  "properties": {
    "pattern": {
      "type": "string",
      "description": "Regular expression to search for (ripgrep syntax)."
    },
    "path": {
      "type": "string",
      "description": "File or directory to search. Defaults to the working directory; relative paths resolve against it."
    },
    "include": {
      "type": "string",
      "description": "One glob filter for which files to search (e.g. \"*.ts\", \"*.{js,jsx}\"). Not a list; negation is not supported."
    }
  },
  "required": [
    "pattern"
  ]
}
```

来源：[`packages/fs/tool-fs-search/src/index.ts`](../packages/fs/tool-fs-search/src/index.ts)

glob 和 grep 是无条件可用的发现工具，通过 ctx.subprocess spawn 随包提供的 ripgrep 二进制文件（`@vscode/ripgrep`），并作为普通前台调用运行，绝不作为后台任务；无需在宿主机安装 `rg`，也不经过 shell 层。本目录使用 `sampleOverCapGlobResults: true`；部署必须显式选择该行为。结果超过上限时，会通过可选的 ctx.spillStore 后端保存完整的格式化列表；在共置部署中，如果后端公开本地路径，返回的定位信息可供后续读取／搜索。

## `@deepseek-ai/dsh-tool-goal`

### `create_goal`

创建本会话的目标：一个跨多个自动 Round 持续推进、直至完成的长期目标。仅当用户在当前轮次中明确要求设定目标时才调用它；绝不要主动创建目标。一个会话同一时间只有一个目标，subagent 不能创建目标。

```json
{
  "type": "object",
  "properties": {
    "objective": {
      "type": "string",
      "description": "The concrete objective, taken from the user's request."
    },
    "max_goal_rounds": {
      "type": "number",
      "description": "Optional limit on automatic rounds, as a positive integer."
    }
  },
  "required": [
    "objective"
  ]
}
```

来源：[`packages/goal/tool-goal/src/index.ts`](../packages/goal/tool-goal/src/index.ts)

### `get_goal`

返回本会话的当前目标（没有则返回 null），包括其 id、revision、目标、阶段、已开始的 Round 数、Round 上限、阻塞原因（如有），以及是否会自动继续。

```json
{
  "type": "object",
  "properties": {}
}
```

来源：[`packages/goal/tool-goal/src/index.ts`](../packages/goal/tool-goal/src/index.ts)

### `update_goal`

更改当前目标。请先调用 get_goal，并传入其返回的确切 goal_id 和 revision。edit、pause 和 resume 需要用户在当前轮次中提出请求；在自动 Goal Round 期间，complete 和 blocked 也可使用。会话被重新打开或 fork 后，其目标会停止运行自动 Round，直到用户要求继续；届时请使用 resume 恢复它。仅当目标已达成时才将其标记为 complete。仅当同一个具体阻碍已连续持续至少 3 个 Goal Round 时才将其标记为 blocked；遇到困难或仍有剩余工作都不算阻碍。

```json
{
  "type": "object",
  "properties": {
    "goal_id": {
      "type": "string",
      "description": "The current goal's id."
    },
    "revision": {
      "type": "number",
      "description": "The current goal's revision."
    },
    "action": {
      "type": "string",
      "description": "edit the objective or round limit, pause or resume automatic rounds, or mark the goal complete or blocked.",
      "enum": [
        "edit",
        "pause",
        "resume",
        "complete",
        "blocked"
      ]
    },
    "objective": {
      "type": "string",
      "description": "The new objective; used only with action edit."
    },
    "max_goal_rounds": {
      "type": "number",
      "description": "The new round limit; used only with action edit."
    },
    "blocked_reason": {
      "type": "string",
      "description": "The concrete obstacle; required with action blocked and unused otherwise."
    }
  },
  "required": [
    "goal_id",
    "revision",
    "action"
  ]
}
```

来源：[`packages/goal/tool-goal/src/index.ts`](../packages/goal/tool-goal/src/index.ts)

create、edit、pause 和 resume 要求直接来自人类的根权限；complete 和 blocked 也接受确切的当前 Goal Round。blocked 的默认下限是 3 个获准的 Round。

<a id="deepseek-aidsh-schedule"></a>

## `@deepseek-ai/dsh-schedule`

### `schedule_create`

在当前会话中创建一条提醒。请提供非空 prompt 和恰好一个 selector：正的安全整数 after_seconds 延时；作为严格带偏移日期时间或本地日期／时间对象的 at；或不小于 300 的安全整数 every_seconds。固定速率提醒始终与创建时刻对齐，会跳过错过的发生时点，并把每条逾期规则的最新一个发生时点合并到一个批次中。交付模式是 session-local：只有此会话处于 live 状态时，提醒才会准时运行；否则提醒会进入 overdue 状态，直至会话恢复。

```json
{
  "type": "object",
  "properties": {
    "prompt": {
      "type": "string",
      "description": "Reminder content to present when the target becomes due."
    },
    "after_seconds": {
      "type": "number",
      "description": "Positive safe-integer delay in seconds."
    },
    "every_seconds": {
      "type": "number",
      "description": "Fixed-rate safe-integer interval in seconds, at least 300."
    },
    "at": {
      "oneOf": [
        {
          "type": "string"
        },
        {
          "type": "object",
          "additionalProperties": false,
          "properties": {
            "date": {
              "type": "string"
            },
            "time": {
              "type": "string"
            },
            "time_zone": {
              "type": "string"
            }
          },
          "required": [
            "date",
            "time",
            "time_zone"
          ]
        }
      ],
      "description": "Absolute target as strict offset RFC 3339 or local date/time with an explicit IANA zone."
    }
  },
  "required": [
    "prompt"
  ]
}
```

来源：[`packages/schedule/schedule/src/tools.ts`](../packages/schedule/schedule/src/tools.ts)

### `schedule_delete`

使用 schedule_create 或 schedule_list 返回的确切 id，删除当前会话中的一条活动提醒。未知或已经结束的 id 会返回 deleted false。

```json
{
  "type": "object",
  "properties": {
    "id": {
      "type": "string",
      "description": "Exact session-local schedule id."
    }
  },
  "required": [
    "id"
  ]
}
```

来源：[`packages/schedule/schedule/src/tools.ts`](../packages/schedule/schedule/src/tools.ts)

### `schedule_list`

按创建顺序列出当前会话中的所有活动提醒，包括确切 id、UTC 目标、scheduled 或 overdue 状态，以及 session-local 交付模式。

```json
{
  "type": "object",
  "properties": {}
}
```

来源：[`packages/schedule/schedule/src/tools.ts`](../packages/schedule/schedule/src/tools.ts)

仅在选择启用的 Schedule 插件加载后创建的 live 根 Agent scope 内注册。版本 1 接受 after_seconds、显式绝对 at 和有界固定速率 every_seconds，并披露 session-local 交付；管理读取与变更必须通过共享的 Session 持久化 barrier。

## `@deepseek-ai/dsh-tool-ralph`

### `ralph`

围绕一个不可变目标运行使用全新 agent 的前台 Ralph 循环。仅当直接人类明确要求 Ralph 或使用全新 agent 迭代时使用。每个 Round 都会启动一个全新子级，该子级看不到父级对话或先前子会话；共享工作区充当长期记忆，Round 之间只传递有界的结构化报告。当工作进程报告完成、报告具体阻塞项或达到 Round 上限时，调用返回。普通的长期同会话工作应使用 goal 工具。

```json
{
  "type": "object",
  "properties": {
    "objective": {
      "type": "string",
      "description": "The immutable completion objective for every fresh Ralph round."
    },
    "maxRounds": {
      "type": "number",
      "description": "Optional positive safe-integer round cap, bounded by the deployment ceiling."
    }
  },
  "required": [
    "objective"
  ]
}
```

来源：[`packages/workflow/tool-ralph/src/index.ts`](../packages/workflow/tool-ralph/src/index.ts)

固定的前台工作流会在每个 Round 启动一个全新的结构化子级；模型只能选择不可变目标和可选的 Round 上限。

<a id="deepseek-aidsh-tool-skill"></a>

## `@deepseek-ai/dsh-tool-skill`

### `skill`

加载可用 skill（技能）的完整说明。在执行点名某项 skill 或与其明确匹配的任务前，请使用会话 skill 目录中的确切名称调用此工具。

```json
{
  "type": "object",
  "properties": {
    "name": {
      "type": "string",
      "description": "The exact skill name from the available skills list."
    }
  },
  "required": [
    "name"
  ]
}
```

来源：[`packages/skill/tool-skill/src/index.ts`](../packages/skill/tool-skill/src/index.ts)

## `@deepseek-ai/dsh-tool-subagent`

### `list_subagent_models`

查询 subagent 可用的模型；这不会更改你自己的模型。无参数调用会列出提供方；提供 `provider` 时会列出该提供方的模型；同时提供 `provider` 和 `model` 时会查看该模型及其推理强度。这些列表只作参考：允许使用的模型 id 即使未列出也可能可用。把返回的 id 作为委派工具的 `provider`、`model` 与 `reasoning_effort` 传入。

```json
{
  "type": "object",
  "properties": {
    "provider": {
      "type": "string",
      "description": "LLM provider id. Omit to list providers."
    },
    "model": {
      "type": "string",
      "description": "Exact model id to inspect. Requires provider; omit to list that provider's models."
    }
  }
}
```

来源：[`packages/subagent/tool-subagent/src/list-models.ts`](../packages/subagent/tool-subagent/src/list-models.ts)

### `subagent`

将一项自包含任务（例如研究、限定范围的实现或分析）委派给在自身上下文中工作的 subagent，使这些工作不会占满当前对话。你会得到它的结果，而不是其中间步骤。它看不到当前对话，因此请提供完整、独立的提示词。此调用默认等待结果。设置 `run_in_background: true` 可返回 job id；使用 `job_output` 收集结果，使用 `job_kill` 停止任务。

```json
{
  "type": "object",
  "properties": {
    "description": {
      "type": "string",
      "description": "A short (3-5 word) description of the delegated task, for display."
    },
    "prompt": {
      "type": "string",
      "description": "The complete, self-contained task. The subagent does not see this conversation, so include everything it needs."
    },
    "run_in_background": {
      "type": "boolean",
      "description": "Run as a background job and return its job id. Defaults to false; collect with job_output or stop with job_kill."
    }
  },
  "required": [
    "description",
    "prompt"
  ]
}
```

来源：[`packages/subagent/tool-subagent/src/index.ts`](../packages/subagent/tool-subagent/src/index.ts)

注册的委派工具名称取决于加载时 `toolName` 配置（默认为 `subagent`）；上述默认 schema 关闭模型选择，而发现 schema 则展示为已启用 Session 中可用的固定配套工具。Web preset 会在每个新顶层 Session 创建时读取插件页偏好，并为其子 Session 保留该决定；`subagent_fork` 始终使用固定路由。每个实例通过 `modelSelectionSettings`、`backgroundMode` 与 `enableRunInBackground` 独立控制是否读取模型选择设置及其后台行为。

<a id="deepseek-aidsh-tool-subagent-control"></a>

## `@deepseek-ai/dsh-tool-subagent-control`

### `interrupt_agent`

根据 agent id 停止你下方某个可继续 subagent 的当前轮次。它仍可接受后续操作，它自己的 subagent 也会继续运行。此调用只发出停止请求，不等待其生效；中断一个已经完成的 subagent 不会产生任何效果。

```json
{
  "type": "object",
  "properties": {
    "agent_id": {
      "type": "string",
      "description": "The agent id of the subagent to interrupt."
    }
  },
  "required": [
    "agent_id"
  ]
}
```

来源：[`packages/subagent/tool-subagent-control/src/index.ts`](../packages/subagent/tool-subagent-control/src/index.ts)

### `list_agents`

列出你启动的可继续 subagent，以及每个 subagent 的 agent id、标签和状态：running、idle（处于轮次之间）或 ready（已保存、可恢复；不是待收集的结果）。用它查找 id，而不是轮询：subagent 完成时你会收到通知。`descendants` 作用域还会列出更深层的 subagent，附带其父级 id 和深度；只有深度为 1 的条目可以发消息，但 `interrupt_agent` 可以停止其中任何一个。

```json
{
  "type": "object",
  "properties": {
    "scope": {
      "type": "string",
      "description": "children (default) lists your direct subagents; descendants lists every subagent below you.",
      "enum": [
        "children",
        "descendants"
      ]
    }
  }
}
```

来源：[`packages/subagent/tool-subagent-control/src/list-agents.ts`](../packages/subagent/tool-subagent-control/src/list-agents.ts)

### `send_message`

根据 agent id 向你的某个直接可继续 subagent 发送消息。如果你本身是可继续 subagent，也可以向你的父级发送消息。正在工作的目标会在其下一个 step 读取消息；否则，消息会启动一个新轮次。你会收到投递确认，而不是回复；出现错误表示消息未投递。

```json
{
  "type": "object",
  "properties": {
    "agent_id": {
      "type": "string",
      "description": "The agent id of a direct continuable subagent, or of your parent if you are a continuable subagent."
    },
    "message": {
      "type": "string",
      "description": "The message to send."
    }
  },
  "required": [
    "agent_id",
    "message"
  ]
}
```

来源：[`packages/subagent/tool-subagent-control/src/index.ts`](../packages/subagent/tool-subagent-control/src/index.ts)

这些是控制可继续后台 subagent 的全局命名工具：绑定提供方的 `tool-subagent` 实例注册不同的委派工具；本包注册一次 `send_message` 和 `interrupt_agent`，另由 `list_agents` 通过单独加载的 `/list-agents` 插件提供，其目录行使用 sessionProjections 和实时 Agent 注册表。

<a id="deepseek-aidsh-tool-jobs"></a>

## `@deepseek-ai/dsh-tool-jobs`

### `job_kill`

停止正在运行的后台任务。否则，任务在你的轮次结束后仍会继续运行，直至完成。此调用立即返回；任务的工作停止后，其状态会变为 `killed`。

```json
{
  "type": "object",
  "properties": {
    "job_id": {
      "type": "string",
      "description": "The background job id from a start result or job_list. A continuable subagent id is an agent id, not a job id."
    },
    "reason": {
      "type": "string",
      "description": "Optional short reason for stopping the job."
    }
  },
  "required": [
    "job_id"
  ]
}
```

来源：[`packages/jobs/tool-jobs/src/index.ts`](../packages/jobs/tool-jobs/src/index.ts)

### `job_list`

列出你的后台任务（包括正在运行和已完成的任务）及其 id、种类、状态和标签。

```json
{
  "type": "object",
  "properties": {}
}
```

来源：[`packages/jobs/tool-jobs/src/index.ts`](../packages/jobs/tool-jobs/src/index.ts)

### `job_output`

读取后台任务的输出。任务完成时你会收到通知，因此在它运行期间无需轮询或 sleep。流式输出的任务会返回自你上次读取以来的新内容；其他任务会在完成后返回结果。每个回复都以 `[status: ...]` 结尾。除非设置 `wait: true`，否则立即返回。

```json
{
  "type": "object",
  "properties": {
    "job_id": {
      "type": "string",
      "description": "The background job id from a start result or job_list. A continuable subagent id is an agent id, not a job id."
    },
    "wait": {
      "type": "boolean",
      "description": "Wait until the job finishes or the timeout passes, for when you cannot continue without the result. A timed-out wait returns `[status: running]` and leaves the job running."
    },
    "timeout_ms": {
      "type": "number",
      "description": "Maximum wait in milliseconds when wait is true (default 30000, at most 600000)."
    }
  },
  "required": [
    "job_id"
  ]
}
```

来源：[`packages/jobs/tool-jobs/src/index.ts`](../packages/jobs/tool-jobs/src/index.ts)

与任务种类无关的后台任务控制器：后台 bash 命令、PTY 发送和 subagent 都通过相同的 3 个工具读取、列出和终止。加载该插件会挂接控制器，从而启用生产方的 `ctx.jobs.start()`。

## `@deepseek-ai/dsh-tool-todo`

### `todo_write`

为多步骤工作创建并更新用户可见的任务列表。每次调用都会替换整个列表，因此请发送所有条目。工作并行进行时，可以同时有多项处于 `in_progress`。

```json
{
  "type": "object",
  "properties": {
    "todos": {
      "type": "array",
      "items": {
        "type": "object",
        "additionalProperties": false,
        "properties": {
          "content": {
            "type": "string",
            "description": "The task, as a short imperative line."
          },
          "status": {
            "type": "string",
            "enum": [
              "pending",
              "in_progress",
              "completed"
            ]
          }
        },
        "required": [
          "content",
          "status"
        ]
      }
    }
  },
  "required": [
    "todos"
  ]
}
```

来源：[`packages/todo/tool-todo/src/index.ts`](../packages/todo/tool-todo/src/index.ts)

todo_write 是会话所有的状态；UI 将最新的 todo/write 事件渲染为检查清单。`allowParallelInProgress` 是没有默认值的必填项，因此本目录明确选择 `true`，对应描述允许同时存在多个 `in_progress` 项。选择 `false` 的部署会获得同一工具，但描述会要求只能有 1 个活动任务。

<a id="deepseek-aidsh-tool-workflow"></a>

## `@deepseek-ai/dsh-tool-workflow`

### `workflow`

运行一个协调许多 subagent 的 JavaScript 工作流脚本，并返回脚本的结果。仅当用户明确要求使用工作流或进行大规模多 agent 编排时才使用它，因为一次运行可能启动许多 subagent；只需一两次委派时，请使用普通的 subagent 调用。此调用会阻塞，直至脚本完成。编写脚本前，先以 name "workflow" 调用 tool_help 读取脚本参考。

```json
{
  "type": "object",
  "properties": {
    "script": {
      "type": "string",
      "description": "Body of an async JavaScript function, so `await` works at the top level. Plain JavaScript only: no TypeScript and no import or export statements. Its return value, which must be JSON-serializable, becomes the tool result; results longer than 50000 characters are truncated."
    },
    "meta": {
      "type": "object",
      "description": "The workflow's identity, as JSON data.",
      "additionalProperties": true,
      "properties": {
        "name": {
          "type": "string",
          "description": "Short kebab-case workflow name."
        },
        "description": {
          "type": "string",
          "description": "One-line description of what the workflow does."
        },
        "whenToUse": {
          "type": "string",
          "description": "Optional note on when this workflow applies."
        },
        "phases": {
          "type": "array",
          "description": "Optional list of the phases the script enters with phase(); informational only.",
          "items": {
            "type": "object",
            "additionalProperties": true,
            "properties": {
              "title": {
                "type": "string",
                "description": "The title the script passes to phase()."
              },
              "detail": {
                "type": "string",
                "description": "Optional one-line description of the phase."
              },
              "provider": {
                "type": "string",
                "description": "Informational; pass `provider` to agent() to route a subagent."
              },
              "model": {
                "type": "string",
                "description": "Informational; pass `model` to agent() to choose a subagent's model."
              }
            },
            "required": [
              "title"
            ]
          }
        }
      },
      "required": [
        "name",
        "description"
      ]
    },
    "args": {
      "type": "object",
      "description": "Optional JSON object available to the script as the global `args`.",
      "additionalProperties": true
    }
  },
  "required": [
    "script",
    "meta"
  ]
}
```

来源：[`packages/workflow/tool-workflow/src/index.ts`](../packages/workflow/tool-workflow/src/index.ts)

<a id="deepseek-aidsh-tool-web"></a>

## `@deepseek-ai/dsh-tool-web`

### `web_fetch`

获取 HTTP(S) URL，并以文本形式返回其内容。这些内容来自外部的不可信页面；使用时请以 markdown 链接形式引用该 URL。

```json
{
  "type": "object",
  "properties": {
    "url": {
      "type": "string",
      "description": "The HTTP(S) URL to fetch."
    }
  },
  "required": [
    "url"
  ]
}
```

来源：[`packages/web/tool-web/src/index.ts`](../packages/web/tool-web/src/index.ts)

### `web_search`

在 Web 上搜索最新信息。返回可选的摘要答案，以及来自外部不可信页面的来源 URL；如需阅读完整页面，请使用 web_fetch。

```json
{
  "type": "object",
  "properties": {
    "queries": {
      "type": "array",
      "description": "1–4 non-empty search queries; their results are merged.",
      "items": {
        "type": "string"
      }
    }
  },
  "required": [
    "queries"
  ]
}
```

来源：[`packages/web/tool-web/src/index.ts`](../packages/web/tool-web/src/index.ts)

web_search 和 web_fetch 将提供方选择置于 ctx.web 之后，使模型可见 schema 在更换后端时保持稳定。
