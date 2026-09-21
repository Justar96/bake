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

从包入口导入 `App` 和 `AppProps`。按[运行器](../app/src/runner.ts)和[组件测试](tests/shell.spec.tsx)所示提供权威状态和回调。`project` 将已提交的会话事件映射为零个或多个行；`formatRow` 提供纯文本表示。此包是库依赖，没有 Cordis 挂载行。

使用 `dictionaries.en` 或 `dictionaries.zh` 获取应用标签。模型文字、工具输出、问题详情和服务诊断保持原样。已提交行必须保持不可变且只能追加，因为 Ink `Static` 只打印一次。

<a id="understand-the-implementation"></a>

## 了解实现

<details>

<summary>输入与展示</summary>

Ink `usePaste` 插入文字而不执行动作；`useInput` 处理键盘输入。`useComposer` 处理同时到达的文字与 Enter，按字素删除可保持 Unicode 字符完整。`InteractionView` 展示完整问题和计划详情，对密钥使用掩码，并提交精确的选项标签。请求 ID 使应用能够拒绝过期回调。

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

- 输入框支持追加文字和 Backspace；未实现光标移动和历史编辑。

### 开发备注

无。
