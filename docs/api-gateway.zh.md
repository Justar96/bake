# API Gateway

[English](api-gateway.md) | 中文

本文是 Typert API Gateway 的当前状态参考。它描述业务服务如何声明一元 Remote 方法、Host 构建如何生成 Host 与 Host-for-Client 约定，以及调用如何复用 Connection 的 RPC 与 `/api` 路由。用 `@Remote({ mode: 'stream' })` 声明的流式 Remote 方法和转发的 Host 事件由 [API Gateway 包 README](../packages/api/gateway/README.zh.md) 描述，不在本文范围内。

## 编程模型

业务服务通过 `@Remote` 或 `@RemoteScope` 选择对 Client 开放的方法。未标记的方法不会进入生成的 Client 类型或运行时贡献，也不能通过 `ctx.remote` 调用。

`@Remote` 表示调用根 Host Context 中注册的 Cordis 服务。复杂的 Host 对象不能直接跨 wire 传输；业务包必须通过 `TypertLookupMap` 声明它与 wire identity 的关联，并在运行时向 `ctx.typert.lookups` 注册默认解析提供方。例如 `Agent` 参数在 Host 签名中名为 `agent`，生成的 wire 字段为 `agentId`，Gateway 在调用业务方法前将 id 解析为 Host 对象。Host 组合可以用 `ctx.typert.lookups.configure()` 覆盖某个 lookup key 的解析策略，而不改变业务包拥有的参数名、wire 字段或规范类型 symbol。

`@RemoteScope(key)` 表示先通过 `ctx.typert.contexts` 把 identity 解析为一个作用域 Context，再从该 Context 取得服务并调用方法。它适用于方法本身依赖作用域组合、而不需要显式接收 `Agent` 等对象的情形。

服务通常继承 `TypertRemoteService`，让 Cordis 服务 key 与默认 Remote namespace 在构造器中显式绑定。已有其他基类的服务可以改为声明 `readonly typertRemote = bindTypertRemote(this, serviceKey)`；两种方式都会留下可检查的公开 binding，不依赖编译器向构造函数注入 symbol。

```ts
import type { Agent } from '@deepseek-ai/dsh-agent'
import { TypertRemoteService, Remote, RemoteScope } from '@deepseek-ai/dsh-typert-protocol'
import type { Context } from '@deepseek-ai/cordis'

export interface CreateGoalRequest {
  objective: string
}

export interface CreateGoalResult {
  accepted: boolean
}

export class GoalService extends TypertRemoteService {
  constructor(ctx: Context) {
    super(ctx, 'goals')
  }

  @Remote('create')
  createForClient(
    agent: Agent,
    request: CreateGoalRequest,
    signal: AbortSignal,
  ): CreateGoalResult {
    signal.throwIfAborted()
    return this.create(agent, request)
  }

  @RemoteScope('agent', 'current')
  currentForClient(): CreateGoalResult {
    return { accepted: true }
  }

  private create(_agent: Agent, request: CreateGoalRequest): CreateGoalResult {
    return { accepted: request.objective.length > 0 }
  }
}
```

Remote 方法可以同步返回或返回 Promise。若需要协作式取消，Host 签名的最后一个参数必须是全局类型的 `signal: AbortSignal`；它记录在描述符中而不是进入 `args`，Client 生成的方法则接受最后一个可选的 `AbortSignal`。

Client 使用普通对象上的具体函数，不使用 JavaScript Proxy。每个 namespace 都是注册为 `remote.<namespace>` 的可追踪 Cordis 子服务；`ctx.remote.$mount()` 挂载一个贡献，最后一个方法撤回后该 namespace 随即卸载。读取 `ctx.remote.<namespace>` 的业务包在自己的 `inject` 中同时声明 `remote` 与 `remote.<namespace>`。当通过 `ctx.typert.contexts.registerClient()` 注册的 Client Context 适配器能为调用方 Context 给出 identity 时，该调用就是作用域调用。当一个 `@Remote` 方法恰好有一个 lookup 参数、且同名 `TypertContextMap` 使用相同 wire identity 时，生成的作用域签名会省略该 identity 参数。`@RemoteScope` 只生成作用域调用接口。每个生成的一元方法都解析为 `RemoteResult<T>`。

