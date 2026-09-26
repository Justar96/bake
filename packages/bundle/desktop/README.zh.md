---
description: "长驻桥接：让 Bake Desktop 应用驱动一个 dsh Agent，提供流式输出、转发审批和三档权限，面向桌面集成方。"
kind: "package-bundle"
---

# @deepseek-ai/dsh-desktop

[English](README.md) | 中文

## 概述

`dsh-desktop` 是 Bake Desktop 应用启动的 bundle：`dsh --profile desktop` 为一个工作区保持一个长驻进程，驱动一个根 Agent，并与桌面应用交换协议消息。它流式发送助手文本，报告工具调用和 token 用量，把每个审批问题转发到桌面的审批卡片，并应用桌面选择的权限档位（只读、普通或完全访问）。它还上报 harness span，使桌面 trace 能从用户点击一路跟到模型请求和每次工具调用。边界：每个进程一个根 Agent，只由桌面协议驱动。

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

桌面应用以工作区为工作目录启动进程，并使用 [`src/protocol.ts`](src/protocol.ts) 定义的协议通信。你很少需要手动运行它；想试用时，把协议行通过管道送入该 profile。

### 启动桥接

```sh
cd /path/to/workspace
printf '%s\n' \
  '{"v":1,"type":"init","workspace":"'"$PWD"'","permission":"normal"}' \
  '{"v":1,"type":"user.message","text":"list the files here"}' \
  | dsh --profile desktop
```

在 Electron 下，桌面应用在 utility 进程中启动同一个入口，每条消息作为一个字符串通过该进程的消息端口传递。没有该端口时，桥接从 stdin 读取、向 stdout 写入换行分隔的 JSON，测试和基准测试就是这样驱动它的。组合就绪后它发送 `ready`，用 `initialized` 回应 `init`，并在 stdin 关闭或收到 `shutdown` 时退出。

### 权限档位

| 档位 | 预设 | 需要询问 | 被拒绝 |
|---|---|---|---|
| `read-only` | `read-only` | 无 | 文件写入、沙箱升级，以及读取、子 Agent 和 shell 集合之外的工具 |
| `normal` | `workspace-write` | shell 命令，以及读取、写入和子 Agent 集合之外的工具 | 沙箱之外无额外拒绝 |
| `full-access` | `danger-full-access` | 无 | 无 |

桥接通过 `permissionPresets.set` 把档位对应的预设固定到 Session 上，`permission.set` 会为之后的调用更改它。在 `read-only` 下 shell 命令仍会运行，但沙箱把它限制在只读文件系统中。在 `normal` 下，读取、工作区内写入和子 Agent 工具无需询问即可运行。

### 审批

根 Agent 的每个审批问题都会变成一条 `approval.request`，带有一行摘要（shell 工具为命令本身），适用时还带有会话授权键：简单命令为 `shell:<program>`，其他工具为 `tool:<name>`。复合命令（管道、命令列表、命令替换、重定向）和沙箱升级不带授权键。桌面回答 `approve_once`、`approve_session` 或 `deny`；两种批准都映射为 `allowed-once`，因为会话授权由桌面保存，之后匹配的请求由桌面自行回答。若调用先被取消，请求会以 `approval.withdrawn` 撤回。

### 何时使用

当应用拥有对话和用户界面时使用此 profile：它需要流式文本、可以展示的审批问题，以及可切换的权限档位。脚本中执行单个任务请用 `dsh-headless`，交互使用请用终端 profile。

-----

<a id="understand-the-implementation"></a>
## 理解实现

<details>
<summary>实现细节——点击展开</summary>

### 运行流程

桥接在挂载时打开传输通道，等待 Loader 就绪后发送 `ready`。`init` 检查工作区是否等于进程的工作目录并记录档位。第一条 `user.message` 创建根 Agent（或恢复 `resume_session_id`），固定档位预设并报告 `session.started`；之后每条消息都是一次 `followup`。助手增量按消息合并 16 ms 后再发送。

### Span

`bake.turn` 以开启该轮次的用户消息所带的 `traceparent` 为父级。`bake.step`、`llm.request`（含 `ttft_ms`）、`tool.pipeline`、`tool.execute`（通过包装 `tools/execute` 只测量工具主体）和 `approval.wait` 嵌套在其中。span 每秒上传一次，并在每条 `turn.done` 之前上传。

### 源码地图

| 文件 | 职责 |
|---|---|
| [`src/index.ts`](src/index.ts) | 桥接插件：入站处理、Agent 生命周期、投影、审批、span |
| [`src/permission.ts`](src/permission.ts) | 档位预设和逐调用分类器 |
| [`src/transport.ts`](src/transport.ts) | 消息端口和 stdio 传输通道及入站信封校验 |
| [`src/spans.ts`](src/spans.ts) | span 记录器和 `traceparent` 解析 |
| [`src/protocol.ts`](src/protocol.ts) | 协议，从桌面仓库逐字节复制 |

### 不变式归属

本包不发布运行时不变量伴随模块；桥接不拥有自己的持久 Session 事件，其出站投影和审批转发由真实组合 spec 覆盖。

</details>

-----

<a id="further-exploration"></a>
## 进一步探索

- [`@deepseek-ai/dsh-headless`](../headless/README.zh.md) — 本桥接所参照的一次性运行器。
- [`@deepseek-ai/dsh-permission-presets`](../../interaction/permission-presets/README.zh.md) — 各档位选用的预设。
- [`@deepseek-ai/dsh-user-approval`](../../interaction/user-approval/README.zh.md) — 桥接所回答的审批服务。

-----

<a id="model-experience"></a>
## 模型体验

### 人设前缀

#### 模型看到什么

本 bundle 用下面的文本替换基础人设前缀，其中 `{{model}}` 是所选模型 id。

##### 人设前缀原文

```markdown
You are a coding agent powered by the {{model}} model, working for a user in the Bake Desktop app. Some tool calls wait for the user's approval before they run.
```

#### Token 影响

系统提示中一段约 35 token 的固定句子，替代基础前缀。

#### KV Cache 影响

前缀稳定：只有所选模型 id 变化时文本才会改变。

### 档位拒绝

#### 模型看到什么

在 `read-only` 下，被拒绝调用的工具结果带有原因 `<tool> is not available in read-only mode`。

#### Token 影响

每次被拒绝的调用产生一条简短结果。

#### KV Cache 影响

仅追加：该结果像其他工具结果一样加入历史。

## 已知限制与延期工作

<a id="known-limitations-and-deferred-work"></a>

- **每个进程一个根 Agent** — 桌面为每个工作区会话启动一个进程；第二个对话需要第二个进程。
- **子 Agent 无法询问** — 子 Agent 的审批策略为 `never`，因此在 `normal` 下子 Agent 的 shell 命令会被拒绝，而不会到达桌面。
- **不投影子 Agent** — 只有根 Agent 的流、工具调用和审批会到达桌面。
- **没有用户问题应答者** — `exit_plan_mode` 和 `ask_user_question` 会因没有提供者而失败，因为桥接尚未转发 `user-questions/request`。

<a id="dev-note"></a>
### 开发备注

<details>
<summary>维护者的工作上下文——点击展开</summary>

`src/protocol.ts` 复制自桌面仓库的 `src/shared/protocol.ts`，必须保持逐字节一致；当两个检出同时存在时，桌面测试会比较这两个文件。

</details>
