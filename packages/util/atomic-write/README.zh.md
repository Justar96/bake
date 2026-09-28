---
description: "原子文件替换与跨进程写锁，供绝不允许在磁盘上留下不完整、被符号链接劫持或权限过宽内容的包使用。"
kind: "package-library"
---

# @deepseek-ai/dsh-atomic-write

[English](README.md) | 中文

## 概述

使用 `dsh-atomic-write` 替换文件时，不会暴露部分内容，也不会跟随临时路径上的符号链接。它的写锁会跨进程串行化读-修改-写入循环，因此并发写入方不会用陈旧状态相互覆盖。每次替换都会在全新 inode 上使用调用方选择的权限位，从而安全地收窄现有文件的权限。持锁期间死亡的写入方不会阻塞后续写入方：在 Linux 和 macOS 上，锁以内核 `flock` 持有，下一个写入方无需操作者介入即可将其恢复。这个库只接受字符串；它不提供 `cordis.yml` 插件，也不保证崩溃持久性，因为它不调用 `fsync`。

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

当文件型存储必须替换一份已渲染好的字符串、且绝不允许暴露部分写入、符号链接劫持或权限过宽状态时，使用 `writeFileAtomic`；当多个进程对同一文件执行读-修改-写入循环时，使用 `withFileLock`。最小路径是一次调用，传入最终内容与替换 inode 的权限位。

### 原子写入文件

```ts
import { writeFileAtomic } from '@deepseek-ai/dsh-atomic-write'

declare const text: string
await writeFileAtomic('/home/u/.dsh/settings.yaml', text, { mode: 0o600 })
```

父目录会按需创建，读取方只会观察到旧内容或完整的新内容。在 Windows 上，报告为 `EACCES`、`EBUSY` 或 `EPERM` 的瞬时替换干扰会在有界时间内重试；任何剩余失败都会移除临时文件，并保持目标文件不变。

### 协调写入方

对于单靠原子提交无法保证安全的读-渲染-提交循环，请在操作期间持有写锁：

```text
import { withFileLock, writeFileAtomic } from '@deepseek-ai/dsh-atomic-write'

declare const render: (previous: string) => string
declare const readCurrent: () => Promise<string>

await withFileLock('/home/u/.dsh/settings.yaml', async () => {
  const previous = await readCurrent()
  await writeFileAtomic('/home/u/.dsh/settings.yaml', render(previous), { mode: 0o600 })
})
```

只有写入方会竞争——读取方从不取锁——竞争者按指数退避，超时后报错，而不是无限阻塞。竞争者等待多久由每次调用经 `waitMs` 声明：默认值只按纯文件工作量级选定，因此持锁方循环若包含一次网络往返——例如刷新过期 token 的凭据变更——就应声明更长的值，否则该文件的其他写入方在这段时间内都会失败。退避节奏保持固定。存活的持锁方无论运行多久都不会被取代：竞争者只在证明持有者已不存在时才移除已有锁，从不依据锁的存续时间。

### 需要规划的失败

Windows 在无法观察到锁时会对 `EPERM` 重试一次，因为持锁方可能在独占创建与存在性检查之间释放锁。再次出现无法确认锁存在的 `EPERM` 时，会重新抛出错误且不运行操作。

锁的父目录必须已经存在，因此 `withFileLock` 会在运行操作之前拒绝无效的父目录层级。在 Linux 和 macOS 上，持锁进程退出或被杀死时会把锁文件留在原地，下一个写入方会移除它并立即继续。无法证明持有者已不存在的锁文件保持原样，写入方照旧超时失败。这包括在另一台主机上写入的锁文件、其进程仍然存在的 PID 记录，以及未指明持有者的内容，例如写入方在创建锁与写入 PID 之间死亡时留下的空文件。Windows、缺失原生绑定的环境，以及不支持 `flock` 的文件系统沿用旧行为，从不恢复锁；在这些环境中，操作者需在确认没有写入方仍持有该锁后移除遗留的锁文件。

-----

<a id="understand-the-implementation"></a>
## 理解实现

<details>
<summary>实现细节——点击展开</summary>

本包遵循一项职责分离：原子提交负责交换，写锁负责跨进程排序。

### 源码地图

| 文件 | 职责 |
|---|---|
| [`src/index.ts`](src/index.ts) | `writeFileAtomic` 与 `withFileLock`，即本包的全部接口 |
| [`tests/fixtures/lock-holder.ts`](tests/fixtures/lock-holder.ts) | 以任一协议持锁的子进程，供跨进程测试使用 |
| — | 不发布运行时不变式伴生入口；这个纯文件系统原语不维护事件流或可变运行时数据；其替换约定由单元测试覆盖。 |

### 写入路径

`writeFileAtomic` 先以独占创建（`wx`）打开一个随机后缀的同级文件并写入内容，然后 rename 到目标上。独占打开拒绝跟随预先埋在可猜测临时路径上的符号链接；同目录兄弟文件保证 rename 落在同一文件系统上；rename 替换的是目标位置的符号链接本身，绝不写穿到该链接指向的文件。Windows 重试会保留同一份完整的兄弟文件，并采用有界指数退避，因此协作式写锁之外的软件瞬时占用目标时，不会让安全替换立即失败；已归档的[重试决策记录](../../../.agents/notes/archived/bug-fix/2026-08-29-windows-atomic-replace-retry.md)记录了最初的理由与被拒绝的替代方案。

