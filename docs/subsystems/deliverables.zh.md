# 产出物

[English](deliverables.md) | 中文

`present` 工具让 agent 声明要交付的文件，并将声明写入仅记录日志的 Session 事件。[包组目录](../../packages/deliverables/README.zh.md)与 [tool-present README](../../packages/deliverables/tool-present/README.zh.md)说明当前配置和行为。

源码：[`packages/deliverables/tool-present/src/types.ts`](../../packages/deliverables/tool-present/src/types.ts)

## `PresentedFile`：一条声明的交付

```ts type-equiv
/** A declared filesystem file whose current contents remain at its source path. */
interface PresentedFile {
  /** Original absolute path or path relative to the Session working directory. */
  path: string
  /** Optional description supplied by the model. */
  description?: string
}
```

## 持久交付事件

`tool-present` 将 `deliverables/presented: { turn; callId; files: PresentedFile[] }` 合并到 `SessionEventMap`，并在 `present` 成功后追加该事件。交付声明不会进入模型上下文。
