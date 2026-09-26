# Bake 直接下载发行版

[English](README.md) | 中文

Railway 项目 `28ff3000-7240-4c57-81d1-7bd05445c1ee` 中的 `bake-downloads` 服务在 `https://bake.justar.dev` 提供发行清单和安装脚本，并把每个版本化平台归档重定向到该版本 GitHub 发行版中的同名资源。服务已经创建，但只有在归档完成构建、检查和部署后，发行版才可下载。Bake 仍在 Node 上运行；Bun 负责构建工作区，并把生产依赖安装到每个平台归档中。

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

每个目标都要在对应宿主机上构建：`darwin-arm64`、`darwin-x64`、`linux-arm64`、`linux-x64` 和 `win32-x64`。[线上清单](https://bake.justar.dev/latest.json)列出当前可用的平台。已验证的平台可以单独发布。组装前，把要发布的归档收集到同一个 `.artifacts/bake-release/<Bake 版本>/` 目录。为现有版本增加目标时，也要放入所有已发布的归档，以免它们从服务中消失；先按现有清单下载这些归档并核验 SHA-256。单台宿主机的构建只能证明自己的目标；Windows 安装脚本和原生模块需要在 Windows 上运行检查。完整发行时，运行 `bun run release:assemble --complete`，再运行 `node distribution/host/verify-manifest.mjs --complete`。每次发行都必须使用新版本号：已安装的发行版以版本和归档哈希命名，而 `bake update` 只提供比正在运行的版本更新的版本。

## 使用 GitHub Actions 发行

[发行工作流](../.github/workflows/release.yml) 从一个标签出发，构建、检查、签名并发布所有平台：

```sh
git switch main && git pull --ff-only
NEXT=0.1.2                           # choose a stable version newer than the current one
bun run release:prepare "$NEXT"       # both manifests, bun.lock, and a CHANGELOG.md section
# write the new section in CHANGELOG.md, then:
bun run release:preflight --offline --tag "v$NEXT"
git add package.json apps/cli/package.json bun.lock CHANGELOG.md
git commit -m "release: $NEXT"
git tag "v$NEXT"
git push --atomic origin HEAD "v$NEXT"
```

请在默认分支通过常规检查后运行这些命令。`release:prepare` 把更新日志中 `[Unreleased]` 下的条目移到新版本下；若没有条目，则留下发行会拒绝的占位符。它接受 `0.1.1` 这类数字格式规范的稳定版本。原子推送会让发行提交和标签同时可见。推送标签后工作流开始运行：

1. **preflight**（`release:preflight`）检查标签是否指向默认分支上的提交、是否与工作区版本一致，以及更新日志章节是否已写好。它将版本与带签名的下载清单比较；关闭下载服务时则与 GitHub 最新已发布版本比较。已部署且未签名的 `0.1.0` 清单是首次过渡时唯一允许的例外。
2. **build** 为每个目标使用一台运行器——`ubuntu-24.04`、`ubuntu-24.04-arm`、`macos-15`、`macos-15-intel` 和 `windows-latest`——因为原生模块按宿主编译。每台运行器先确认自己构建的是对应目标，然后打包归档，用真实安装脚本安装，并通过 `bake update` 更新（`release:verify-local`，以临时密钥签名）；非 Windows 平台还会运行已构建配置的终端场景。即使某个平台失败，其余平台也会继续报告结果，而发布需要全部五个平台通过。
3. **publish** 在 `release` 环境中运行，签名密钥和部署令牌只存在于此。它对清单签名（`release:assemble --complete`），运行镜像的签名检查，并确认重试不会更改此版本已对外提供的归档。然后它把归档、清单、签名和安装脚本上传到 GitHub 发行版草稿，再下载并逐字节比较。由于下载服务把归档重定向到 GitHub 发行版，它先发布草稿并按客户端的方式验证。随后它部署不含归档的下载服务（全部归档超出 Railway 的上传上限），并等待清单和每个归档都能经由该服务获取。重新运行时会保留已发布 GitHub 发行版的资源，只证明它们与本次构建一致。

在 Actions 页面手动运行工作流即为试运行：它会构建并检查全部五个平台，但不发布任何内容，即使从标签启动也一样。若标签运行在发布前失败，可重新运行；同一次运行已部署的相同版本可以继续，但归档字节不同会被拒绝。若 GitHub 发行版公开后才失败，请运行 `bun run release:verify-live <版本> --complete` 检查主机，并查看失败步骤，再开始新版本。

首次打标签发行前，请在仓库设置中创建 `release` 环境，限制其只用于匹配 `v*` 的标签；如果发行需要人工确认，再添加必需的审核者。设置签名密钥；若要部署下载服务，还需要 Railway 令牌：

| 密钥 | 值 |
|---|---|
| `BAKE_RELEASE_SIGNING_KEY` | 发行签名密钥的完整 PEM 文本 |
| `RAILWAY_TOKEN` | 下载服务所在项目 `production` 环境的 Railway 项目令牌；设置 `BAKE_DOWNLOAD_SERVICE=off` 时不需要 |

GitHub 发行版本身也是一个发行主机：它在归档旁附带已签名的清单，安装脚本和 `bake update` 接受 `BAKE_RELEASE_BASE_URL=https://github.com/Justar96/bake/releases/latest/download`，并从各自的标签获取每个归档。仓库公开后，这些资源才能免登录下载；工作流也会按客户端的方式验证已发布的 GitHub 发行版。将仓库变量 `BAKE_DOWNLOAD_SERVICE` 设为 `off` 即只发布到 GitHub；此模式要求公开仓库，并在构建前检查 GitHub 最新发行版。此时客户端需要使用该主机，可以通过上述变量，也可以把它设为 `packages/boot/updater/src/keys.ts` 和两个安装脚本中的默认值。

## 手动发布已检查的发行版

清单中列出的每个归档通过对应宿主机的本机检查并已发布到该版本的 GitHub 发行版后，运行 `node distribution/host/verify-manifest.mjs`，再在仓库根目录部署下载服务：

```sh
host="$(mktemp -d)/bake-downloads"
cp -R distribution/host "$host" && rm -rf "$host/public/releases"
railway up "$host" --path-as-root --no-gitignore \
  --project 28ff3000-7240-4c57-81d1-7bd05445c1ee \
  --environment production --service bake-downloads \
  --detach --json -m "Bake CLI direct download release"
```

`--no-gitignore` 会包含生成的清单和签名，复制时去掉的归档由服务重定向到 `https://github.com/Justar96/bake/releases/download/v<版本>/`。Docker 构建（`verify-manifest.mjs --allow-missing-archives`）会拒绝未签名或并非由已提交密钥签名的清单、空清单、不支持的目标、无效文件名、空归档，以及镜像中实际包含的任何归档的大小或 SHA-256 不匹配。使用 `--complete` 可额外要求全部五个平台。记录部署 ID，等待该部署达到 `SUCCESS`，然后通过公开域名检查 `/health`、`/latest.json`、`/latest.json.sig`、两个安装脚本路径和本次新增的每个平台归档。上传完成本身不能证明发行成功。清单列出当前可用的平台；安装脚本在不支持的平台上会明确报错。

安装了 Node 24 或更新版本的用户可在已发布清单列出的平台上安装。Unix 命令需要先发布对应的 macOS 或 Linux 归档：

```sh
curl -fsSL https://bake.justar.dev/install.sh | sh
```

```powershell
irm https://bake.justar.dev/install.ps1 | iex
```

安装脚本验证清单签名，按宿主平台选择归档，对照清单验证 SHA-256，并在用户主目录下安装 `bake`。它们不会读取或迁移 `~/.dsh`；除非设置 `DSH_HOME`，命令使用 Bake 的 `~/.bake` 目录。Unix 需要 `curl`、`tar`，以及 `shasum` 或 `sha256sum`；Windows 需要 `tar.exe`。下载服务只提供发行文件，不保存 API 密钥。

交互式安装和 `bake update` 会在操作期间显示琥珀色 ASCII 烤箱，成功后留下刚出炉的面包。重定向输出、CI 和 `TERM=dumb` 使用纯文本。设置 `BAKE_NO_ANIMATION=1` 可关闭动画，设置 `NO_COLOR=1` 可关闭颜色。安装脚本内嵌 CLI 渲染器；修改 `apps/cli/src/bakery.ts` 或 `scripts/release/installer-animation.ts` 后，运行 `bun scripts/release/embed-animation.ts`，再用 `bun scripts/release/embed-animation.ts --check` 验证。

<a id="updating-an-install"></a>

## 更新安装

`bake update` 用最新的已签名发行版替换由安装脚本创建的安装；`bake update --check` 只报告结果：已是最新时退出码为 0，有更新版本时为 10，失败时为 1。更新会下载到安装根目录，检查归档的大小、SHA-256，以及其命令能否以声明的版本启动，并在最后一步才移动 `current`，因此在此之前的任何失败都会保持安装原样；参见[更新程序](../packages/boot/updater/README.zh.md)。已经打开的会话继续运行各自的版本，新会话启动新版本。源码检出会被拒绝，并给出更新它的命令。

在 Unix 上，`<安装根目录>/current` 是 `~/.local/bin/bake` 所跟随的链接。在 Windows 上，`bake.cmd` 每次运行都从 `<安装根目录>\current.txt` 读取发行版名称，因此更新从不改写正在被 `cmd.exe` 读取的批处理文件；直接指定某个版本的旧版 `bake.cmd` 会在运行它的 `cmd.exe` 退出后被替换。

终端会在状态栏中提示更新的发行版，该结果缓存在 `<Bake 主目录>/update-check.json` 中一天，并在后台刷新，不会延迟首帧。它从不自动安装任何内容。设置 `BAKE_NO_UPDATE_CHECK=1` 可关闭检查。更新程序出现之前的发行版没有 `bake update`；再次运行安装脚本即可获得它。
