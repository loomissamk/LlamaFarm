# Provider 参考（简体中文）

这是 Wave 1 首版本地化页面，用于快速查阅 provider 标识、别名与认证变量。

英文原文：

- [../../providers-reference.md](../../providers-reference.md)

## 适用场景

- 选择 provider 与模型接入路径
- 核对 provider ID / alias / 环境变量
- 处理 provider 配置错误与鉴权问题

## 使用建议

- Provider ID 与环境变量名称保持英文。
- 规范与行为说明以英文原文为准。

## Ollama 自适应上下文

- `provider.ollama_num_ctx` 是精确的手动覆盖值。
- 未设置时，Auto 使用所选模型报告的原生最大值。
- 只有启用 `LLAMAFARM_ADAPTIVE_CONTEXT=true` 时，`OLLAMA_NUM_CTX` 才会成为
  快速基线；它不会限制普通 Auto。增长上限是模型原生长度与
  `LLAMAFARM_ADAPTIVE_CONTEXT_MAX` 中的较小值。
- 使用 `docker exec LlamaFarm ollama ps` 核实 Ollama 实际加载的上下文。
