# Agent Note: 显示 shell 命令改动的文件

Status: implemented

[English](2026-10-01-shell-change-report.md) | 中文

## 问题

模型经常不用 `edit`，而是通过 shell 修改文件：`sed -i … && node test.cjs`，或者用 Python heredoc 改写多个文件并断言每处替换。在本机最近十个会话中，有 103 次 `bash` 调用这样修改了文件，其中 92 次来自 Claude Opus。更早的[基于内容锚定的编辑](2026-09-30-content-anchored-edits.zh.md)评审在 75 个会话中统计到 384 次这类编辑。一次配对探测表明，这条路径不会多出往返次数，因为 Claude 把编辑和测试串在同一次调用里。让模型优先使用 `edit` 的指引改变了 Opus 选用的工具，但没有改变其请求次数或成功率，因此 Bake 把选择留给模型。

代价落在用户身上。`edit` 调用会显示差异卡片，shell 编辑却只显示命令输出。用户不运行 `git diff` 就看不到改了什么，非零退出时连有文件被改动这件事都会被掩盖。

## 决定

在 git 工作区中，每次前台的顶层 `bash` 或 `pwsh` 调用前后，工具记录工作区状态、运行命令并进行比较。它把改动的文件及有界的 hunk 作为仅供展示的 `meta`，附加到该调用自己的 `tool/result` 上。终端卡片在命令输出下方把它们绘制为改动区。无论报告开启与否，模型可见的结果文本、规范 `value`、工具 schema 和描述都逐字节相同，因此模型的输入不变。报告存放在现有的不透明字段 `tool/result.meta` 中，因此 Session 格式不变。

### 仅供展示的结果元数据

过去没有任何钩子能让工具附加不由其规范 `value` 派生的 `meta`：

- `output.presentationMeta` 是 `(args, value)` 的纯函数。
- `normalizeDispatchResult` 重建结果时，`tools/execute` 包装器在成功结果上的 `meta` 会被丢弃。
- `PostToolDecision` 没有 `meta` 字段。

把报告放进 `value` 会使它成为模型输入，因为 PTC SDK 提示词会渲染每个工具的 `output.schema`，PTC 程序也会收到 `value`。`ToolRunContext.presentResultMeta(meta)` 填补了这个空缺。注册表快照该值，并在 `createSuccessResult` 计算 `meta` 的位置应用它，且仅限顶层调用。它在 post-execute 替换值或内容后依然保留。失败的结果从不携带它。声明了 `output.presentationMeta` 的工具不能调用它，因此一个结果永远不会有两个元数据来源。

### 检测

[`dsh-shell-change-report`](../../../../packages/shell/shell-change-report/README.zh.md) 是一个库，不提供服务，也没有 `ctx` 键。命令之前，它把仓库的索引复制到临时目录，并对这份副本运行 `git status --porcelain=v2 --branch -z --untracked-files=all --no-renames --ignore-submodules=all`。它为每个与索引已有差异的路径记录 `lstat` 签名，并在上限内保存这些文件的字节。命令之后，它对同一份副本运行相同的 status，因此命令中的提交、stash 或检出无法掩盖改动。原本干净的路径通过不应用任何过滤器的 `git cat-file blob` 从其索引 blob 取得改动前内容。hunk 由带 `timeout` 的 jsdiff 计算，因为没有它时改写 10,000 行耗时 9.7 s。

读取直接启动 git，而不经由 `ctx.subprocess`。在 Linux 上，subprocess 服务的约束机制会把每个进程放进自己的 systemd scope，实测每次启动约 250 ms，而一次读取只需 2 到 15 ms。每次读取仍然拥有自己的进程。POSIX 子进程领导自己的进程组，截止时间或调用取消会终止该进程组，读取只有在子进程关闭后才结束。环境变量以共享的凭据清理函数 `scrubbedParentEnv()` 为基础。在本仓库上（有 70 个已改动文件），不改动任何文件的调用在中位数上多耗时 33 ms。

这些上限作用于完整序列化后的报告，每个上限降级的都是报告而不是命令：

- 命令前 200 ms，命令后 750 ms；
- 20 个带 hunk 的文件，列出 200 个文件；
- 每个文件读取 1 MiB；
- 每个文件 64 KiB 的 hunk；
- 每份报告 256 KiB；
- 每个文件 100 ms、每次调用 400 ms 的差异计算。

命令前连续两次超时后，该工作目录在十分钟内会被跳过。

### 加固与约束

