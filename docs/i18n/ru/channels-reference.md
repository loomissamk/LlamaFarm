# Справочник каналов (Русский)

Это первичная локализация Wave 1 для обзора возможностей каналов и путей настройки.

Оригинал на английском:

- [../../channels-reference.md](../../channels-reference.md)

## Когда использовать

- Сравнение возможностей Telegram/Discord/Slack и других каналов
- Проверка allowlist и границ безопасности
- Разбор проблем доставки/приема сообщений

## Правило

- Идентификаторы каналов, API-пути и config keys остаются на английском.
- Источник истины по поведению — английский оригинал.

## Настройка Discord в dashboard

Откройте **Connections -> Discord -> Connect Discord** и укажите bot token,
необязательный guild ID и хотя бы один разрешённый числовой user ID. LlamaFarm
проверяет и показывает привязанного bot, сохраняя `[channels_config.discord]`
без возврата секрета в браузер. **Add to server** открывает официальный экран
установки Discord OAuth; затем доступны Update и Disconnect, как для GitHub.
Включите **Message Content Intent** в Discord Developer Portal и один раз
перезапустите node, чтобы запустить, обновить или остановить listener.
