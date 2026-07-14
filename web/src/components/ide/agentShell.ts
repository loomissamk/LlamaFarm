export const AGENT_SHELL_TOOLS = new Set(['shell', 'process', 'git_operations']);

function asRecord(args: unknown): Record<string, unknown> {
  return args && typeof args === 'object' ? (args as Record<string, unknown>) : {};
}

export function formatShellCommandLine(toolName: string, args: unknown): string {
  const record = asRecord(args);

  if (toolName === 'shell' && typeof record.command === 'string') {
    return record.command;
  }

  if (toolName === 'process') {
    const action = typeof record.action === 'string' ? record.action : 'process';
    const command = typeof record.command === 'string' ? ` ${record.command}` : '';
    const id = typeof record.id === 'string' ? ` #${record.id}` : '';
    return `process ${action}${command}${id}`;
  }

  if (toolName === 'git_operations' && typeof record.operation === 'string') {
    const message = typeof record.message === 'string' ? ` -m "${record.message}"` : '';
    return `git ${record.operation}${message}`;
  }

  return `${toolName}(${JSON.stringify(record)})`;
}