```ts ignore-check
import type { SessionId } from '@deepseek-ai/dsh-session/types'
import type { ClientRemote } from '@deepseek-ai/dsh-api-gateway/client'
import type { TypertRemoteScopeApi } from '@deepseek-ai/dsh-typert-protocol'
import type { Context } from '@deepseek-ai/cordis'
import type {} from '@deepseek-ai/dsh-goal/remote'

export const inject = ['remote', 'remote.goals']

declare const ctx: Context
declare const agentCtx: Context & { readonly remote: ClientRemote & TypertRemoteScopeApi<'agent'> }
declare const agentId: SessionId

await ctx.remote.goals.create(agentId, { objective: 'ship it' })
await agentCtx.remote.goals.create({ objective: 'ship it' })
```

目前没有任何应用装配 Client 侧。业务包仍发布 `./remote` 贡献，但 `packages/api/gateway/tests` 之外没有代码导入它们或调用 `ctx.remote.$mount()`，也没有包注册 Client Context 适配器。本文关于 Client 的描述以 `@deepseek-ai/dsh-api-gateway/client` 的源码与测试为准。

## 组件职责

| 位置 | 包或入口 | 职责 |
|---|---|---|
| 共享 | `@deepseek-ai/dsh-typert-protocol` | 声明 decorator、Gateway binding、可合并协议映射、调用描述符及提供方类型；不启动 TypeScript 分析，也不注册 Cordis 服务 |
| 构建 | `@deepseek-ai/dsh-typert-generator` | 从 Host `ts.Program` 严格分析 Remote 签名、类型图、lookup、Context 与源码位置，并生成 Host 和 Host-for-Client 产物 |
| Host | `@deepseek-ai/dsh-typert-registry` 与 Loader | 把生成的 Host 描述符、schema 及业务包注册项放入 `ctx.typert`，并持有 lookup 与 Context 提供方 |
| Host | `@deepseek-ai/dsh-agent` 与 `@deepseek-ai/dsh-session` | 注册 `agent` 与 `session` lookup，以及 `agent` Host Context |
| Host | `@deepseek-ai/dsh-api-gateway` | 提供 `ctx.typertGateway`，认领 Remote endpoint，校验请求值，解析对象或 Context，并调用实时 Cordis 服务 |
| Client | `@deepseek-ai/dsh-api-gateway/client` | 提供 `ctx.remote` 与 `remote.<namespace>` 子服务，把生成的描述符挂成具体方法，并通过 Connection 发起和取消调用 |
| 双侧 | `@deepseek-ai/dsh-client-connection` | 提供 RPC carrier、请求关联、信任边界、取消、响应 envelope 与 `/api` HTTP bridge |

API Gateway 包同时拥有 Host dispatcher 与 Client Remote endpoint 两个对等入口，分别由 `tsconfig.host.json` 与 `tsconfig.client.json` 两个独立 project 编译，因此两侧不会进入同一个 `ts.Program`。Host 入口不导入 Client 的 Cordis `Context` 合并，Client 入口也不导入 Host Gateway 服务。

## 严格生成流水线

`bun run build` 在 `build:runtime` 中执行 `build:lib:host`，再构建 TUI。`build:lib:host` 先运行 `tsc -b tsconfig.host.json`，再运行 `tsdown`；根 `tsdown.config.ts` 加载 `typertPlugin({ mode: 'workspace', faces: ['host'] })`，该插件由正常 Host Project Reference 图编译，并在这次 tsdown 中以 Host aggregate 为唯一 `ts.Program` 种子运行。没有任何构建阶段编译或打包 Client 侧。

tsdown 接收 `vendor/*`、`packages/*/*` 与 `apps/cli` 下的 workspace 包，且只打包 tsc 发射的 `lib/types/{index,invariant,startup}.js` 入口。

