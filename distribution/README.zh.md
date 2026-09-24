# Bake 直接下载发行版

[English](README.md) | 中文

Railway 项目 `28ff3000-7240-4c57-81d1-7bd05445c1ee` 中的 `bake-downloads` 服务在 `https://bake.justar.dev` 提供发行清单、安装脚本和版本化平台归档。服务已经创建，但只有在归档完成构建、检查和部署后，发行版才可下载。Bake 仍在 Node 上运行；Bun 负责构建工作区，并把生产依赖安装到每个平台归档中。

## 在本机构建并检查一个平台

使用装有稳定版 Bun 1.4.2（固定在 `package.json` 中）、Node 24 或更新版本及本机原生构建依赖的干净检出。先运行 `bun install --frozen-lockfile` 和 `bun run build`，再运行：

```sh
bun run release:pack
bun run release:assemble
bun run release:verify-local
```

`release:pack` 创建 `.artifacts/bake-release/<Bake 版本>/bake-v<Bake 版本>-<平台>.tar.gz`。它要求根目录与 CLI 的版本一致，复制包声明的载荷根目录、许可证、更新日志和已构建文件，然后在临时工作区执行冻结的 Bun 生产依赖安装，并启动暂存的 CLI。`release:assemble` 为当前 Bake 版本的每个归档计算哈希，写入 `distribution/host/public/latest.json` 和版本化归档。本机检查通过 HTTP 提供这些文件，把对应平台的安装脚本安装到临时目录，并启动安装后的命令。这些生成目录已被 Git 忽略。

每个目标都要在对应宿主机上构建：`darwin-arm64`、`darwin-x64`、`linux-arm64`、`linux-x64` 和 `win32-x64`。公开发行前，把所有归档收集到同一个 `.artifacts/bake-release/<Bake 版本>/` 目录，再运行 `bun run release:assemble --complete`，以拒绝缺少目标的发行版。单台宿主机的构建只能证明自己的目标；Windows 安装脚本和原生模块需要在 Windows 上运行检查。当前 Bake 版本为 `0.1.0`。

## 发布已检查的发行版

清单包含全部目标且每个归档通过本机检查后，在仓库根目录部署下载服务：

```sh
railway up ./distribution/host --path-as-root --no-gitignore \
  --project 28ff3000-7240-4c57-81d1-7bd05445c1ee \
  --environment production --service bake-downloads \
  --detach --json -m "Bake CLI direct download release"
```

`--no-gitignore` 会包含生成的 `public/` 载荷。Docker 构建会核验全部五个平台归档及其哈希，因此只有部分平台的本机清单无法发布。记录部署 ID，等待该部署达到 `SUCCESS`，然后通过公开域名检查 `/health`、`/latest.json`、两个安装脚本路径和一个平台归档。上传完成本身不能证明发行成功。

发布后，安装了 Node 24 或更新版本的用户可以运行：

```sh
curl -fsSL https://bake.justar.dev/install.sh | sh
```

```powershell
irm https://bake.justar.dev/install.ps1 | iex
```

安装脚本按宿主平台选择归档，对照清单验证 SHA-256，并在用户主目录下安装 `bake`。它们不会读取或迁移 `~/.dsh`；除非设置 `DSH_HOME`，命令使用 Bake 的 `~/.bake` 目录。Unix 需要 `curl`、`tar`，以及 `shasum` 或 `sha256sum`；Windows 需要 `tar.exe`。下载服务只提供发行文件，不保存 API 密钥。
