---
description: "使用纯 Ink 展示已提交的会话行、实时输出、待处理输入和人工交互。"
kind: "package-library"
---

# @dsh-tui/ui

[English](README.md) | 中文

## 概要

此库呈现 `@dsh-tui/app` 提供的终端状态。`App` 展示已提交历史、实时回复文字、待处理输入、状态和一个人工请求。组件使用类型化的语言字典和回调，不访问 Cordis、Node 服务、存储或时钟。

## 目录

- [使用此包](#use-this-package)

- [了解实现](#understand-the-implementation)

- [模型体验](#model-experience)

- [已知限制](#known-limitations)

<a id="use-this-package"></a>

## 使用此包

从包入口导入 `App` 和 `AppProps`。按[运行器](../app/src/runner.ts)和[组件测试](tests/shell.spec.tsx)所示提供权威状态和回调。`project(event, projector)` 将已提交的会话事件映射为零个或多个行；用 `projector(copy, lookup)` 为每条记录构建一个投影器，其中 `lookup` 把日志记录的工具名解析为它的 `presentCall` / `presentResult`——传入 `() => undefined` 则所有工具都按原始参数呈现。`formatRow` 提供纯文本表示。通过 `completion` 提供会话拥有的命令和技能元数据及发现状态，并用正整数 `completionLimit` 指定可见菜单行数。菜单筛选开头的斜杠词，按 Tab 插入所选名称，Escape 先关闭菜单，随后才向应用传递取消操作。粘贴只修改草稿。此包是库依赖，没有 Cordis 挂载行。

提供带查询标记的 `files` 和 `onReferenceQuery` 以发现工作区路径。回调接收活动查询，菜单关闭时接收 `undefined`。只能选择与当前查询匹配的结果。从 Harness 语法模块导入的纯函数 `activeAtToken` 和 `formatFileMention` 负责识别词和添加引号；文件 Tab 补全保留周围文字，目录 Tab 补全则保持发现菜单打开。

输入框支持在光标处插入、按字素移动和删除、用 Home/End 移到逻辑行首尾，以及用 Shift-Enter 换行。Enter 提交完整草稿。补全菜单关闭时，上下方向键浏览人工输入；菜单打开时，Ctrl-P/N 也可调出历史。越过最新条目向后浏览会恢复原始草稿和光标位置。补全和编辑行为见[键盘操作](../app/README.zh.md#use-this-package)。

通过 `todos` 提供代理当前的任务列表，在它首次写入之前为 `undefined`；面板显示尚未完成的条目，并统计已完成的数量。通过 `attachments` 提供暂存元数据；待处理输入和用户行也可包含附件摘要。界面显示名称、字节长度和可用的图像元数据，不读取字节或路径。`onSubmit` 可返回 `false` 或 promise：拒绝或失败会保留草稿及光标，异步接收成功后才清除。等待接收期间暂停编辑、粘贴、历史浏览和重复 Enter，包括同次读取中解码的按键；Escape 和 Ctrl-C 仍可使用。存在暂存附件时，空草稿也可提交。

使用 `dictionaries.en` 或 `dictionaries.zh` 获取应用标签。模型文字、工具输出、问题详情和服务诊断保持原样。使用 `emptyTranscript` 和 `appendTranscript(previous, rows)` 构建 `committed`。快照及其行批次必须保持不可变且只能追加，因为 Ink `Static` 对每段新增内容只打印一次。`transcriptRows(snapshot, start)` 只读取此前已渲染行数之后的内容；流式更新不扫描已提交行。更改 `sessionId` 会重新挂载会话界面，重置草稿、光标、历史浏览和补全状态，并在历史前打印本地化的会话标题。导航期间设置 `inputBlocked` 可暂停输入框输入，同时保留 Escape 和 Ctrl-C；选择交互仍可使用。上下文估计值显示 `~` 前缀，待处理输入包含本地化的 `/clear-pending` 提示。

<a id="understand-the-implementation"></a>

## 了解实现

<details>

<summary>输入与展示</summary>

Ink `usePaste` 插入文字而不执行动作；`useInput` 处理键盘输入。`useComposer` 在 React 绘制前保留同次读取中的编辑结果。历史浏览按需遍历待处理人工输入和已提交的用户行，仅为访问过的条目保留本地编辑。输入和渲染不会扫描历史，调出或编辑条目也不会修改对话记录。文字问题和登录提示共用光标编辑功能，但没有历史来源。

`cards.ts` 把 `dsh-tools` 中带 `card` 标签的呈现意图映射为卡片行，`present.ts` 将它们放在所属调用之下。未声明呈现器的工具、查找不到的工具、比本次构建更新的卡片类型，以及抛出异常的呈现器，都保留原始参数和结果文本。呈现器对调用参数和持久结果（包括会话日志保存的 `meta`）是纯函数，因此重放会重现完全相同的卡片。

`InteractionView` 展示完整问题和计划详情，对密钥使用掩码，并提交精确的选项标签。`select` 交互显示可筛选的 `Picker`，行数由 `completionLimit` 控制。上下方向键移动选择；Enter 返回一个精确值，包括用于提供器默认值的空值。粘贴只筛选而不确认，没有匹配时仍可编辑输入。请求 ID 使应用能够拒绝过期回调。

所有权说明见[连接参考](../../DESIGN.md)，Ink 和 React 版本见[当前依赖 API](../../DEPENDENCIES.md)。组件仅呈现传入的属性，因此不安装独立的运行时不变量。

</details>

<a id="model-experience"></a>

## 模型体验

没有直接影响；应用负责提交已记录消息和服务答案的回调。

### KV Cache 影响

无；展示层不构建模型请求。

<a id="known-limitations"></a>

## 已知限制与后续工作

- 内联终端输出没有虚拟滚动或富附件查看器。

- `diff` 卡片标记补丁块两端共有行之间的区间。若变更内部也存在相同的行对，标记区间会相应变宽，而不会被拆分。

- 输入框不支持鼠标定位、按词移动或在自动折行之间上下移动；补全菜单关闭时，上下方向键用于浏览历史。

### 开发备注

无。