`git status` 会执行部分仓库本地配置。在 git 2.43 的测试中，它运行了植入的 `core.fsmonitor` 命令，并对改动的文件运行了植入的 clean 过滤器。每次读取都传入 `-c core.fsmonitor=false` 和 `--no-optional-locks`。读取从不使用 `git add`、`--filters` 或 diff 驱动。status 期间 clean 过滤器仍会运行，因此读取遵循调用的实际文件策略：

| 策略 | 读取 |
|---|---|
| `workspace-write` | 通过 `ctx.sandbox` 在 `read-only` 约束下运行。没有提供方时不生成报告。 |
| `read-only` | 不读取：命令无法写入工作区。 |
| `danger-full-access`，或没有沙箱执行器 | 直接运行。它们触及的范围不超出命令本身。 |

同一发现也暴露了状态栏的问题。它每两秒在沙箱之外轮询一次 `git status`，因此沙箱中的命令可以植入一个钩子，随后由这次轮询在无约束的情况下运行。对受约束的会话，该轮询现在在同样的 `read-only` 约束下运行，并关闭 fsmonitor 钩子。

### 一次调用

1. 只有在 `changeReport` 开启、策略允许读取时，才对有 agent 的前台顶层调用做快照。
2. 先运行升权审批，使漫长的等待不会扩大时间窗口。
3. 做命令前快照并运行命令；命令被中止时释放窗口，并像以前一样抛出 `TOOL_ABORTED`。
4. 否则进行比较，并通过 `presentResultMeta` 附加 `{ shellChanges }`。
5. 在 `finally` 中释放临时索引。

报告只在追加 `tool/result` 时才变得可见。`concurrent` 表示该窗口与同一仓库中另一个报告的窗口重叠，或调用方有正在运行的后台任务。

### 终端展示

`TerminalResultView` 有一个可选的 `changes` 字段。终端卡片为每个文件绘制一行 `edited <path>`，改动不是普通编辑时附上状态词，再像 `edit` 卡片那样绘制该文件带编号的 `-` 和 `+` 行。标题行在退出状态之前显示总计 `+N −M`。

输出和改动分别计算上限。非零退出把输出染成红色，而改动保留自己的色调。来自磁盘的差异行与工具输出经过同样的净化。`edit` 卡片的差异行此前跳过了净化，现在也会经过它。不带 `changes` 的结果渲染与以前完全相同。

### 配置

`tool-bash` 和 `tool-pwsh` 接受 `changeReport: boolean`，默认 `true`。上限保持为库的默认值，因为没有使用方需要调整它们。

### 范围

只有 git 工作区中顶层的前台 `bash` 和 `pwsh` 调用会被报告。以下不在范围内：

- 后台任务，其 `tool/result` 在任务启动时就已提交；
- 嵌套在 `run_code` 中的 PTC 调用；
- `tool-bash-persistent`；
- git 之外的工作区；
- 被忽略的文件，以及子模块和嵌套仓库的内容；
- 任何模型可见的摘要。

### 与现有决策的关系

本决策部分取代两项已实现的决策，两者都保持有效并相互链接。

- **[规范工具输出契约](../architecture/2026-07-20-canonical-tool-output-contract.zh.md)。**其中文件修改从 `args` 和规范 `value` 派生差异元数据，“而不是从工具主体返回 UI 状态”。shell 命令的改动前状态不属于其结果，而把它放进 `value` 在 PTC 模式下会使它成为模型输入。因此 `presentResultMeta` 是一个窄的例外：来自工具主体、仅供展示的元数据，且仅限顶层调用。能够从 `value` 派生元数据的工具继续使用 `output.presentationMeta`。
- **[带标签的渲染意图联合类型](../architecture/2026-07-02-tool-render-intent-union.zh.md)。**该 Agent Note 把“终端卡片不能携带差异”列为联合类型排除的非法状态之一。命令同时改动了文件是合法状态，因此 `TerminalResultView` 携带可选的 `changes`。联合类型保持封闭。

## 曾考虑的替代方案

