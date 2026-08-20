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

- `agent.max_tool_iterations` の既定値は `0`（無制限）です。完了、実際の停止/エラー、またはオペレーターによる明示的なキャンセルまで実行されます。正の整数ではターンごとの上限を明示できます。
- `agent.max_output_tokens_per_turn` の既定値は `16384` で、推論/出力の1セグメントだけを制限します。到達時は同じタスクをチェックポイントから継続します。
- `agent.max_no_progress_spins` の既定値は `6` です。空の推論セグメントまたは重複 tool call が連続した場合だけ停止し、実際の進捗があれば直ちにリセットされます。
- `research.max_iterations` の既定値も `0`（無制限）です。有効な調査は、完了、provider/tool エラー、または同一の呼び出し/結果を検出する停滞ガードまで継続します。正の整数で明示的な調査上限を設定できます。
- `host_runner.max_exec_timeout_secs` の既定値は `0`（上限なし）です。省略またはゼロの `timeout_secs` は実時間の期限を設けず、正の整数で明示的な期限を設定できます。
- `scheduler.max_concurrent` の既定値は `4` で、すべてのポーリングにまたがる同時実行ジョブ数を制限します。`0` は無制限で、`LLAMAFARM_SCHEDULER_MAX_CONCURRENT` で上書きできます。
- `gateway.require_pairing` は既定値 `false` の旧互換フィールドです。pairing は廃止され、実行時にはこの値を無視します。
- `model_routes[].api_url` が追加され、特定の route だけで上位の `api_url` を上書きできます。
- 同じ provider 種別の複数ローカル推論 endpoint に hint ごとで振り分けたい場合に使います。
- `provider.ollama_num_ctx` は厳密な手動コンテキスト上書きです。ダッシュボードでは
  2,048～262,144 を設定でき、未設定時の Auto はモデル固有の最大値を使います。
- `LLAMAFARM_ADAPTIVE_CONTEXT=true` の場合だけ `OLLAMA_NUM_CTX` は高速な基準値となり、
  リクエストに必要なときだけ 2 倍の段階で拡張されます。上限はモデル固有の長さと
  `LLAMAFARM_ADAPTIVE_CONTEXT_MAX`（既定 262,144）の小さい方です。
- `provider.ollama_workers` は単一 GPU または GPU セットに固定した Ollama worker を定義し、
  `provider.ollama_model_placements` はモデルを worker にルーティングします。複数 GPU で
  `spread = true` にするとモデルを分散でき、別 worker なら複数モデルを同時常駐できます。
- 名前付き `[[db_connections]]` は Database Explorer と
  `db_schema`/`db_query` ツールで使用されます。対応 driver は `sqlite`、
  `postgres`、`mysql`（MariaDB を含む）、`mongodb` です。MySQL/MariaDB には
  Cargo feature `db-mysql` が必要で、bundled Docker image では既定で有効です。
