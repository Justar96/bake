# Bake profile 启动器

[English](README.md) | 中文

`@deepseek-ai/dsh` 包通过命名 Cordis profile 启动 Bake 的 Node 进程。`tui` 启动终端 Agent；`headless` 执行单个任务后退出；`desktop` 是 Bake Desktop 应用启动的长驻桥接。每个 profile 都在首次使用时初始化。包标识和 `dsh` 命令保持与共享运行时的包解析兼容。

## Profile

每个 `$DSH_HOME/profiles/<name>` 包含列出有序组合包的包清单，以及可选的用户 `cordis.patch.yml`。终端模板选择 `@deepseek-ai/dsh-base` 和 `@dsh-tui/app`；headless 模板选择 base 和 `@deepseek-ai/dsh-headless`，desktop 模板选择 base 和 `@deepseek-ai/dsh-desktop`。

配置层依次应用：组合包补丁、profile 补丁、home 补丁、调用时的 `--patch` 文件。缺少组合包会明确失败。现有用户 profile 保留其组合包选择；启动不会将其重写为模板内容。

`--from-default-profile <template>` 从随附模板创建新名称的自定义 profile。`--dump-default-config` 和 `--dump-config` 在不启动 Agent 的情况下检查组合。启动器原样转发自身选项之后的应用参数，不解释其含义。

使用 `--dump-default-config` 和 `--dump-config` 可在不启动的情况下检查组合后的配置树。`--dump-config-schema` 会导入组合树中插件声明的 schema，并打印描述 entry 与 patch 的 JSON Schema，而不是配置值；检查不受信任的插件前，请阅读 [schema dump 的安全性与范围](reference/README.zh.md#config-schema-dump)。

## 开发

在仓库根目录运行 `bun run build` 构建运行时和终端包。`bun run start` 使用生产版 React 启动终端；`bun run start --help` 显示选项。使用 `bun apps/tui/scripts/tui.ts e2e` 通过真实 PTY 进行无密钥录制重放验证。

`bun run dsh` 运行构建后的启动器，使用与 `bun run start` 相同的 Bake 数据目录 `~/.bake`；显式设置 `DSH_HOME` 可覆盖它。npm 的 `dsh` 入口和直接运行 `node apps/cli/lib/bin.js` 也使用相同的默认目录。修改启动器后需重新构建。

[直接下载归档](../../distribution/README.zh.md) 会在此启动器外安装 `bake` 包装命令。默认启动 `tui` profile；`bake tui`、`bake headless`、`bake plugin`、`bake update` 和 `bake --profile` 会转交给 profile CLI。除非设置 `DSH_HOME`，该命令使用 `~/.bake`。`dsh update` 用最新的已签名发行版替换该安装，`dsh update --check` 只报告结果，有更新版本时退出码为 10；与 `plugin` 一样，开头的 `update` 保留给此用途，名为 `update` 的 profile 需用 `--profile update` 访问。参见[更新安装](../../distribution/README.zh.md#updating-an-install)。

交互式更新在 stderr 的一行上烘焙面包，面包随下载和安装进度变色，并在结果或错误前清除。`BAKE_NO_ANIMATION=1` 关闭动画，`NO_COLOR=1` 关闭颜色。仅检查、重定向输出、CI 和不支持控制序列的终端保留纯文本报告。

## 启动与关闭

npm 的 `dsh` 入口在加载应用之前，以 `--report-exclude-env --report-exclude-network --diagnostic-dir=<Bake 主目录>/diagnostics` 重启 Node。它尽可能创建仅所有者可访问的诊断目录。在 POSIX 上，进程替换保留进程 ID 和终端信号传递；在 Windows 上，父子进程共享控制台事件，父进程等待子进程的退出状态。发行版和开发启动器已提供这些参数，因此直接运行。这些参数不会添加到 `NODE_OPTIONS`，所以智能体启动的命令不会继承它们。运行时看门狗启用致命错误报告；堆快照仍需显式启用。

入口在加载应用之前开启 Node 的模块编译缓存，之后的启动会复用启动器和 profile 模块已编译的代码。缓存使用 Node 默认的 `<tmpdir>/node-compile-cache`，且仅当该目录归当前用户所有、其他用户无法写入时才启用。Node 按自身版本、用户以及文件路径和内容为每个条目建立键。profile 启动完成后，启动器写入缓存；之后加载的模块由 Node 在进程退出时补充写入。`NODE_DISABLE_COMPILE_CACHE=1` 关闭缓存，设置 `NODE_COMPILE_CACHE` 目录则替代默认位置。Bake 启动的进程不继承该缓存。缓存缺失或不可写都不会阻止启动。

每次启动都会把 profile 的根 `cordis.yml` 恢复为空的 entry 列表。内容未变时不改动文件；内容变化时以原子方式替换，因此同一 profile 的并发启动永远不会读到不完整的文件。

`SIGINT` 以 130 退出，`SIGTERM` 以 0 退出，二者都会先 dispose 应用：dispose 会刷写会话并停止受管子进程。卸载插件树之前，启动器会等待 `app/shutdown`，让运行中的 agent 在会话持久化服务仍挂载时取消轮次并写入收尾事件。dispose 最多等待 5 秒，第二个信号会强制退出。终端关闭时（在 Windows 上为控制台关闭时）收到的 `SIGHUP` 执行同样的 dispose 并以 129 退出。写入已断开的终端会失败，因此启动器此后忽略 stdio 错误。重复的 `SIGHUP` 会等待进行中的 dispose，而不会强制退出。

## 外部插件

`bun run dsh plugin --profile <name>` 将包操作委托给 profile 的 pnpm 配置。这与 Bun 管理的 Bake 源码工作区独立。Profile 管理器负责安装批准、锁、回滚和配置重载，参见[插件管理器](../../packages/boot/plugin-manager/README.zh.md)。

启动器保留启动诊断、代理设置、profile 重载和有界关闭。[App boot](../../packages/boot/app-boot/README.zh.md) 负责共享启动行为；[`src/args.ts`](src/args.ts) 负责参数解析。
