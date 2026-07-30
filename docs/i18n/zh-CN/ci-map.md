# 本地化桥接文档：CI Workflow Map

这是英文 CI 规范的增强桥接页。当前仓库只有两个可执行 GitHub Actions
工作流：`ci-run.yml` 与 `docs-deploy.yml`。

英文原文：

- [../../ci-map.md](../../ci-map.md)

## 主题定位

- 类别：工程流程与交付
- 深度：增强桥接（章节导览 + 执行提示）
- 适用：先理解结构，再按英文规范逐条执行

## 原文章节导览

- [Executable Workflow Baseline](../../ci-map.md#executable-workflow-baseline)
- [Core CI Contract](../../ci-map.md#core-ci-contract)
- [Docs Pages Contract](../../ci-map.md#docs-pages-contract)
- [Local Reproduction](../../ci-map.md#local-reproduction)
- [Fast Triage](../../ci-map.md#fast-triage)
- [Maintenance Rules](../../ci-map.md#maintenance-rules)

## 操作建议

- 合并门禁的稳定检查名是 `CI Required Gate`。
- 文档 PR 只构建站点；仅 `main` 分支部署 GitHub Pages。
- 命令名、配置键、API 路径和代码标识保持英文。
- 发生语义歧义或行为冲突时，以英文原文为准。

## 相关入口

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
