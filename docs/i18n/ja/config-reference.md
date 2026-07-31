# 設定リファレンス（日本語）

このページは Wave 1 の初版ローカライズです。主要設定キー、既定値、リスク境界を確認します。

英語版原文:

- [../../config-reference.md](../../config-reference.md)

## 主な用途

- 新規環境の初期設定
- 設定衝突や回復手順の確認
- セキュリティ関連設定の監査

## 運用ルール

- 設定キー名は英語のまま保持します。
- 実行時挙動の定義は英語版原文を優先します。

## 更新メモ

- `agent.max_tool_iterations` の既定値は `100000` です。`0` もこの既定値にフォールバックし、反復する進捗停止は専用の停滞検出で処理します。
- `gateway.require_pairing` は既定値 `false` の旧互換フィールドです。pairing は廃止され、実行時にはこの値を無視します。
- `model_routes[].api_url` が追加され、特定の route だけで上位の `api_url` を上書きできます。
- 同じ provider 種別の複数ローカル推論 endpoint に hint ごとで振り分けたい場合に使います。
- `provider.ollama_num_ctx` は厳密な手動コンテキスト上書きです。ダッシュボードでは
  2,048～262,144 を設定でき、未設定時は `OLLAMA_NUM_CTX` が実行時の既定値になります。
- `LLAMAFARM_ADAPTIVE_CONTEXT=true` の場合、環境の既定値は高速な基準値となり、
  リクエストに必要なときだけ 2 倍の段階で拡張されます。上限はモデル固有の長さと
  `LLAMAFARM_ADAPTIVE_CONTEXT_MAX`（既定 262,144）の小さい方です。
