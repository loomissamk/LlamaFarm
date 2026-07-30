# ローカライズブリッジ: CI Workflow Map

このページは英語版 CI 仕様への強化ブリッジです。現在の実行可能な
GitHub Actions は `ci-run.yml` と `docs-deploy.yml` の 2 つです。

英語版原文:

- [../../ci-map.md](../../ci-map.md)

## テーマ位置付け

- 分類: エンジニアリング運用とデリバリー
- 深度: 強化ブリッジ（セクション導線 + 実行ヒント）
- 使い方: 構成を把握してから英語版の規範記述に従う

## 原文セクションガイド

- [Executable Workflow Baseline](../../ci-map.md#executable-workflow-baseline)
- [Core CI Contract](../../ci-map.md#core-ci-contract)
- [Docs Pages Contract](../../ci-map.md#docs-pages-contract)
- [Local Reproduction](../../ci-map.md#local-reproduction)
- [Fast Triage](../../ci-map.md#fast-triage)
- [Maintenance Rules](../../ci-map.md#maintenance-rules)

## 実行ヒント

- マージゲートの安定したチェック名は `CI Required Gate` です。
- ドキュメント PR はビルドのみを行い、GitHub Pages へのデプロイは
  `main` だけで実行されます。
- コマンド名、設定キー、API パス、コード識別子は英語のまま保持します。
- 解釈に差分がある場合は英語版原文を優先します。

## 関連エントリ

- [README.md](README.md)
- [SUMMARY.md](SUMMARY.md)
- [docs-inventory.md](docs-inventory.md)
