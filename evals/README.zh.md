# Agent 循环评估

[English](README.md) | 中文

每个 Bake 版本都会记录其 agent 循环在一组固定任务上的开销，并与上一个版本对比测量，让回归以数字而不是印象的形式呈现。记录来自一次配对的真实运行：各检出的已构建 headless CLI 通过同一个模型网关运行相同的场景，各组交错执行，使网关与缓存的漂移对双方的影响相同。

## 布局

```text
evals/agent-loop/
  run.ts          paired runner: arms, scenarios, fixtures, wire capture
  record.ts       raw output to a committed record and a regression check
  accounting.ts   provider usage normalization
  versions/
    v0.2.0/release/                       a release's own baseline
    unreleased/<YYYY-MM-DD>-<topic>/      one change since the last release
      REPORT.md  summary.json  samples.jsonl
      steps/<n>-<name>/                   optional intermediate measurements
```

`versions/<tag>` 存放某个发行版的记录，`versions/unreleased` 存放尚未发行的变更的记录。发布新版本时，在发行提交中把 `unreleased` 重命名为新标签，与变更日志的 `[Unreleased]` 小节被重命名的方式相同。`REPORT.md` 是可读的结果，`summary.json` 以供工具使用的形式携带相同的对比，`samples.jsonl` 每次运行一行计数：不含对话记录、文件内容或 stderr。

## 何时记录

凡是可能改变模型所见内容、或改变一项任务所需往返次数的变更，都要记录一次评估。这包括提示词与 persona；工具名称、schema、描述、参数、结果与错误；上下文组装、压缩与缓存；agent 循环；以及 LLM 适配器。只涉及终端 UI、文档或测试的变更不需要记录。

## 运行

为基线（PR 的基线或上一个发行版）和候选分别构建干净的 worktree。检出有未提交修改时，记录会标出。

```sh
git worktree add --detach .agents/worktrees/eval-base <base-commit>
(cd .agents/worktrees/eval-base && bun install --frozen-lockfile && bun run build)
bun run build
EVAL_ARMS=base=.agents/worktrees/eval-base,candidate=. \
EVAL_MODELS=claude-sonnet-5-5 EVAL_OUTPUT=.preflight/evals/agent-loop/<topic>/claude-sonnet-5-5 \
  bun run eval
```

每个模型运行一个进程，并行执行。运行器从 `~/.bake/settings.yaml` 读取 `cliproxyapi` 路由，并把 `~/.bake/.credentials.yaml` 复制到每个样本的私有主目录，因此记录需要本机具备这两者；二者都不会写入输出。原始输出（包括对话记录和捕获的请求）保留在被忽略的 `.preflight/` 下。标准套件在三个模型、每项三次试验下约需 40 分钟和 400 万 token。

| 变量 | 含义 |
|---|---|
| `EVAL_ARMS` | `name=checkout` 对，两个或更多 |
| `EVAL_MODELS` | `cliproxyapi` 模型 id |
| `EVAL_CASES` | 场景；默认是下面的标准套件 |
| `EVAL_TRIALS` | 每个场景的试验次数，默认 3 |
| `EVAL_OUTPUT` | 原始输出目录 |
| `EVAL_EXTRA_<ARM>` | 为某一组追加的覆盖行 JSON 数组，用于归因组，例如 `[{"id":"fs-observation-policy","config":{"editGuard":"version"}}]` |

| 场景 | 检查内容 |
|---|---|
| `no_tools` | 固定前缀：首次请求字节数与计费输入 |
| `ordinary_edit`、`unprompted_edit` | 一行修复，分别在提示中点名与不点名工具 |
| `path_discovery` | 在 40 个干扰项中找到被移动的模块 |
| `stale_edit` | 读取后有外部写入者修改文件 |
| `multi_site_edit`、`multi_file_edit` | 同一文件内多处修改，以及跨两个文件的修改 |
| `shell_then_edit` | 编辑前有一个 shell 步骤改写了文件 |
| `workflow_script` | 运行两个 subagent 的 `workflow` 脚本 |

当 agent 正常退出且外部检查通过时，样本即为成功。该检查是 `no_tools` 的精确回复、fixture 的 `node test.cjs` 在测试文件未被修改的情况下通过，或得到预期的 `summary.txt`；场景注入了内容时，还必须保留该内容。

## 记录

```sh
bun run eval:record --raw .preflight/evals/agent-loop/<topic> \
  --out evals/agent-loop/versions/unreleased/<YYYY-MM-DD>-<topic> \
  --arm base=<base label>,candidate=<candidate label> --candidate candidate --base base \
  --title '<one line>' --note '<what changed and anything a reader must know>'
```

标签说明某一组测量的是什么：例如 `v0.2.0` 这样的标签，未发行提交用 `<tag>+<short commit>`，或者一个步骤名。把记录与它所测量的变更一起提交，或在同一分支上紧随其后提交；其 `summary.json` 会写明确切的提交。

## 回归

对某一个模型的全部任务，`record.ts` 在以下情况下标记回归：

- 总 token 上升，且整个 95% 区间都在零以上；
- 候选失败的运行比基线至少多两次；或
- 工具错误增加。

`--fail-on-regression` 会让脚本以非零状态退出。被标记的回归要么在合并前修复，要么在记录的 `--note` 与 PR 中说明其原因。有些回归是变更有意付出的代价，例如 `workflow_script` 为读取 `tool_help` 所付出的开销。只能通过配对运行比较两个版本：不同日期运行得到的绝对数值会随网关、提供方缓存和模型更新而漂移。

## 规则

- 不要编辑或删除已提交的记录；只有在发行时重命名 `unreleased` 才移动记录。错误的记录由一条新记录取代，并在其说明中写明原因。`--replace` 仅用于重新记录你自己尚未合并的记录。
- 原始输出不进入 git。记录只携带计数和单行错误文本。
- 保持套件稳定。新场景在单独的变更中加入默认列表，使之后的记录可以同类相比，并在其第一条记录中注明它是新加入的。
