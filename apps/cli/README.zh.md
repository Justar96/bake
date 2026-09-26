# Bake profile 启动器

[English](README.md) | 中文

`@deepseek-ai/dsh` 包通过命名 Cordis profile 启动 Bake 的 Node 进程。`tui` 启动终端 Agent；`headless` 执行单个任务后退出；`desktop` 是 Bake Desktop 应用启动的长驻桥接。每个 profile 都在首次使用时初始化。包标识和 `dsh` 命令保持与共享运行时的包解析兼容。

## Profile

每个 `$DSH_HOME/profiles/<name>` 包含列出有序组合包的包清单，以及可选的用户 `cordis.patch.yml`。终端模板选择 `@deepseek-ai/dsh-base` 和 `@dsh-tui/app`；headless 模板选择 base 和 `@deepseek-ai/dsh-headless`，desktop 模板选择 base 和 `@deepseek-ai/dsh-desktop`。

配置层依次应用：组合包补丁、profile 补丁、home 补丁、调用时的 `--patch` 文件。缺少组合包会明确失败。现有用户 profile 保留其组合包选择；启动不会将其重写为模板内容。

`--from-default-profile <template>` 从随附模板创建新名称的自定义 profile。`--dump-default-config` 和 `--dump-config` 在不启动 Agent 的情况下检查组合。启动器原样转发自身选项之后的应用参数，不解释其含义。

## 开发

在仓库根目录运行 `bun run build` 构建运行时和终端包。`bun run start` 使用生产版 React 启动终端；`bun run start --help` 显示选项。使用 `bun apps/tui/scripts/tui.ts e2e` 通过真实 PTY 进行无密钥录制重放验证。

`bun run dsh` 运行构建后的启动器，使用与 `bun run start` 相同的 Bake 数据目录 `~/.bake`；显式设置 `DSH_HOME` 可覆盖它。直接运行共享运行时启动器仍使用上游默认目录。使用 Bun 命令运行 Bake，修改启动器后需重新构建。

[直接下载归档](../../distribution/README.zh.md) 会在此启动器外安装 `bake` 包装命令。默认启动 `tui` profile；`bake tui`、`bake headless`、`bake plugin`、`bake update` 和 `bake --profile` 会转交给 profile CLI。除非设置 `DSH_HOME`，该命令使用 `~/.bake`。`dsh update` 用最新的已签名发行版替换该安装，`dsh update --check` 只报告结果，有更新版本时退出码为 10；与 `plugin` 一样，开头的 `update` 保留给此用途，名为 `update` 的 profile 需用 `--profile update` 访问。参见[更新安装](../../distribution/README.zh.md#updating-an-install)。

交互式更新在 stderr 上显示 ASCII 烘焙动画，并在结果或错误前清除动画。`BAKE_NO_ANIMATION=1` 关闭动画，`NO_COLOR=1` 关闭颜色。仅检查、重定向输出、CI 和不支持控制序列的终端保留纯文本报告。

## 外部插件

`bun run dsh plugin --profile <name>` 将包操作委托给 profile 的 pnpm 配置。这与 Bun 管理的 Bake 源码工作区独立。Profile 管理器负责安装批准、锁、回滚和配置重载，参见[插件管理器](../../packages/boot/plugin-manager/README.zh.md)。

启动器保留启动诊断、代理设置、profile 重载和有界关闭。[App boot](../../packages/boot/app-boot/README.zh.md) 负责共享启动行为；[`src/args.ts`](src/args.ts) 负责参数解析。
