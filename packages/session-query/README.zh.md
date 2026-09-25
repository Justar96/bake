---
description: "会话检索能力家族的包映射：搜索、追踪与读取实时和持久会话历史，以及 Web 端会话日志导出。"
kind: "package-group"
---

# session-query/：会话检索能力家族

[English](README.md) | 中文

## 概述

`session-query/` 组提供对实时及持久会话历史的有界检索，包括精确日志、过滤列表、关系追踪和 SQLite 全文搜索。服务与后端 README 说明查询及索引约定。

## 目录

- [包](#packages)
- [相关文档](#related-documentation)
- [开发备注](#dev-note)

-----

<a id="packages"></a>
## 包

每个包的 README 都会说明该包在此组中的用途。

| 包 | 职责 | ctx 键 |
|---|---|---|
| [`session-query/`](session-query/README.zh.md) | 统一的会话历史查询服务：精确读取、关系追踪与过滤 | `ctx.sessionQuery` |
| [`session-query-sqlite/`](session-query-sqlite/README.zh.md) | 基于 SQLite FTS5 索引的会话历史全文搜索 | 注册到 `ctx.sessionQuery` |

-----

<a id="related-documentation"></a>
## 相关文档

先从子系统参考了解共享的查询词汇，再看追踪与搜索背后的设计记录。

- [会话查询子系统参考](../../docs/subsystems/session-query.zh.md)——逻辑记录、过滤器、搜索页、血缘、有界读取与事件关系。
- [会话查询关系追踪](../../.agents/notes/archived/feature/2026-07-13-session-query-tracing.md)——追踪语义与校验边界。
- [SQLite FTS5 会话搜索](../../.agents/notes/archived/feature/2026-07-10-sqlite-session-query-provider.md)——搜索语义、对账与 tokenizer 决策。

<a id="dev-note"></a>
## 开发备注

无。
