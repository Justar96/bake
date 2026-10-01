---
description: "shell 命令运行期间改动的工作区文件，供 bash 与 pwsh 工具的维护者，以及想了解 shell 编辑如何以差异形式出现在终端中的读者阅读。"
kind: "package-library"
---

# @deepseek-ai/dsh-shell-change-report

[English](README.md) | 中文

## 概要

`dsh-shell-change-report` 找出 shell 命令运行期间改动的工作区文件，使终端能像显示 `edit` 调用那样显示 shell 编辑。它在命令前后用经过加固、有时间上限的方式读取 git，返回改动的文件及其上下文 hunk，并加以限制，使会话日志保持精简。该报告仅供展示：shell 工具把它附加为 `tool/result.meta`，模型永远收不到。`tool-bash` 和 `tool-pwsh` 使用它；它不注册服务，也没有 `ctx` 键。

## 目录

- [使用本包](#use-this-package)
- [理解实现](#understand-the-implementation)
- [延伸阅读](#further-exploration)
- [模型体验](#model-experience)
- [已知限制与延后工作](#known-limitations-and-deferred-work)
- [开发笔记](#dev-note)

-----

<a id="use-this-package"></a>
## 使用本包

shell 工具在运行前台命令之前打开一个窗口，之后关闭它。`openChangeReport` 根据调用的文件策略决定是否报告该调用，以及如何约束 git 读取。`finish` 返回报告或 `undefined`；`release` 在不比较的情况下关闭窗口，被中止的命令就是这样。

```ts
import { openChangeReport, withRunningJobs } from '@deepseek-ai/dsh-shell-change-report'

const window = await openChangeReport(ctx, exec, policy, workdir)
try {
  const result = await ctx.shell.run(spec)
  const changes = await window?.finish(exec.signal)
  if (changes !== undefined) exec.presentResultMeta({ shellChanges: withRunningJobs(ctx, exec, changes) })
} finally {
  await window?.release()
}
```

工具的 `presentResult` 用 `shellChangesOf(result.meta)` 读回记录的报告，再用 `terminalChanges` 把它转换为终端卡片的改动区。格式错误或版本更新的元数据视为没有报告，因此重放的会话不会渲染失败。

### 哪些调用会被报告

| 调用 | 报告 |
|---|---|
| git 工作区中顶层的前台调用，不受约束 | git 读取直接运行 |
| 同上，处于 `workspace-write` 下 | git 读取通过 `ctx.sandbox` 在 `read-only` 约束下运行；没有提供方时不报告 |
| 处于 `read-only` 下 | 不报告：命令无法写入工作区 |
| 后台任务、嵌套的 PTC 调用或没有 agent | 不报告 |
| git 之外的工作区 | 不报告 |

### 报告内容

`ShellChanges` 带有 `version: 1`、按路径排序的改动文件，以及可选的 `omittedFiles`、`timedOut`、`concurrent` 和 `headChanged`。每个文件带有相对于会话工作目录的路径、状态、新增和删除的行数、重命名前的路径，以及 `edit` 卡片绘制的 hunk 结构。状态为 `created`、`modified`、`deleted`、`renamed`、`mode`、`symlink`、`binary`、`too-large` 或 `unknown-before` 之一。`concurrent` 表示同一仓库中另一个报告的窗口与本窗口重叠，或调用方有正在运行的后台任务。报告说明的是命令运行期间改动了什么，其中可能包含其他写入者的改动。

### 上限

`DEFAULT_CHANGE_REPORT_LIMITS` 存放默认值。每个上限都作用于完整序列化后的报告，因为会话日志会保留它：

| 上限 | 默认值 | 超出时 |
|---|---|---|
| 命令前的时间 | 200 ms | 不报告 |
| 命令后的时间 | 750 ms | 已找到的文件，并标记 `timedOut` |
| 带 hunk 的文件 | 20 | 只保留路径和状态 |
| 列出的文件 | 200 | `omittedFiles` |
| 读取内容的改动路径 | 1,000 | 只保留路径和状态 |
| 保存的已有改动文件 | 256 个文件，16 MiB | `unknown-before` |
| 读取的文件大小 | 1 MiB | `too-large` |
| 每个文件的 hunk 文本 | 64 KiB | 只保留计数 |
| 序列化后的报告 | 256 KiB | 从最大的文件开始丢弃 hunk，然后丢弃文件 |
| 差异计算时间 | 每个文件 100 ms，总计 400 ms | 只有状态，没有 hunk |

命令前连续两次超时后，该工作目录在十分钟内会被跳过。

-----

<a id="understand-the-implementation"></a>
## 理解实现

<details>
<summary>实现细节——点击展开</summary>

### 源码地图

| 文件 | 作用 |
|---|---|
| [`src/index.ts`](src/index.ts) | `beginChangeReport`、比较、分类、重命名配对、上限以及 `shellChangesOf` |
| [`src/git.ts`](src/git.ts) | 加固的 git 读取器和 `status --porcelain=v2 -z` 解析器 |
| [`src/hunks.ts`](src/hunks.ts) | 有时间上限的上下文 hunk |
| [`src/tool.ts`](src/tool.ts) | shell 工具的衔接代码：按策略约束、运行中任务标记以及终端卡片的改动区 |
| — | 不发布运行时不变量配套；除每个进程内的窗口登记表外，本库不拥有事件流或可变运行时数据，该登记表由其测试覆盖。 |

### 比较如何进行

命令之前，本库把仓库的索引复制到临时目录，并对这份副本运行 `git status --porcelain=v2 --branch -z --untracked-files=all --no-renames --ignore-submodules=all`。它为每个与索引已有差异的路径记录 `lstat` 签名，并在上限内保存这些文件的字节。命令之后，它对同一份副本运行相同的 status。由于比较基准是命令前的索引，命令中的提交、stash 或检出无法掩盖改动。签名不变的已有改动路径会被跳过。原本干净的路径通过不应用任何过滤器的 `git cat-file blob` 从其索引 blob 取得改动前内容。内容相同的一个删除文件和一个新建文件报告为一次重命名。在本仓库上（有 70 个已改动文件），不改动任何文件的调用在中位数上约增加 33 ms。

### 加固

git 读取传入 `-c core.fsmonitor=false` 和 `--no-optional-locks`，设置 `GIT_OPTIONAL_LOCKS=0`、`GIT_TERMINAL_PROMPT=0` 和 `LC_ALL=C`，并清除会把读取指向其他仓库的变量。它们从不运行 `git add`、`--filters` 或 diff 驱动。`git status` 仍会运行仓库的 clean 过滤器，而沙箱中的命令可能植入了它们，因此在受约束的策略下，读取本身也在约束下运行。

### 为什么 git 不经由 `ctx.subprocess` 运行

在 Linux 上，`ctx.subprocess` 会把每个进程放进自己的 systemd scope。实测每次启动约耗时 250 ms，而一次 git 读取只需 2 到 15 ms，且每条命令前后都要读取多次。因此读取直接启动 git。每次读取仍然拥有自己的进程：POSIX 子进程领导自己的进程组，截止时间或调用取消会终止该进程组，读取只有在子进程关闭后才结束。环境变量以共享的凭据清理函数 `scrubbedParentEnv()` 为基础。

### 为什么 hunk 有时间上限

jsdiff 在差异很大的输入上是平方复杂度：完整改写 10,000 行耗时 9.7 s，并阻塞事件循环。`boundedHunks` 传入 jsdiff 的 `timeout`，差异计算超时的文件不带 hunk 报告。

</details>

-----

<a id="further-exploration"></a>
## 延伸阅读

- [shell 改动报告 Agent Note](../../../.agents/notes/implemented/feature/2026-10-01-shell-change-report.zh.md)——设计、它胜过的替代方案及其风险。
- [`dsh-tool-bash`](../tool-bash/README.zh.md) 与 [`dsh-tool-pwsh`](../tool-pwsh/README.zh.md)——在每次前台调用前后打开窗口的工具。
- [`dsh-tools`](../../core/tools/README.zh.md#host-presentation-descriptors)——`presentResultMeta`，报告经由的通道。

-----

<a id="model-experience"></a>
## 模型体验

无。报告是 `tool/result.meta`，会话的模型历史投影不包含它，因此无论有没有它，模型的请求都逐字节相同。

#### Token 影响

无。

#### KV Cache 影响

无。

<a id="known-limitations-and-deferred-work"></a>
## 已知限制与延后工作

这些限制界定了报告有意不覆盖的范围。它们是当前的包约束，而不是任务清单。

- **归属而非因果。**用户的编辑器、另一个 agent 或在命令之前启动的进程都可能出现在报告中。只有重叠的报告窗口和调用方的后台任务会被检测并标记。
- **仅限 git 工作区。**git 之外的工作区、被忽略的文件以及子模块和嵌套仓库的内容都不会被报告。
- **并非每次调用。**后台任务、嵌套在 `run_code` 中的 PTC 调用以及 `tool-bash-persistent` 的调用都不会被报告。
- **超出保存预算的文件**报告为 `unknown-before`，不带 hunk。

<a id="dev-note"></a>
### 开发笔记

<details>
<summary>给维护者的工作背景——点击展开</summary>

本开发笔记是给维护者的工作背景：未解决的问题和尚未决定的方向。它不具权威性；已交付的行为和限制以上文各节和包代码为准。

- 在后台任务结束时生成报告需要一个独立的持久事件，因为该任务的 `tool/result` 在任务启动时就已提交。
- git 之外的工作区可以用文件列表和签名进行比较，代价是失去已修改文件的改动前内容。

</details>
