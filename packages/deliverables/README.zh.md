---
description: "显式文件交付工具的包组目录。"
kind: "package-group"
---

# packages/deliverables

[English](README.md) | 中文

## 概述

`deliverables/` 组包含 `tool-present`，让 agent 通过持久 Session 事件标识要交付的文件。该包 README 说明工具配置与事件约定。

## 目录

- [包](#packages)
- [相关文档](#related-documentation)
- [开发备注](#dev-note)

-----

<a id="packages"></a>
## 包

| 包 | 角色 | ctx key |
|---|---|---|
| [`tool-present`](tool-present/README.zh.md) | 通过 `present` 工具把已有文件声明为最终交付物 | 注册到 `ctx.tools` |

-----

<a id="related-documentation"></a>
## 相关文档

- [产出物子系统](../../docs/subsystems/deliverables.zh.md)——`PresentedFile` 与 `WorkspaceChangesSummary` 的词汇、两个持久事件和摘要服务。
- [present 声明工作区源文件](../../.agents/notes/implemented/feature/2026-09-08-present-workspace-source-files.zh.md)——交付决策。
- [本轮改动文件卡片](../../.agents/notes/implemented/feature/2026-09-11-turn-changed-files-card.zh.md)——快照设计与覆盖规则。

-----

<a id="dev-note"></a>
## 开发备注

<details>
<summary>维护者的工作上下文——点击展开</summary>

无。

</details>
