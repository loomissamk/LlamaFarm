# 配置参考（简体中文）

这是 Wave 1 首版本地化页面，用于查阅核心配置键、默认值与风险边界。

英文原文：

- [../../config-reference.md](../../config-reference.md)

## 适用场景

- 新环境初始化配置
- 排查配置项冲突与回退策略
- 审核安全相关配置与默认值

## 使用建议

- 配置键保持英文，避免本地化改写键名。
- 生产行为以英文原文定义为准。

## 更新说明

- `agent.max_tool_iterations` 的默认值为 `100000`，值 `0` 也回退到该默认值；运行时使用专用停滞检测器识别重复的无进展。
- `gateway.require_pairing` 是默认值为 `false` 的旧版兼容字段；pairing 已停用，运行时忽略该值。
- 新增 `model_routes[].api_url`，可仅为某个路由覆盖顶层 `api_url`。
- 适用于将不同 hint 指向同一种 provider 的多个本地推理端点。
- `provider.ollama_num_ctx` 是精确的手动上下文覆盖值，控制面板支持
  2,048–262,144。未设置时，`OLLAMA_NUM_CTX` 提供运行时默认值。
- 设置 `LLAMAFARM_ADAPTIVE_CONTEXT=true` 后，环境默认值会成为快速基线；
  LlamaFarm 根据请求需要按 2 倍档位增长，最高不超过模型原生上下文和
  `LLAMAFARM_ADAPTIVE_CONTEXT_MAX`（默认 262,144）中的较小值。
