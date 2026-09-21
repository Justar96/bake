/** Locale-owned terminal labels. Model, tool, and command text stays verbatim. */
export const dictionaries = {
  en: {
    ready: 'Ready', working: 'Working', stopping: 'Stopping', command: 'Command',
    pending: 'Pending input', nextStep: 'Next step', nextTurn: 'Next turn',
    help: 'Enter sends · Esc interrupts · Ctrl-C twice quits',
    steering: 'Enter steers the next step', quit: 'Press Ctrl-C again to quit',
    approval: 'Approval required', approve: 'Y allows once · N rejects',
    questions: 'Answer required', questionHelp: 'Enter an option number or a written answer',
    multiHelp: 'Enter option numbers separated by commas, or a written answer',
    cancelled: 'Interrupted', compacted: 'Context compacted',
    unknownCommand: 'Unknown command', commandBusy: 'A command is already running',
    session: 'Session', cancelHelp: 'Esc cancels',
    noCredentials: 'No credential is configured; type /login to set one',
    signIn: 'Sign in to a provider', pasteCredential: 'Paste the credential value',
    noTargets: 'This profile has no sign-in targets', configured: 'Configured',
    notSet: 'Not set', readOnly: 'Read-only', stored: 'Stored; the next request uses it',
    loginCancelled: 'Sign-in cancelled', unknownTarget: 'Unknown sign-in target',
    context: 'Context', listCommands: 'List available commands',
  },
  zh: {
    ready: '就绪', working: '处理中', stopping: '正在停止', command: '命令',
    pending: '待处理输入', nextStep: '下一步', nextTurn: '下一轮',
    help: 'Enter 发送 · Esc 中断 · 按两次 Ctrl-C 退出',
    steering: 'Enter 将输入发送到下一步', quit: '再次按 Ctrl-C 退出',
    approval: '需要批准', approve: 'Y 允许一次 · N 拒绝',
    questions: '等待回答', questionHelp: '输入选项编号或文字回答',
    multiHelp: '输入以逗号分隔的选项编号，或文字回答',
    cancelled: '已中断', compacted: '上下文已压缩',
    unknownCommand: '未知命令', commandBusy: '已有命令正在运行',
    session: '会话', cancelHelp: 'Esc 取消',
    noCredentials: '尚未配置凭据；输入 /login 设置',
    signIn: '登录提供方', pasteCredential: '粘贴凭据值',
    noTargets: '此配置没有登录目标', configured: '已配置',
    notSet: '未设置', readOnly: '只读', stored: '已保存；下次请求将使用它',
    loginCancelled: '已取消登录', unknownTarget: '未知登录目标',
    context: '上下文', listCommands: '列出可用命令',
  },
} as const

/** Supported terminal locales. */
export type Locale = keyof typeof dictionaries
/** Labels supplied to presentation components by the application. */
export type TuiCopy = { readonly [K in keyof typeof dictionaries.en]: string }
