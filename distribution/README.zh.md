# Bake 直接下载发行版

[English](README.md) | 中文

Railway 项目 `28ff3000-7240-4c57-81d1-7bd05445c1ee` 中的 `bake-downloads` 服务在 `https://bake.justar.dev` 提供发行清单、安装脚本和版本化平台归档。服务已经创建，但只有在归档完成构建、检查和部署后，发行版才可下载。Bake 仍在 Node 上运行；Bun 负责构建工作区，并把生产依赖安装到每个平台归档中。

## 在本机构建并检查一个平台

使用装有稳定版 Bun 1.4.2（固定在 `package.json` 中）、Node 24 或更新版本及本机原生构建依赖的干净检出。先运行 `bun install --frozen-lockfile` 和 `bun run build`，再运行：

```sh
bun run release:pack
BAKE_RELEASE_SIGNING_KEY_FILE=~/.config/bake/release-signing-key.pem bun run release:assemble
bun run release:verify-local
```

`release:pack` 创建 `.artifacts/bake-release/<Bake 版本>/bake-v<Bake 版本>-<平台>.tar.gz`。它要求根目录与 CLI 的版本一致，复制包声明的载荷根目录、许可证、更新日志和已构建文件，然后在临时工作区执行冻结的 Bun 生产依赖安装，并启动暂存的 CLI。`release:assemble` 为当前 Bake 版本的每个归档计算哈希，写入 `distribution/host/public/latest.json` 和版本化归档，并对清单的原始字节签名，写入 `latest.json.sig`。本机检查先验证该签名，再通过 HTTP 提供这些文件，把对应平台的安装脚本安装到临时目录，并启动安装后的命令。随后它基于已安装的文件发布一个高一个补丁号、以独立密钥签名的发行版，并用 `bake update` 更新到该版本：依次检查 `bake update --check`、在不信任新清单密钥时被拒绝的更新、更新本身、`bake --version`、`current` 的指向，以及被替换的版本仍保留在磁盘上。这些生成目录已被 Git 忽略。

### 发行签名密钥

只有 `distribution/host/release-key.pub` 中的 Ed25519 密钥签署的清单，才会被安装脚本和 `bake update` 信任；同一公钥也写在两个安装脚本和 `packages/boot/updater/src/keys.ts` 的 `RELEASE_PUBLIC_KEYS` 中，并由一项发行测试保证所有副本一致。私钥部分从不进入仓库：请将其保存为只有发行负责人可读的 PKCS#8 PEM 文件，并通过 `BAKE_RELEASE_SIGNING_KEY_FILE` 指定。缺少该文件时 `release:assemble` 会拒绝运行；公钥部分与 `release-key.pub` 不一致的密钥也会被拒绝。没有密钥的检查（例如 CI）运行 `bun run release:assemble --ephemeral-key`：它以临时密钥签名，并把公钥作为 `ephemeral-release-key.pub` 放在归档旁边，`release:verify-local` 会通过 `BAKE_RELEASE_PUBLIC_KEY` 把它交给客户端。下载服务的镜像构建不设置该变量，因此以临时密钥签名的清单不会被部署。轮换密钥时，先在由旧密钥签名的发行版中把新公钥加入 `RELEASE_PUBLIC_KEYS`，再改用新密钥签名。

每个目标都要在对应宿主机上构建：`darwin-arm64`、`darwin-x64`、`linux-arm64`、`linux-x64` 和 `win32-x64`。已验证的平台可以单独发布；当前 `0.1.0` 载荷仅包含 Windows x64，下载页明确说明 macOS 和 Linux 尚不可用。组装前，把要发布的归档收集到同一个 `.artifacts/bake-release/<Bake 版本>/` 目录。为现有版本增加目标时，也要放入所有已发布的归档，以免它们从服务中消失；先按现有清单下载这些归档并核验 SHA-256。单台宿主机的构建只能证明自己的目标；Windows 安装脚本和原生模块需要在 Windows 上运行检查。完整发行时，运行 `bun run release:assemble --complete`，再运行 `node distribution/host/verify-manifest.mjs --complete`。每次发行都必须使用新版本号：已安装的发行版以版本和归档哈希命名，而 `bake update` 只提供比正在运行的版本更新的版本。

## 使用 GitHub Actions 发行

[发行工作流](../.github/workflows/release.yml) 从一个标签出发，构建、检查、签名并发布所有平台：

```sh
bun run release:prepare 0.1.1        # 两个清单、bun.lock 以及 CHANGELOG.md 中的一个章节
# 在 CHANGELOG.md 中写好 0.1.1 章节，然后：
git commit -am "release: 0.1.1"
git tag v0.1.1
git push origin HEAD v0.1.1
```

`release:prepare` 把更新日志中 `[Unreleased]` 下的条目移到新版本下；若没有条目，则留下发行会拒绝的占位符。推送标签后工作流开始运行：

1. **preflight**（`release:preflight`）在任何构建开始前检查：标签必须与工作区版本一致；更新日志章节不得缺失、为空或仍是占位符；版本必须比下载服务当前提供的版本更新。任一条件不满足即失败。
2. **build** 为每个目标使用一台运行器——`ubuntu-24.04`、`ubuntu-24.04-arm`、`macos-15`、`macos-15-intel` 和 `windows-latest`——因为原生模块按宿主编译。每台运行器先确认自己构建的是对应目标，然后打包归档，用真实安装脚本安装，并通过 `bake update` 更新（`release:verify-local`，以临时密钥签名）；非 Windows 平台还会运行已构建配置的终端场景。即使某个平台失败，其余平台也会继续报告结果，而发布需要全部五个平台通过。
3. **publish** 在 `release` 环境中运行，签名密钥和部署令牌只存在于此。它对清单签名（`release:assemble --complete`），运行镜像自身的签名检查，创建包含归档、`latest.json`、`latest.json.sig` 和两个安装脚本的 GitHub 发行版草稿，部署下载服务，等待公开主机提供新版本并按客户端的方式验证（`release:verify-live`），之后才发布草稿。任一步骤失败都会留下草稿，重新运行该任务即可完成。

