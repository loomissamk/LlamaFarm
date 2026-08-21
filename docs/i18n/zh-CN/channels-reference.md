# Channel 参考（简体中文）

这是 Wave 1 首版本地化页面，用于查阅各通信渠道能力与配置路径。

英文原文：

- [../../channels-reference.md](../../channels-reference.md)

## 适用场景

- 了解 Telegram/Discord/Slack 等渠道能力差异
- 确认 allowlist、安全边界与接入前置条件
- 排查渠道消息收发问题

## 使用建议

- 通道标识、配置键与 API 路径保持英文。
- 具体行为定义以英文原文为准。

## 在 dashboard 中配置 Discord

打开 **Connections -> Discord -> Connect Discord**，填写 bot token、可选 guild ID，
以及至少一个允许访问的数字 user ID。LlamaFarm 会验证并显示配对的 bot 身份，且在
不把 secret 返回浏览器的情况下保存 `[channels_config.discord]`。使用 **Add to server**
进入 Discord 官方 OAuth 安装页面；之后可像 GitHub connection 一样 Update 或 Disconnect。
请在 Discord Developer Portal 中启用 **Message Content Intent**，然后重启 node 一次以
启动、更新或停止长期 listener。
