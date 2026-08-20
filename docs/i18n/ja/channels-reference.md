# チャネルリファレンス（日本語）

このページは Wave 1 の初版ローカライズです。チャネル機能と設定経路の確認用です。

英語版原文:

- [../../channels-reference.md](../../channels-reference.md)

## 主な用途

- Telegram/Discord/Slack などの機能差分を確認する
- allowlist と安全境界を確認する
- メッセージ送受信トラブルを切り分ける

## 運用ルール

- チャネル識別子、設定キー、API パスは英語のまま保持します。
- 仕様の最終判断は英語版原文に従います。

## ダッシュボードからの Discord 設定

**Connections -> Discord -> Connect Discord** を開き、bot token、任意の guild ID、
許可する数値 user ID を 1 つ以上入力します。LlamaFarm は bot の ID を検証して表示し、
secret をブラウザへ返さずに `[channels_config.discord]` を保存します。**Add to server**
から Discord 公式 OAuth インストール画面を開けます。その後は GitHub connection と同様に
Update または Disconnect できます。Discord Developer Portal で **Message Content Intent**
を有効にし、listener の開始、更新、停止のため node を一度再起動してください。