`withFileLock` 通过以 `wx` 创建的 `<filename>.lock` 同级文件串行化写入方。`EEXIST` 直接表示竞争；只有一次新的 `lstat` 确认锁路径存在时，`EPERM` 才表示竞争，从而兼容 Windows 的独占创建行为，又不掩盖无关的权限故障。竞争按指数退避，在每次调用声明的 `waitMs` 期限（默认两秒）过后失败，任何配置下都报告相同的错误。

持锁方先把自己的 `<pid>\n` 记录写入所创建的文件，与旧版本相同。在 [`node-addon-system`](../../../native/system/README.zh.md) 的 `flock` 绑定可以加载的平台（Linux 和 macOS）上，持锁方随后对其描述符加非阻塞 `flock`，并确认锁路径仍指向自己的文件。接着它就地以记录自身 PID 与主机名的 JSON 记录覆盖原记录，并保持描述符打开直到释放。持锁方只在持有 `flock` 时写入 JSON 记录，而竞争者也只在持有 `flock` 时读取记录，因此竞争者永远不会读到未加锁或未写完的 JSON 记录。发现锁的竞争者以不跟随符号链接的方式打开它，并尝试获取其 `flock`。竞争表示持锁方存活，竞争者随即退避。否则竞争者先确认该路径仍指向自己加锁的文件，再读取记录。同一主机的 JSON 记录证明其持有者已不存在，因为进程死亡时内核会释放它的 `flock`。`<pid>\n` 记录只有在向该 PID 发送信号报告不存在此进程时，才证明其持有者已不存在。持有者被证明已不存在时，竞争者在仍持有其 `flock` 的状态下删除该锁，并立即重试。持有 `flock` 保证两个竞争者不会同时移除同一个锁，也都不会移除替换了其所锁文件的新文件。若持锁方的文件在其取得 `flock` 之前被移除，它会重新获取。持锁方释放时，先在路径仍指向自己文件的前提下删除锁，再关闭描述符。该协议除锁文件本身外不创建任何文件，因此目录监视器与以前一样只会看到锁文件这一个同级文件。

Windows、绑定无法加载的环境，以及拒绝 `flock` 的文件系统保留 `<pid>\n` 记录：持锁方关闭该文件并在 `finally` 中移除它，竞争者从不移除其他写入方的锁。两种形式在持有期间都会占据锁路径，因此相互排斥，不同版本的写入方可以同时运行。

### 交换为何安全

- **全新 inode，调用方声明的权限位**——临时文件带着 `mode` 走完 rename，因此收窄权限过宽的文件没有 chmod 竞态。`mode` 为必填，让权限决策始终可见于每个调用点。
- **读取方从不竞争**——rename 提交是原子的，读取方无需加锁。
- **竞争者只删除已证明死亡的锁**——内核 `flock` 已释放或 PID 已不存在，才证明持有者已停止；锁的存续时间从不计入，因为它无法区分已崩溃的所有者与暂停但仍存活的写入方。

</details>

-----

<a id="further-exploration"></a>
## 进一步探索

当你需要了解使用本原语的存储或它所属的工具家族时，阅读以下页面。

- [用户设置文件存储](../../settings/settings-file/README.zh.md)——每次写入都通过本包替换的设置文档。
- [凭据存储](../../credentials/credentials-local/README.zh.md)——本包加锁并替换的凭据文件。
- [原生 `flock` 行为](../../../native/system/docs/flock-contract.md)——写锁崩溃恢复所依赖的内核锁语义。
- [util 组映射](../README.zh.md)——本包所属的工具家族。

-----

<a id="model-experience"></a>
## 模型体验

无：本包是纯文件系统写入原语，不注册任何面向模型的内容。

#### KV Cache 影响

此处没有任何内容进入请求前缀，因此提供方缓存复用不受影响。

## 已知限制与延期工作

<a id="known-limitations-and-deferred-work"></a>


这些限制说明本包何时不是合适的工具。它们是当前包约束，不是任务积压。

- **原子但不保证持久**——不对文件或其所在目录做 `fsync`，因此崩溃后可能观察到 rename 被回退。此处的文件型存储在启动时重新读取并重新发布，把持久性留作调用方的策略。
- **仅支持字符串内容**——在有消费方需要之前，不提供 `Buffer` 或流式形态。
- **自动恢复锁需要原生 `flock`**——在 Windows 上、缺失原生绑定时，或在不支持 `flock` 的文件系统上，遗留的锁文件会一直阻塞写入方，直到操作者将其移除。写入方在创建锁与写入 PID 之间死亡时留下的空锁文件同样如此。
- **恢复假定锁目录只属于一台主机**——旧协议的 PID 记录不含主机信息，竞争者依据本机进程表判断它。若另一台主机或另一个 PID 命名空间中存活的旧版本写入方共享该目录，它可能失去自己的锁。来自另一台主机的内核持锁记录从不被恢复，本机主机名变更后亦然。
- **锁属于进程，而非其子进程**——锁描述符在 exec 时关闭，因此持锁方启动的子进程（例如插件操作运行的包管理器）在持锁方死亡后会在无锁状态下继续运行。

<a id="dev-note"></a>
### 开发备注

<details>
<summary>维护者的工作上下文——点击展开</summary>

一种对文件及其父目录执行 `fsync`、并在 Windows 上保留仅属主权限的持久性替换方案仍未实现（在源码中记录为 `settings-atomic-durability`）。

</details>
