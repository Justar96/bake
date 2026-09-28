---
description: "面向长会话的进程健康诊断：在 Harness 主目录下记录事件循环延迟与堆占用，并提供致命错误取证，供排查内存增长或卡顿的用户与维护者阅读。"
kind: "package-reference"
---

# @deepseek-ai/dsh-runtime-watchdog

[English](README.md) | 中文

## 概述

当长会话变慢、堆占用逼近 V8 上限或因致命错误退出时，本包会留下证据。它通过一个 unref 的定时器，每十秒采样一次 Node 的事件循环延迟、V8 堆占用与常驻内存。当事件循环持续延迟，或堆占用达到上限的较高比例时，它会向 `$DSH_HOME/diagnostics` 下的文件追加一条限速的 JSON 记录。当 Node 以能把机密与网络数据排除在外的参数运行时，它还会把 Node 的致命错误报告，以及可选的接近上限时的堆快照，写入该目录。它不会向 stdout、stderr、会话日志或模型上下文写入任何内容。`dsh` 基础组合包为每个 profile 挂载本包。

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

基础组合包已经用默认值挂载看门狗，因此 Bake profile 无需任何设置。会话变得迟缓或内存耗尽之后，阅读这些记录；默认值记录过多或过少时，调整下面的阈值。

### 何时使用

在任何运行长时间 agent 会话的进程中保持挂载：终端、桌面桥接以及长时间的 headless 任务。它的开销是每十秒一次不到 20 µs 的采样，以及一个每秒唤醒 Node 20 次的事件循环探针。当部署不得在 Harness 主目录下写文件时，禁用该行。

### 配置

基础组合包挂载如下行：

```yaml
- id: runtime-watchdog
  name: '@deepseek-ai/dsh-runtime-watchdog'
  config:
    directory: !!js dshHomePath('diagnostics')
```

patch 会替换该行的整个 `config`，因此覆盖时要重新写出 `directory`：

```yaml
- id: runtime-watchdog
  config:
    directory: !!js dshHomePath('diagnostics')
    eventLoopDelayMs: 500
    heapSnapshots: 1
```

| 字段 | 默认值 | 含义 |
|---|---|---|
| `directory` | 必填 | 存放记录、报告与快照的绝对目录；以 `0700` 权限创建 |
| `intervalMs` | `10000` | 两次采样之间的毫秒数；每次采样结束一个事件循环延迟窗口 |
| `eventLoopDelayMs` | `250` | 窗口的第 99 百分位事件循环延迟达到该值时，该窗口计为延迟 |
| `sustainedMs` | `30000` | 连续的延迟窗口或一次连续卡顿需要持续多久才记录延迟 |
| `heapFraction` | `0.85` | 堆占用达到或超过 V8 堆大小上限的该比例时记录 |
| `recordIntervalMs` | `300000` | 同类两条记录之间的最短间隔；期间的越限次数计入下一条记录 |
| `fatalErrorReport` | `true` | 当 Node 以 `--report-exclude-env` 运行时，把 Node 的致命错误报告写入 `directory` |
| `heapSnapshots` | `0` | 当 Node 的 `--diagnostic-dir` 为 `directory` 时，V8 在接近上限时可写入 `directory` 的堆快照数量；`0` 表示禁用 |