- **引导模型使用 `edit`：**在配对探测中，一行 persona 指令和旧的描述语句都没有改变请求次数或成功率，而且 Bake 选择不约束模型。
- **把报告放进 bash 的 `value` 并使用 `output.presentationMeta`：**在 PTC 模式下，`value` 和 `output.schema` 都是模型输入。这会改变模型的输入、需要评估，并把 UI 词汇带进模型契约。
- **用一个包裹 `tools/execute` 的独立插件，并给 post-execute 增加 `meta` 字段：**这会为单一能力增加公开接口，需要把状态从包装器带到 post-execute，使监听器顺序变得重要，还会让一个插件写入另一个工具的私有 `meta`。它也会匹配注册了同名工具的 `tool-bash-persistent`。
- **新增日志事件，例如 `shell/changes`：**它需要变更记录和一个可忽略的事件类型。transcript 会把它打印在对应调用上方，UI 还得把两个来源合并成一张卡片。
- **在 `ctx.shell` 执行器中做快照：**执行器有四个提供方和多个使用方，这样会把 UI 关注点放进共享的服务定义。
- **经由 `ctx.subprocess` 启动 git：**在 Linux 上每次读取约耗时 250 ms，而每条命令要读取多次。
- **mtime 和哈希扫描：**在本仓库上每次扫描需 340 到 390 ms，没有忽略规则，也没有改动前内容。
- **递归文件监视器：**启用约需 800 ms，常驻内存 305 MB。它只报告路径，并且在一次涉及 5,000 个文件的 `sed -i` 下静默丢失了事件。
- **解析命令：**会漏掉脚本、`python -c`、格式化工具、代码生成、`git checkout` 和变量。
- **用 `git add -A` 构建影子索引：**它对重命名的处理最好，但会运行 clean 过滤器，还改动了真实对象库的时间戳。
- **在 TUI 中做快照：**重放时无法重建结果，而 transcript 渲染的是已记录的事件。
- **合成的 `Edit` 行：**这会凭空制造日志中不存在的调用，并使该步骤的工具计数失真。

## 影响

shell 编辑现在和 `edit` 一样可见，包括命令失败之后，而模型的请求不变。这项工作还发现并修复了状态栏 git 轮询中的一处沙箱逃逸。

记录说明的是命令运行期间改动了什么，而不是命令做了什么。用户的编辑器、正在运行的进程或同一工作区中的另一个 agent 都可能出现在其中。只有重叠的报告窗口和调用方的后台任务会被检测并标记。

大型仓库、网络或 NTFS 文件系统以及 racy 索引条目都会让 status 变慢。预算限制了增加的时间，慢仓库会失去报告，而不是让命令变慢。

报告持久化在 `meta` 中，而压缩修剪会重新追加事件数据，因此被修剪结果的报告会存两份。256 KiB 的上限约束每一份。

在 agent 可写的仓库上运行 git 仍是一类风险。参数和约束封堵了目前发现的途径；面对将来可能在 status 期间执行的 git 配置项，主要防线是约束，而不是参数。

后台任务、PTC 程序、持久 shell、git 之外的工作区以及被忽略文件中的改动仍然不可见。`edit` 和 `write` 的差异计算曾有同样的无上限开销；`tool-fs` 中的 `computeHunkDiffs` 现在在改动超过 1,000 行后停止计算，并报告从第一处差异行到最后一处差异行的一个块。

## 测试

- [`shell-change-report/tests/change-report.spec.ts`](../../../../packages/shell/shell-change-report/tests/change-report.spec.ts) 运行真实的 git 仓库，覆盖：
  - 原地修改、新建、删除、重命名、模式和二进制改动；
  - 与保存字节比较的已有改动文件；
  - 命令自己提交的编辑；
  - 无变化的改写、被忽略的输出以及 git 之外的工作区；
  - 上限和有时间上限的差异计算；
  - 植入的 fsmonitor 钩子，以及未被写入的 `.git`；
  - 约束路由、重叠的窗口和释放。
- [`tool-bash/tests/integration.spec.ts`](../../../../packages/shell/tool-bash/tests/integration.spec.ts) 通过 agent 循环驱动真实的 bash 工具。它表明报告会被记录，模型请求中不含任何报告内容，并且后台调用、`changeReport: false` 和未改动的工作区都不会生成报告。
- [`core/tools/tests/tools.spec.ts`](../../../../packages/core/tools/tests/tools.spec.ts) 固定了 `presentResultMeta` 的行为：仅限顶层、会被快照、在 post-execute 替换后保留，失败时不存在。
- [`apps/tui/packages/app/tests/git.spec.ts`](../../../../apps/tui/packages/app/tests/git.spec.ts) 表明，受约束的状态栏读取会把植入的 clean 过滤器限制在真实的 bwrap 约束之内，而被拒绝的包装器会让该字段为空。
- `shell-edit` PTY 场景在默认的 `workspace-write` 策略下，通过已构建的 profile 运行一次录制的 `sed -i` 和一条失败的命令。它表明改动被绘制出来、磁盘上的文件已被编辑，并且 `--resume` 绘制出相同的行。
