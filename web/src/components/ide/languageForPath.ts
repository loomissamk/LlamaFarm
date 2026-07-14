const EXTENSION_LANGUAGE_MAP: Record<string, string> = {
  ts: 'typescript',
  tsx: 'typescript',
  js: 'javascript',
  jsx: 'javascript',
  mjs: 'javascript',
  cjs: 'javascript',
  json: 'json',
  rs: 'rust',
  py: 'python',
  go: 'go',
  rb: 'ruby',
  java: 'java',
  c: 'c',
  h: 'c',
  cpp: 'cpp',
  cc: 'cpp',
  hpp: 'cpp',
  cs: 'csharp',
  php: 'php',
  sh: 'shell',
  bash: 'shell',
  zsh: 'shell',
  yml: 'yaml',
  yaml: 'yaml',
  toml: 'ini',
  md: 'markdown',
  mdx: 'markdown',
  html: 'html',
  htm: 'html',
  css: 'css',
  scss: 'scss',
  sql: 'sql',
  xml: 'xml',
  dockerfile: 'dockerfile',
  lua: 'lua',
  swift: 'swift',
  kt: 'kotlin',
  vue: 'html',
};

export function languageForPath(path: string): string {
  const name = path.split('/').pop() ?? path;
  if (name.toLowerCase() === 'dockerfile') return 'dockerfile';
  const ext = name.includes('.') ? name.split('.').pop()?.toLowerCase() : undefined;
  if (!ext) return 'plaintext';
  return EXTENSION_LANGUAGE_MAP[ext] ?? 'plaintext';
}