`api/gateway` 与 `client/connection` 拆分 TypeScript face：各自带有 `tsconfig.host.json` 与 `tsconfig.client.json`，根 `tsconfig.host.json` 与 `tsconfig.client.json` aggregate 分别引用对应的 face。

每个贡献业务包把生成文件写入自己的 `lib/`，而不是源码目录：

| 文件 | 消费方 | 内容 |
|---|---|---|
| `typert.host.js` | Host Loader | Host face 的运行时反射、严格调用描述符和 schema 注册值 |
| `typert.host.d.ts` | Host 类型系统 | Host face 的生成声明 |
| `typert.remote-client.js` | `ctx.remote.$mount()` | 可挂载的 `TypertRemoteContribution`，包含严格描述符与运行时 codec |
| `typert.remote-client.d.ts` | Client 类型系统 | `TypertRemoteNamespaceMap` 与 `TypertRemoteScopeMap` 的声明合并及 Client-safe 类型引用 |
| `typert.remote-client.d.ts.map` | 编辑器 | 将生成的方法属性映射回 Host 包中的 Remote 方法声明 |

业务包通过 `./typert` 暴露 Host Loader 入口，通过 `./remote` 暴露 Host-for-Client 入口。生成器同时校验这些包 export 及发布文件清单；只有具备相应入口的显式贡献包才会生成产物。

Remote Client 声明中的参数名来自 wire 字段，参数和返回类型则引用原业务包导出的 Client-safe 类型。声明 map 把 `ctx.remote.goals.create` 最终解析到的生成属性映射到带 `@Remote` 的 Host 源方法，因此支持 declaration-map 的编辑器可以从 Client 调用跳到真实实现，而不是停在生成的 `.d.ts`。

严格分析要求 Remote 是公开、非静态、有具体实现的实例方法。方法不能是泛型；参数必须是具名的简单标识符，不能使用解构、默认值或 rest 参数，lookup 参数还必须必填并以其 lookup key 命名。可 JSON 表示的普通类型由 Typert 生成严格 schema；工作区 class 等复杂对象必须具有唯一的 `TypertLookupMap` 声明。lookup 与 Context 包同时负责静态声明合并和运行时提供方注册；缺少任一侧都会导致构建失败，或者首次调用需要该提供方时失败。

## 运行时调用

Remote 调用使用 Connection 的 `/api` 路由。Client Remote 调用 `connection.rpc.call('/api', '<namespace>/<method>', { args }, signal)`；HTTP carrier 对应 `POST /api/<namespace>/<method>`，payload 只包含一个具名 `args` 对象。

Connection 在 HTTP bridge 之前执行 `/api` 的统一信任检查，再在共享 FetchHandler 内分发。Typert Gateway 只认领现有或曾有严格描述符、或与活跃 SRC marker 匹配的两段式 endpoint；功能自有的精确 Fetch 路由处理非 JSON 响应，其他请求返回 404。Connection 拥有传输、RPC id、响应 envelope 和请求取消，Gateway 只拥有 Remote 数据协议和业务分发。Gateway 只在存在 `connection` 服务时安装 `/api` 拦截器，而 base bundle 加载 Typert registry、Loader 与 Gateway，但不加载 Connection。

Gateway 每次调用都从当前注册表解析描述符和实时服务，不缓存业务对象。它要求 `args` 的字段与描述符一致，只允许省略可选的 JSON 字段；随后解析接收者、检查服务 binding、用 codec 解码 wire 值、通过注册的 lookup 提供方解析对象，最后调用 binding 指向的服务方法。业务结果不经解码直接返回。缺少提供方、identity 未命中、binding 不一致、参数缺失或多余、codec 失败和方法不存在都会在进入业务代码前失败。

