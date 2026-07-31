# Provider リファレンス（日本語）

このページは Wave 1 の初版ローカライズです。provider ID、別名、認証環境変数の確認に使います。

英語版原文:

- [../../providers-reference.md](../../providers-reference.md)

## 主な用途

- provider/モデル接続先を選定する
- provider ID・alias・認証変数を確認する
- provider 設定ミスや認証エラーを切り分ける

## 運用ルール

- Provider ID と環境変数名は英語のまま保持します。
- 正式な仕様は英語版原文を優先します。

## Ollama の適応型コンテキスト

- `provider.ollama_num_ctx` は厳密な手動上書きです。
- 未設定時は `OLLAMA_NUM_CTX` が環境の既定値になり、
  `LLAMAFARM_ADAPTIVE_CONTEXT=true` では高速な基準値として扱われます。
- RTX 5070 Ti プロファイルは 65,536 から始まり、リクエストの見積りが
  必要とする場合だけ 131,072 または 262,144 を選択します。上限はモデル固有の
  長さと `LLAMAFARM_ADAPTIVE_CONTEXT_MAX` の小さい方です。
- Ollama が実際に確保したコンテキストは
  `docker exec LlamaFarm ollama ps` で確認します。