在 Actions 页面手动运行工作流即为试运行：它会构建并检查全部五个平台，但不发布任何内容。

首次打标签发行前，请在仓库设置中创建 `release` 环境，限制其只用于匹配 `v*` 的标签；如果发行需要人工确认，再添加必需的审核者。为它设置两个密钥：

| 密钥 | 值 |
|---|---|
| `BAKE_RELEASE_SIGNING_KEY` | 发行签名密钥的完整 PEM 文本 |
| `RAILWAY_TOKEN` | 下载服务所在项目 `production` 环境的 Railway 项目令牌 |

GitHub 发行版本身也是一个发行主机：它在归档旁附带已签名的清单，安装脚本和 `bake update` 接受 `BAKE_RELEASE_BASE_URL=https://github.com/Justar96/bake/releases/latest/download`，并从各自的标签获取每个归档。仓库公开后，这些资源才能免登录下载；此后工作流还会按客户端的方式验证已发布的 GitHub 发行版。将仓库变量 `BAKE_DOWNLOAD_SERVICE` 设为 `off` 即只发布到 GitHub；此时客户端需要使用该主机，可以通过上述变量，也可以把它设为 `packages/boot/updater/src/keys.ts` 和两个安装脚本中的默认值。

## 手动发布已检查的发行版

清单中列出的每个归档通过对应宿主机的本机检查后，运行 `node distribution/host/verify-manifest.mjs`，再在仓库根目录部署下载服务：

```sh
railway up ./distribution/host --path-as-root --no-gitignore \
  --project 28ff3000-7240-4c57-81d1-7bd05445c1ee \
  --environment production --service bake-downloads \
  --detach --json -m "Bake CLI direct download release"
```

`--no-gitignore` 会包含生成的 `public/` 载荷。Docker 构建会拒绝未签名或并非由已提交密钥签名的清单、空清单、不支持的目标、无效文件名、空归档，以及任何已包含归档的大小或 SHA-256 不匹配。使用 `--complete` 可额外要求全部五个平台。记录部署 ID，等待该部署达到 `SUCCESS`，然后通过公开域名检查 `/health`、`/latest.json`、`/latest.json.sig`、两个安装脚本路径和本次新增的每个平台归档。上传完成本身不能证明发行成功。清单列出当前可用的平台；安装脚本在不支持的平台上会明确报错。

安装了 Node 24 或更新版本的用户可在已发布清单列出的平台上安装。目前 Windows x64 可用；Unix 命令需要先发布对应的 macOS 或 Linux 归档：

```sh
curl -fsSL https://bake.justar.dev/install.sh | sh
```

```powershell
irm https://bake.justar.dev/install.ps1 | iex
```

安装脚本验证清单签名，按宿主平台选择归档，对照清单验证 SHA-256，并在用户主目录下安装 `bake`。它们不会读取或迁移 `~/.dsh`；除非设置 `DSH_HOME`，命令使用 Bake 的 `~/.bake` 目录。Unix 需要 `curl`、`tar`，以及 `shasum` 或 `sha256sum`；Windows 需要 `tar.exe`。下载服务只提供发行文件，不保存 API 密钥。

交互式安装和 `bake update` 会在操作期间显示琥珀色 ASCII 烤箱，成功后留下刚出炉的面包。重定向输出、CI 和 `TERM=dumb` 使用纯文本。设置 `BAKE_NO_ANIMATION=1` 可关闭动画，设置 `NO_COLOR=1` 可关闭颜色。安装脚本内嵌 CLI 渲染器；修改 `apps/cli/src/bakery.ts` 或 `scripts/release/installer-animation.ts` 后，运行 `bun scripts/release/embed-animation.ts`，再用 `bun scripts/release/embed-animation.ts --check` 验证。

## 更新安装

`bake update` 用最新的已签名发行版替换由安装脚本创建的安装；`bake update --check` 只报告结果：已是最新时退出码为 0，有更新版本时为 10，失败时为 1。更新会下载到安装根目录，检查归档的大小、SHA-256，以及其命令能否以声明的版本启动，并在最后一步才移动 `current`，因此在此之前的任何失败都会保持安装原样；参见[更新程序](../packages/boot/updater/README.zh.md)。已经打开的会话继续运行各自的版本，新会话启动新版本。源码检出会被拒绝，并给出更新它的命令。

在 Unix 上，`<安装根目录>/current` 是 `~/.local/bin/bake` 所跟随的链接。在 Windows 上，`bake.cmd` 每次运行都从 `<安装根目录>\current.txt` 读取发行版名称，因此更新从不改写正在被 `cmd.exe` 读取的批处理文件；直接指定某个版本的旧版 `bake.cmd` 会在运行它的 `cmd.exe` 退出后被替换。

终端会在状态栏中提示更新的发行版，该结果缓存在 `<Bake 主目录>/update-check.json` 中一天，并在后台刷新，不会延迟首帧。它从不自动安装任何内容。设置 `BAKE_NO_UPDATE_CHECK=1` 可关闭检查。更新程序出现之前的发行版没有 `bake update`；再次运行安装脚本即可获得它。
