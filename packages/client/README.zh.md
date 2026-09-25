---
description: "共享 Client-to-Host Connection 包的目录。"
kind: "package-group"
---

# client/ — 共享 Connection

[English](README.md) | 中文

## 概述

`client/` 组目前包含共享的 Connection 包。它为使用 Client 传输的调用方传送类型化的 Client 到 Host 请求与事件。

## 目录

- [包](#packages)
- [相关文档](#related-documentation)
- [开发备注](#dev-note)

-----

<a id="packages"></a>
## 包

包 README 说明传输约定与配置。

| 包 | 职责 | ctx 键 |
|---|---|---|
| [`connection/`](connection/README.zh.md) | 维护浏览器与宿主之间的 RPC 通信与事件投递 | `ctx.connection` |

-----

<a id="related-documentation"></a>
## 相关文档

传输约定见 [Connection 包](connection/README.zh.md)；服务端包见[宿主组目录](../host/README.zh.md)。

<a id="dev-note"></a>
## 开发备注

<details>
<summary>维护者的工作上下文——点击展开</summary>

无。

</details>