lookup 提供方的 `register()` 同时提供稳定声明和默认 resolver；`configure()` 提供由 Host 组合拥有、可异步执行且受 effect 生命周期约束的 resolver。配置可以先于提供方挂载；没有提供方时调用仍以 `gateway/lookup-unavailable` 失败，配置卸载后则恢复提供方默认策略。`@deepseek-ai/dsh-agent` 与 `@deepseek-ai/dsh-session` 注册默认的 `agent` 与 `session` resolver，它们只从各自的 store 返回 live Agent 或 Session；目前没有包调用 `configure()`。resolver 抛出的 `RemoteError` 原样把码带上 wire，其他 resolver throw 变为 `gateway/lookup-failed`，未解析到的 identity 以 `gateway/lookup-not-found` 失败；只有未归类的 throw 才折成 `gateway/internal`。

Client 卸载一个贡献时会一起移除描述符和具体方法，中止其进行中的调用，并使外部仍持有的陈旧方法句柄在后续调用中失败。Host 上已经注册过的严格 endpoint 被撤回后也不会降级到 SRC 推断，以免热卸载悄然降低校验强度。

## SRC 开发回退

Host 进程从源码运行时（例如通过 `node --import tsx/esm`）不会执行 Typert 编译插件。标准 decorator 初始化器仍会把方法名和调用模式记录到 Service 原型上的带版本描述符中，`TypertRemoteService` 或 `bindTypertRemote()` 则提供显式服务 binding；Gateway 因而可以在不启动 `ts.Program` 的情况下构造一个较弱的临时描述符。描述符使用稳定的字符串属性名，因此 `remoteMethods()` 能读取协议包另一个已安装副本写入的标记。

SRC 回退从运行中函数解析简单参数名。参数名与某个已注册 lookup 的 `parameter` 相同，例如 `agent` 或 `session`，就使用其 `agentId` 或 `sessionId` wire 字段并在 Host 解析对象；其他参数只检查值是否为无循环、无特殊 prototype 的 JSON-safe 数据。`@RemoteScope` 直接使用已注册 Host Context 提供方的 wire 字段。SRC 不读取 TypeScript 类型，也不生成 Zod schema；它无法区分可选参数，因此允许省略任何非 lookup 字段，并且不支持解构、默认值、rest 或重复参数名。

SRC 只解决 Host 源码进程的分发问题。Client 不会从运行中的 Host 发现 decorator，Client Remote 也拒绝挂载缺少严格 codec 的 SRC 描述符；其类型、codec 和 Remote 注册值始终来自最近一次生成的 `lib/typert.remote-client.*`。

## 开发模式

只修改 Remote 方法实现体而不改变约定时，无需重新生成 Typert 文件。新增或删除 decorator、修改导出名、namespace、参数、返回值、lookup、Context 或取消签名时，重新执行 Host lib 构建，让 Typert 插件重新生成严格约定：

```sh
bun run build:lib:host
```

`bun run build` 会在构建 TUI 之前执行同一阶段。`bun run typecheck` 运行 `tsc -b tsconfig.host.json` 与 TUI 类型检查，但不运行 tsdown，因此不会重新生成 Typert 产物。`bun run start` 与 `bun run dsh` 启动的是构建后的 CLI 而非源码 Host，因此使用上次构建生成的严格描述符。

## 边界

一元 Remote 方法有单个请求与单个结果。用 `@Remote({ mode: 'stream' })` 声明的方法改为返回 `Iterable` 或 `AsyncIterable`，其描述符带有 `mode: 'stream'`，且不能通过一元 `/api` 分发调用。

API 各层按 `gateway → connection → webserver` 组织。Typert Gateway 与 settings controller 位于 `packages/api`；Connection 与 WebServer 位于 `packages/client/connection` 和 `packages/host/webserver`。需要非 JSON 或浏览器原生响应的功能注册精确的 Connection Fetch 路由，而不定义 Remote 方法。

lookup 策略按 key 配置，因此所有 `agent` 或 `session` 参数共享同一个 resolver。为单个参数或 endpoint 选择不同策略需要显式的逐参数或逐 endpoint 策略，而这种策略并不存在；不能通过业务方法内部猜测对象是如何解析的。