相对路径的 `directory` 会在启动时失败，因为它会解析到工作区内。生成的[配置目录](../../../docs/config-catalog.zh.md#deepseek-aidsh-runtime-watchdog)记录了每个可接受的值。

### 你会得到什么

第一次越过阈值时，会在 `directory` 中以 `0600` 权限创建 `watchdog.<YYYYMMDD>.<HHMMSS>.<pid>.jsonl`；同一进程之后的记录追加到该文件。每行是一条记录，下面为便于阅读做了换行：

```json
{"time":"2026-09-28T12:00:00.000Z","kind":"event-loop-delay","pid":4242,"uptimeMs":3600000,
 "eventLoop":{"p50Ms":40,"p99Ms":320,"maxMs":1200,"meanMs":60,"samples":199,"windowMs":10000,"delayedForMs":30000},
 "memory":{"heapUsed":157286400,"heapTotal":209715200,"heapLimit":4395630592,"rss":402653184,"peakHeapUsed":167772160,"peakRss":419430400},
 "suppressed":0}
```

- `kind` 为 `event-loop-delay` 或 `heap-limit`。两类记录携带相同字段，因为堆压力与繁忙的事件循环常常互为解释。
- `eventLoop` 描述在 `time` 结束的采样窗口，单位为超出探针周期的延迟毫秒数。`delayedForMs` 是连续窗口保持延迟的时长；由单次卡顿触发记录时为 `0`。
- `memory` 的单位是字节；峰值是看门狗启动以来采样到的最高值。
- `suppressed` 统计自上一条记录以来，被限速跳过的同类越限次数。

每条记录还会在名为 `runtime-watchdog` 的 Cordis logger 上产生一条警告。已发布的 profile 不挂载控制台 logger，因此文件是持久副本。

只有在不会泄漏到工作区、也不会暴露凭据时，才会启用 Node 自身的诊断：

- **致命错误报告。** 启用 `fatalErrorReport` 且 Node 以 `--report-exclude-env` 启动时，内存耗尽等致命错误会把 Node 的 `report.<date>.<time>.<pid>.<thread>.<seq>.json` 写入 `directory`。报告包含 JavaScript 与原生调用栈、堆空间统计、资源用量和 libuv 句柄，不含环境变量。没有该参数时，插件不改动 `process.report`，并记录原因；`--report-exclude-network` 还会排除网络接口信息。
- **接近上限时的堆快照。** `heapSnapshots` 大于零且 Node 的 `--diagnostic-dir` 解析为 `directory` 时，V8 会在堆接近上限时写入最多该数量的 `Heap.<date>.<time>.<pid>.<thread>.<seq>.heapsnapshot` 文件。快照大小是堆的数倍，V8 写入时进程会暂停，而且快照包含内存中的每个字符串，包括凭据与文件内容。否则插件会记录未启用的原因。

请以命令行参数而不是 `NODE_OPTIONS` 传给 Node，因为 agent 的子进程会继承 `NODE_OPTIONS`：

```sh
node --report-exclude-env --report-exclude-network --diagnostic-dir="$DSH_HOME/diagnostics" apps/cli/lib/bin.js --profile tui
```

-----

<a id="understand-the-implementation"></a>
## 理解实现

<details>
<summary>实现细节——点击展开</summary>

本节解释看门狗如何测量、判定与记录，并指出实现它的代码位置；可观察行为已在[使用本包](#use-this-package)中说明。

### 设计说明

- **低开销采样。** 每次采样读取延迟直方图、`v8.getHeapStatistics()` 与 `process.memoryUsage.rss()`，在开发机上实测为 7 到 18 µs，取决于负载。间隔定时器是 unref 的，因此不会让已结束的进程继续存活。直方图每 50 ms 采样一次；在高负载机器上，空闲时每秒约耗 0.6 ms CPU，而 Node 默认的 10 ms 周期约为 3 ms，且仍能分辨远低于 250 ms 阈值的延迟。
- **超出探针周期的延迟。** 直方图的每个值都包含 50 ms 的采样周期，看门狗会将其扣除。当卡顿发生在 JavaScript 回调内部时，Node 会先运行逾期的看门狗采样，再由直方图自己的定时器记录这次卡顿，此时重置直方图会丢弃该样本。采样本身的迟到时长测量的是同一次卡顿，因此它会并入窗口最大值。
- **持续而非瞬时。** 当窗口的第 99 百分位延迟达到 `eventLoopDelayMs` 时，该窗口计为延迟。当连续的延迟窗口持续达到 `sustainedMs`，或单次卡顿本身持续这么久时，才会记录延迟。启动、大段粘贴或一次长时间渲染造成的孤立尖峰不会被记录。
- **接近上限的堆。** 每次采样都会把 `used_heap_size / heap_size_limit` 与 `heapFraction` 比较。85% 时距离内存耗尽崩溃仍有余量，且该阈值远高于健康用量：一次记录下的 2000 轮回放在数 GiB 的上限下只保留了约 82 MiB。
- **限速且持久的记录。** 每类记录每 `recordIntervalMs` 至多一次，这使一个记录文件每小时约不超过 24 行。记录以同步方式追加，因此崩溃前刚写入的记录已在磁盘上。每次采样都会捕获并记录所有错误，因为定时器中的未捕获异常会让 [fail-loud](../../boot/app-boot/README.zh.md) 退出进程。
- **不输出到终端。** 终端界面占有 stdout，stderr 与它共用屏幕，因此记录只写入文件与 Cordis logger。
- **致命错误报告需要命令行参数。** Node 只有在致命错误发生于 JavaScript 上下文内时，才会应用运行时的 `process.report.excludeEnv` 与 `excludeNetwork`。在其他位置触发的内存耗尽会回退到命令行设置；经由 Loader 启动的组合复现了这一点，尽管运行时设置已开启，报告仍写入了环境变量。因此只有当 `--report-exclude-env` 出现在 `process.execArgv` 或 `NODE_OPTIONS` 中时，插件才会启用报告；它设置四个报告字段，并在释放时恢复原值。
- **快照需要 Node 的诊断目录。** `v8.setHeapSnapshotNearHeapLimit()` 写入 `--diagnostic-dir`，未设置时写入工作目录，且没有运行时 API 能改变这一点。插件只在该目录解析为 `directory` 时才启用它，因此快照永远不会落入工作区。

### 源码地图

| 文件 | 职责 |
|---|---|
| [`src/index.ts`](src/index.ts) | 插件入口：`Config` schema、Node 进程绑定与 `apply` |
| [`src/watchdog.ts`](src/watchdog.ts) | 采样、阈值、限速、记录文件、取证启用与 Node 选项解析 |
| — | 不发布运行时不变式配套入口；看门狗读取进程级测量值，不暴露独立配套入口可检查的包自有事件或快照。 |

</details>

-----

<a id="further-exploration"></a>
## 进一步探索

当包级约定不够用时阅读以下页面。它们从挂载看门狗的组合逐步进入它所配置的 Node 设施。

- [dsh-base 组合包](../../bundle/base/README.zh.md)——每个 profile 继承的共享行。
- [App boot fail-loud](../../boot/app-boot/README.zh.md#use-this-package)——为什么抛出异常的定时器回调会结束进程。
- [生成配置目录](../../../docs/config-catalog.zh.md#deepseek-aidsh-runtime-watchdog)——每个受支持配置字段及其源声明。
- [Node.js 诊断报告](https://nodejs.org/api/report.html)——报告内容与 `--report-exclude-*` 参数。
- [runtime-diagnostics 组映射](../README.zh.md)——同组的不变式检查。

-----

<a id="model-experience"></a>
## 模型体验

无，因为看门狗只写诊断文件与 logger 警告，不注册任何提示词、工具或会话事件。

#### KV Cache 影响

无；采样与记录从不触及模型请求。

## 已知限制与延期工作

<a id="known-limitations-and-deferred-work"></a>

这些限制说明看门狗不捕获或不控制的内容。它们是当前包约束，不是任务积压。

- **取证依赖启动参数**——Bake 的启动器（`bake`、`bake.cmd` 和开发启动器）会传入 `--report-exclude-env --report-exclude-network --diagnostic-dir=$DSH_HOME/diagnostics`，因此会启用致命错误报告；堆快照仍需 `heapSnapshots` 大于 0。npm 的 `dsh` 命令和直接运行 `node apps/cli/lib/bin.js` 的入口会在需要时用这些参数重启 Node。自定义嵌入方必须自行提供这些参数。
- **已启用的堆快照会保持启用**——V8 无法取消 `setHeapSnapshotNearHeapLimit()`，因此释放或重新加载配置后，它仍保持启用直到进程退出。
- **不清理目录**——记录文件与报告会跨进程累积。在限速下每个记录文件都很小，每份报告为数十 KB，但每个快照都与堆大小相当。
- **低于阈值的增长不会被记录**——低于 `heapFraction` 的稳定堆增长不会留下记录，V8 堆之外的常驻内存也没有阈值，只出现在记录内部。
- **短暂卡顿不会被记录**——短于 `sustainedMs` 且不重复的单次卡顿不会进入记录。
- **只挂载一个实例**——`process.report` 是进程全局的，第二个实例会覆盖第一个实例的设置，并以错误的顺序恢复它们。

<a id="dev-note"></a>
### 开发备注

<details>
<summary>维护者的工作上下文——点击展开</summary>

本开发备注是维护者的工作上下文：开放问题与尚未决定的方向。它明确不具权威性——已交付的行为、限制与既定理由以上文和包代码为准。

#### 开放问题

目前没有记录。

</details>
