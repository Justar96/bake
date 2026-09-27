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

<a id="config-schema-dump"></a>
### 配置 schema dump

`--dump-config-schema` 使用与 `--dump-config` 相同的组合包、profile、home 和 argv patch 层，支持可重复的 `--patch` 与 `--from-default-profile`。三种 dump flag 互斥，并拒绝应用参数和保留的 `desktop` profile。组合成功后，stdout 输出一份缩进排版的 JSON Schema 2020-12 文档。根 schema 描述 `--dump-config` 输出经解析后的 entry list；`$defs.patchList` 单独描述 profile/home/CLI overlay。验证该片段时应保留文档的 `$defs`。准备、patch 解析/组合或解析器设置失败时，以非零退出码结束，不输出 schema。收集或投影失败时保留部分输出并退出 1；诊断也会输出到 stderr。即使 stdout 中的 schema 有效且可用，`partial` 投影或省略非 JSON 注释也适用此退出码。schema dump 的未匹配目标警告不包含层标签；需要来源标签时，使用相同配置层运行 `--dump-config`。

插件 Config 的字段、默认值、描述及支持的约束从原生 Schemastery 声明投影。普通字段内联，共享 Config 和递归使用 `$ref`。JSON Schema 的默认值是注释，不执行填值。必填字段会考虑 Schemastery 的 nullable fallback 能否通过验证。联合类型使用 `anyOf`，而原生执行仍选择首个成功分支。`secret`、`credential-ref`、`ms` 等 role 元信息及 `volatile` 实时更新元信息保留在 `x-cordis` 注释中。非法 volatile 嵌套属于 schema 定义错误；字段输入类型不会变成引用对象类型。回调验证、不支持的正则语义及其他未投影约束会标记为 partial，而不是静默丢弃。非有限数边界和非 JSON 默认值/展示注释会被省略并附上限制说明，结构字段仍可用。对象常量保留普通 nullable 成员约束，但继承属性的比较会标记为 partial。无法表示的默认值、不支持的交集及无法求解的递归默认值依赖保留未知的省略行为，仍需原生验证。

使用 Cordis entry-list 方言解析 YAML：`!!js` 标量变为不执行的 `{ "__jsExpr": "..." }` 标记。普通 Config 值和 entry 的 `disabled` 接受这些标记，但不求值其结果。Group 列表和 Include 字段保持字面量。entry id 可省略；缺少非空 id 的非 insert patch 被接受为无操作，并由 Loader 警告。禁用项可以省略必需的 Config，除非 `group: true` 强制激活；disabled 表达式也使省略行为留待运行时决定。已提供的 Config 值仍接受验证。`disabled` 接受布尔值、null 和表达式标记；Loader 会把其他真值强制视为禁用，但此 schema 拒绝它们。未设置 `group: true` 的禁用 group 或 include 的子项不接受验证，因为 Loader 从不创建它们。禁用的 `group: true` 行之下的子项，以及插入到禁用 group 的 patch，都按启用状态验证，尽管 Loader 不会初始化祖先被禁用的子项。已知根树目标验证完整 Config 替换，而不是深层 partial 对象。不推断 Include 内部 id，也不推断前序 patch 新增或改变的顺序相关目标。未知插件名保持开放；同名插件解析出不同 schema 时，使用联合约束并报告歧义。

根 `x-cordis` 注释包含 `profile`、`complete`、`entries`、`diagnostics` 和 `patchSchema`。配置项按遍历顺序保留 `path`、可选 `id`、`name`、`status` 和 `configRef`；状态为 `schema`、`partial`、`absent`、`unsupported` 或 `error`。可选的 `tree: "group" | "include"` 标识原生承载插件：它们可能因未导出 Config 而具有 `status: "absent"`，但 `configRef` 指向 Loader 结构定义。被检查的非法条目会获得带位置的错误，不丢弃有效的相邻条目；没有字面量名称时省略 `name`。列表容器非法或 include patch 组合失败时，在承载条目上报告诊断；组合失败后该 include 的子项不可用，不会把未应用 patch 或过滤后的子项当作最终结果。未设置 `group: true` 的禁用承载条目可以省略 config，并记录为没有子项的树。投影限制会在共享该 Config 的每个条目上重复报告。Config 缺失表示字段未知，不表示禁止配置。group 子项的发现路径追加 `/config/<index>`，include 子项追加 `/include/<index>`；后者不是根 dump 中的 JSON Pointer。存在错误诊断、`partial`/`unsupported`/`error` 条目，或同一插件名对应多个 Config 定义时，`complete` 为 false。此参考覆盖所有声明，包括禁用项：它们的导入或 include 失败可能使可启动的 profile 也被标为不完整。运行时生成的 preset/客户端树及插件启动检查不属于此参考范围；反过来，`complete` 也不保证启动成功。

可能修改输入的前序 union 分支，以及可能改名或碰撞的字典键，需要放宽验证。Lazy 元数据传播可能影响当前 Config 之外的共享节点，因此该 Config 保留可获得的声明细节，同时增加不受限制的备选项，而不模拟原生修改。这些情况标记为 partial，仍需原生验证。

收集会导入可信模块，也可能调用 Config getter 和 lazy builder，但绝不应用插件、执行 transform 回调或求值配置表达式。导入可能在输出前后阻塞或保留进程句柄；自动调用方应设置外部超时。dump 不强制退出，也不持有导入期间资源的释放职责。导入或 builder 对 stdout 的常规写入转向 stderr；直接写文件描述符不被拦截。profile 准备保留 YAML dump 的初始化写入。输出只有 schema 声明，没有实际配置值；声明的默认值和插件原始错误仍可能含敏感数据，分享前应检查。收集器 API 参见 [app-boot](../../../packages/boot/app-boot/README.zh.md)。

输出是可重新生成的 pre-stable 参考，不是单独版本化的持久化目录。`$schema` 标识 JSON Schema 验证方言；`x-cordis` 随 dsh 版本演进。更改 dsh 或插件后应重新生成，并跟随 `$ref` 和 `configRef`，而非硬编码定义名、顺序或文本。不兼容的方言变更使用新的 `$schema`；注释变更不采用 Session 格式迁移。
