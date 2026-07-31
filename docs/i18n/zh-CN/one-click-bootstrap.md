# 本地化桥接文档：One Click Bootstrap

这是增强型 bridge 页面。它提供该主题的定位、原文章节导览和执行提示，帮助你在不丢失英文规范语义的情况下快速落地。

英文原文:

- [../../one-click-bootstrap.md](../../one-click-bootstrap.md)

## 主题定位

- 类别：运行与接入
- 深度：增强 bridge（章节导览 + 执行提示）
- 适用：先理解结构，再按英文规范逐条执行。

## 原文章节导览

- [H2 · Option 0: Homebrew (macOS/Linuxbrew)](../../one-click-bootstrap.md#option-0-homebrew-macos-linuxbrew)
- [H2 · Option A (Recommended): Clone + local script](../../one-click-bootstrap.md#option-a-recommended-clone-local-script)
- [H3 · Resource preflight and pre-built flow](../../one-click-bootstrap.md#resource-preflight-and-pre-built-flow)
- [H2 · Dual-mode bootstrap](../../one-click-bootstrap.md#dual-mode-bootstrap)
- [H2 · Option B: Remote one-liner](../../one-click-bootstrap.md#option-b-remote-one-liner)
- [H2 · Optional onboarding modes](../../one-click-bootstrap.md#optional-onboarding-modes)
- [H3 · Containerized onboarding (Docker)](../../one-click-bootstrap.md#containerized-onboarding-docker)
- [H3 · Quick onboarding (non-interactive)](../../one-click-bootstrap.md#quick-onboarding-non-interactive)
- [H3 · Interactive onboarding](../../one-click-bootstrap.md#interactive-onboarding)
- [H2 · Useful flags](../../one-click-bootstrap.md#useful-flags)
- [H2 · Related docs](../../one-click-bootstrap.md#related-docs)

## 操作建议

- 先通读原文目录，再聚焦与你当前变更直接相关的小节。
- 命令名、配置键、API 路径和代码标识保持英文。
- 发生语义歧义或行为冲突时，以英文原文为准。

## 捆绑运行时更新

`./scripts/docker/up-bundle.sh` 构建带 `rag-pdf` 的 PDF 读取器，启动
Xvfb 虚拟显示并用真实 PNG 截图检查其就绪状态。状态与 federation API
还会报告启动脚本注入的源码 commit 和 UTC 构建时间。模型仅在
`OLLAMA_PULL_MODELS` 中明确列出时拉取。

## 相关入口

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
