---
description: "应用 Remote 层的包映射：类型化的 Client 到 Host 能力调用、结果与转发事件，供用户与维护者浏览该组。"
kind: "package-group"
---

# api/ — Remote API 层

[English](README.md) | 中文

## 概述

`api/` 组包含 Typert Gateway 和设置控制器。Gateway 在 Client 与 Host 之间传送类型化调用及事件；设置控制器负责配置的读取与写入。各包 README 说明其现有约定。

## 目录

- [包](#packages)
- [相关文档](#related-documentation)
- [开发备注](#dev-note)

-----

<a id="packages"></a>
## 包

下面这些包共同提供 Remote 层；完整约定以各包 README 为准。

| 包 | 职责 | ctx key |
|---|---|---|
| [`gateway/`](gateway/README.zh.md) | 承载类型化一元调用、多路复用流与转发的 Host 事件。 | `ctx.typertGateway` / `ctx.remote` |
| [`settings-controller/`](settings-controller/README.zh.md) | 拥有 settings 域各 seam 之上的配置界面读写。 | `ctx.settingsController`、`ctx.credentialsController` / `ctx.remote.settings`、`ctx.remote.credentials` |

Remote 调用沿 Client → Host 方向运行在应用共享的 Connection 之上。API Gateway 拥有 Remote 传输，各控制器包分别拥有 Session、配置界面与 Workspace 行为。流式下载等不适合 Remote 调用的响应由功能包注册精确的 Connection Fetch 路由。

-----

<a id="related-documentation"></a>
## 相关文档

先读 API Gateway 参考以端到端了解 Remote 模型，再读 Typert 子系统页了解共享定义，并通过 Connection 了解物理载体。

- [API Gateway 参考](../../docs/api-gateway.zh.md)——Typert API Gateway 的现状参考：编程模型、生成流水线与运行时调用。
- [Typert 子系统参考](../../docs/subsystems/typert.zh.md)——protocol、Gateway 与消费方装配共享的公共约定。
- [Connection](../client/connection/README.zh.md)——每次 Remote 调用背后的 RPC 载体、`/api` 信任围栏与响应封装。

<a id="dev-note"></a>
## 开发备注

<details>
<summary>维护者的工作上下文——点击展开</summary>

无。

</details>
