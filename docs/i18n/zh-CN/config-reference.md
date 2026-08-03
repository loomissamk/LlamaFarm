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

- `agent.max_tool_iterations` 的默认值为 `0`（无限制）；任务会运行到完成、真实停滞/错误或操作者明确取消。正整数仍可设置每轮显式上限。
- `research.max_iterations` 的默认值为 `0`（无限制）；有效研究会持续到完成、provider/tool 错误或相同调用/结果停滞检测器触发。正整数设置显式研究轮次上限。
- `host_runner.max_exec_timeout_secs` 的默认值为 `0`（无最大值）；缺省或为零的 `timeout_secs` 不设置墙钟截止时间，正整数仍可设置显式截止时间。
- `scheduler.max_concurrent` 默认为 `4`，表示所有轮询之间同时运行的计划任务总数；`0` 表示不限制并发。可用 `LLAMAFARM_SCHEDULER_MAX_CONCURRENT` 覆盖。
- `gateway.require_pairing` 是默认值为 `false` 的旧版兼容字段；pairing 已停用，运行时忽略该值。
- 新增 `model_routes[].api_url`，可仅为某个路由覆盖顶层 `api_url`。
- 适用于将不同 hint 指向同一种 provider 的多个本地推理端点。
- `provider.ollama_num_ctx` 是精确的手动上下文覆盖值，控制面板支持
  2,048–262,144。未设置时，`OLLAMA_NUM_CTX` 提供运行时默认值。
- 设置 `LLAMAFARM_ADAPTIVE_CONTEXT=true` 后，环境默认值会成为快速基线；
  LlamaFarm 根据请求需要按 2 倍档位增长，最高不超过模型原生上下文和
  `LLAMAFARM_ADAPTIVE_CONTEXT_MAX`（默认 262,144）中的较小值。
- `provider.ollama_workers` 定义绑定到单个 GPU 或 GPU 集合的 Ollama worker；
  `provider.ollama_model_placements` 将指定模型路由到 worker。多个 GPU 且
  `spread = true` 时可拆分模型，独立 worker 可同时驻留不同模型。
- 命名的 `[[db_connections]]` 供 Database Explorer 和
  `db_schema`/`db_query` 工具使用。支持 `sqlite`、`postgres`、`mysql`
  （包括 MariaDB）和 `mongodb` 驱动。MySQL/MariaDB 需要 Cargo feature
  `db-mysql`；bundled Docker image 默认已启用。
