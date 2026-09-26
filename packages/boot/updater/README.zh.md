---
description: "Bake 的自更新程序：校验已签名的发行清单，在正在运行的版本旁安装新版本，并以每小时缓存回答更新提示。"
kind: "package-library"
---

# @deepseek-ai/dsh-updater

[English](README.md) | 中文

## 摘要

`@deepseek-ai/dsh-updater` 是 `bake update`、终端 `/update` 及其更新提示背后的代码。只有已知的 Ed25519 密钥签署了发行清单的原始字节，它才信任该清单；它把新版本安装到正在运行版本旁边的独立版本目录，并在最后一步才移动安装的 `current` 指针，因此中断的更新会保留旧版本。它只更新由安装脚本布局的安装；源码检出保持不变。请将其作为直接库依赖使用，而不是通过 `cordis.yml`。

## 目录

- [使用本包](#use-this-package)
- [了解实现](#understand-the-implementation)
- [已知限制与延后工作](#known-limitations-and-deferred-work)
- [开发说明](#dev-note)

-----

<a id="use-this-package"></a>
## 使用本包

`detectInstall(release)` 判断本进程运行所在的发行目录是否受管理：它直接位于 `<root>/versions/` 下，名称为 `<版本>-<归档 SHA-256 的前 12 位十六进制>`。`fetchRelease(source)` 获取 `latest.json` 和 `latest.json.sig`，只有受信任的密钥验证签名通过时才返回清单；`releaseSource(env)` 给出当前环境的发行主机和密钥。`statusOf(manifest, running, target)` 将结果与本主机 `hostTarget()` 上正在运行的版本比较，报告 `newer`、`current`，或在发行版未提供本平台归档时报告 `unavailable`。`installRelease(options)` 安装 `newer` 结果，并通过 `onProgress` 报告每一步：下载已接收与总字节数，然后是 `unpack` 和 `verify`。`selfUpdate(options)` 是两个命令共用的完整流程：它在询问主机之前拒绝不受管理的安装（`check` 运行仍可询问），然后检查、记录结果并安装，返回由各表层自行措辞的 `UpdateOutcome`。`currentVersion(root)` 给出下次启动将运行的版本，使提示能区分已安装的更新与可用的更新。

`cachedUpdate(home, running)` 同步读取 `<Bake 主目录>/update-check.json` 中的上次结果，供首帧显示提示。`refreshCheck(options)` 只在该结果已满一小时（`CHECK_INTERVAL_MS`）时再次询问主机；失败的检查也会记录，并在十分钟后重试（`FAILED_CHECK_RETRY_MS`）。较短的重试在发版刚结束时最重要：发布期间，新的 `latest.json` 可能与旧签名并存，此时检查会失败。`recordCheck(home, status)` 写入其他命令得到的结果，使 `bake update` 与提示保持一致。只有已验证且包含本平台归档的清单才会记录版本。当 `BAKE_NO_UPDATE_CHECK` 设为空、`0` 或 `false` 以外的任何值时，`checksDisabled(env)` 为真。

| 变量 | 作用 |
|---|---|
| `BAKE_RELEASE_BASE_URL` | 发行主机；默认 `https://bake.justar.dev`，与安装脚本相同。`https://github.com/<owner>/<repo>/releases/latest/download` 表示某仓库的 GitHub 发行版，此时 `archiveUrl` 从各自的标签获取每个归档 |
| `BAKE_RELEASE_PUBLIC_KEY` | 额外信任的一个密钥（base64 DER SPKI），用于本地或 CI 检查中以临时密钥签署的发行版 |
| `BAKE_NO_UPDATE_CHECK` | 关闭后台检查；`bake update` 仍可使用 |

<a id="understand-the-implementation"></a>
## 了解实现

一次安装分五步，只有最后一步会改变 `bake` 启动的内容：

1. 获取 `<root>/update.lock`，其中记录持有者的进程 ID 和开始时间。若持有进程已退出，或锁已超过 30 分钟，竞争者会接管，因此被终止的更新不会阻塞下一次。
2. 持锁期间清空 `<root>/.staging/`（中断的运行可能在此留下文件），并把归档流式写入其中。归档超出声明大小时立即停止读取；大小或 SHA-256 与清单不符时拒绝。
3. 使用系统 `tar` 解包，并运行解包后命令的 `--version`，其输出必须是清单中的版本。
4. 在同一文件系统内把解包目录重命名到 `versions/`。已存在且能启动的目录会直接复用而不再下载；不能启动的目录会被移开并替换。
5. 移动 `current`。Unix 把新的绝对链接重命名覆盖 `<root>/current`；Windows 把新文件重命名覆盖 `<root>/current.txt`。读取方看到的要么是旧目标，要么是新目标，不会两者皆无。

正在运行的进程在启动时已解析出自己的版本目录，因此会继续从该目录运行。下次启动时才会运行新版本。

每次启动都会更新其版本目录中的 `.last-launch`（`markLaunched`）。安装之后，只有当版本目录不是新的当前版本、不是被替换的版本、不是更新程序运行所在的版本，并且一周内无人启动时，才会被删除。这样可保留仍被其他终端使用的版本，否则其后续的延迟导入会失败。

在 Windows 上，`cmd.exe` 运行批处理文件时按偏移逐行读取，因此改写正在运行的 `bake.cmd` 会让它从旧文件的偏移处继续执行新文件。所以 `windowsLauncher(root)` 写出的启动器每次运行都读取 `current.txt`，从不需要改写；发行归档以 `bin/bake-launcher.cmd.template` 提供同样的文本供 `install.ps1` 使用。直接指定某个版本的旧版 `bake.cmd`，会在运行它的 `cmd.exe` 退出后由一个分离的辅助进程替换。

发行签名密钥的私钥部分从不进入仓库。`release:assemble` 从 `BAKE_RELEASE_SIGNING_KEY_FILE` 读取私钥，并对清单写出的字节签名。`RELEASE_PUBLIC_KEYS` 可以包含多个密钥，因此新密钥可在旧密钥停止签名前先获得信任；`distribution/host/release-key.pub` 和两个安装脚本携带第一个密钥，并由一项发行测试保证所有副本一致。

<a id="known-limitations-and-deferred-work"></a>
## 已知限制与延后工作

- 提示只告知有可用更新；不运行 `bake update` 或 `/update` 就不会安装。
- 没有发行渠道：所有安装都跟随 `latest.json`。
- 除启动标记外，清理无法得知其他进程是否仍在使用某个版本；在 Unix 上，一个从两次更新之前的版本启动、且保持打开超过一周的会话可能会丢失其文件。

<a id="dev-note"></a>
## 开发说明

```sh
bun run test:runtime packages/boot/updater
```

测试会打包真实的 `.tar.gz` 发行版，从内存中提供，并以临时密钥签名。`bun run release:verify-local` 覆盖完整路径：它用真实安装脚本安装暂存的发行版，基于它发布一个更新的版本，然后依次运行 `bake update --check`、针对不受信任密钥而被拒绝的更新、`bake update` 和 `bake --version`。
