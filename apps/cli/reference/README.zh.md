# Bake Profile 启动器参考

[English](README.md) | 中文

`@deepseek-ai/dsh` 启动器通过具名 Cordis Profile 启动 Bake 的 Node 运行时。随附的 Profile 是用于终端 agent 的 `tui` 和用于单次任务的 `headless`。[启动器概览](../README.zh.md)说明常用命令。

<a id="profile-boot"></a>
## Profile 启动

每个 Profile 在 `$DSH_HOME/profiles/<name>` 下保存组合包列表与可选的 `cordis.patch.yml`。启动器依次应用组合包、Profile、home 和调用时指定的补丁。首次使用会初始化随附的 Profile；既有 Profile 保留其已选组合包。缺少组合包会导致启动失败。

启动器解析自身的 Profile 参数，并把其余参数转交应用。`--from-default-profile <template>` 从随附模板创建自定义 Profile；`--dump-default-config` 与 `--dump-config` 可以在不启动 agent 的情况下查看组合。

<a id="source-execution"></a>
## 从源码运行

在仓库根目录，`bun run build` 构建 Node 运行时与 TUI，`bun run start` 启动构建后的终端 agent。`bun run dsh` 运行构建后的 Profile 启动器。Bake 默认使用 `~/.bake`；`DSH_HOME` 可选择其他 home。外部 Profile 包安装仍由 pnpm 管理，与 Bun 源码工作区分开。

<a id="startup-diagnostics"></a>
## 启动诊断

必需插件激活失败时，启动器报告失败与待激活插件、缺失服务及原始错误。能够写入时，它在所选 home 的 `logs/` 目录下保存唯一命名的启动报告；写入失败也会显示在 stderr。进程以状态码 1 退出。原始插件错误可能包含配置值，因此分享报告前应先检查。
