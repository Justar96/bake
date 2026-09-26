---
description: "HTTP Web 服务器与只读插件清单包的目录。"
kind: "package-group"
---

# host/ — Web GUI 宿主侧

[English](README.md) | 中文

## 概述

`host/` 组包含 HTTP Web 服务器和只读插件清单投影。各包 README 说明当前公开的路由与数据。

## 目录

- [包](#packages)
- [相关文档](#related-documentation)
- [开发备注](#dev-note)

-----

<a id="packages"></a>
## 包

八个包分别承担 Host 角色；各包的 README 拥有自己的约定与配置。

| 包 | 职责 | ctx 键 |
|---|---|---|
| [`webserver/`](webserver/README.zh.md) | 浏览器 HTTP 服务器：具名路由、upgrade、index 转换与回退席位 | `ctx.webServer` |
| [`plugin-inventory/`](plugin-inventory/README.zh.md) | 当前 Loader 条目的只读投影 | Remote `pluginInventory/list` |

-----

<a id="related-documentation"></a>
## 相关文档

先从传输与工作区记录的子系统参考读起，再看 Web Client 背后的分层决策。

- [HTTP 服务器子系统](../../docs/subsystems/web-server.zh.md)——webserver 的路由、匹配顺序与配置。
- [工作区子系统](../../docs/subsystems/workspace.zh.md)——目录选择器所喂给的工作区记录。
- [Web 配置树启动与传输分层](../../.agents/notes/implemented/architecture/2026-07-24-web-config-tree-boot-and-transport-layering.zh.md)——Web 传输各层的所有权。

<a id="dev-note"></a>
## 开发备注

<details>
<summary>维护者的工作上下文——点击展开</summary>

无。

</details>
