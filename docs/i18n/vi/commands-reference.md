# Tham khảo lệnh LlamaFarm

Dựa trên CLI hiện tại (`llamafarm --help`).

Xác minh lần cuối: **2026-07-30**.

## Lệnh cấp cao nhất

| Lệnh | Mục đích |
|---|---|
| `onboard` | Khởi tạo workspace/config nhanh hoặc tương tác |
| `agent` | Chạy chat tương tác hoặc chế độ gửi tin nhắn đơn |
| `gateway` | Khởi động gateway webhook và HTTP WhatsApp |
| `daemon` | Khởi động runtime có giám sát (gateway + channels + heartbeat/scheduler tùy chọn) |
| `service` | Quản lý vòng đời dịch vụ cấp hệ điều hành |
| `doctor` | Chạy chẩn đoán và kiểm tra trạng thái |
| `status` | Hiển thị cấu hình và tóm tắt hệ thống |
| `cron` | Quản lý tác vụ định kỳ |
| `models` | Làm mới danh mục model của provider |
| `providers` | Liệt kê ID provider, bí danh và provider đang dùng |
| `channel` | Quản lý kênh và kiểm tra sức khỏe kênh |
| `integrations` | Kiểm tra chi tiết tích hợp |
| `skills` | Liệt kê/cài đặt/gỡ bỏ skills |
| `migrate` | Nhập dữ liệu từ runtime khác (hiện hỗ trợ OpenClaw) |
| `config` | Xuất schema cấu hình dạng máy đọc được |
| `completions` | Tạo script tự hoàn thành cho shell ra stdout |
| `hardware` | Phát hiện và kiểm tra phần cứng USB |
| `peripheral` | Cấu hình và nạp firmware thiết bị ngoại vi |

## Nhóm lệnh

### `onboard`

- `llamafarm onboard`
- `llamafarm onboard --interactive`
- `llamafarm onboard --channels-only`
- `llamafarm onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`
- `llamafarm onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none>`

### `agent`

- `llamafarm agent`
- `llamafarm agent -m "Hello"`
- `llamafarm agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`
- `llamafarm agent --peripheral <board:path>`

### `gateway` / `daemon`

- `llamafarm gateway [--host <HOST>] [--port <PORT>]`
- `llamafarm daemon [--host <HOST>] [--port <PORT>]`

Pairing và flag cũ `--new-pairing` đã ngừng dùng. Gateway khởi động trực tiếp
với các thiết lập runtime đã cấu hình.

### `service`

- `llamafarm service install`
- `llamafarm service start`
- `llamafarm service stop`
- `llamafarm service restart`
- `llamafarm service status`
- `llamafarm service uninstall`

### `cron`

- `llamafarm cron list`
- `llamafarm cron add <expr> [--tz <IANA_TZ>] <command>`
- `llamafarm cron add-at <rfc3339_timestamp> <command>`
- `llamafarm cron add-every <every_ms> <command>`
- `llamafarm cron once <delay> <command>`
- `llamafarm cron remove <id>`
- `llamafarm cron pause <id>`
- `llamafarm cron resume <id>`

### `models`

- `llamafarm models refresh`
- `llamafarm models refresh --provider <ID>`
- `llamafarm models refresh --force`

`models refresh` hiện hỗ trợ làm mới danh mục trực tiếp cho các provider: `openrouter`, `openai`, `anthropic`, `groq`, `mistral`, `deepseek`, `xai`, `together-ai`, `gemini`, `ollama`, `astrai`, `venice`, `fireworks`, `cohere`, `moonshot`, `glm`, `zai`, `qwen` và `nvidia`.

### `channel`

- `llamafarm channel list`
- `llamafarm channel start`
- `llamafarm channel doctor`
- `llamafarm channel bind-telegram <IDENTITY>`
- `llamafarm channel add <type> <json>`
- `llamafarm channel remove <name>`

Lệnh trong chat khi runtime đang chạy (Telegram/Discord):

- `/models`
- `/models <provider>`
- `/model`
- `/model <model-id>`

Channel runtime cũng theo dõi `config.toml` và tự động áp dụng thay đổi cho:
- `default_provider`
- `default_model`
- `default_temperature`
- `api_key` / `api_url` (cho provider mặc định)
- `reliability.*` cài đặt retry của provider

`add/remove` hiện chuyển hướng về thiết lập có hướng dẫn / cấu hình thủ công (chưa hỗ trợ đầy đủ mutator khai báo).

### `integrations`

- `llamafarm integrations info <name>`

### `skills`

- `llamafarm skills list`
- `llamafarm skills install <source>`
- `llamafarm skills remove <name>`

`<source>` chấp nhận git remote (`https://...`, `http://...`, `ssh://...` và `git@host:owner/repo.git`) hoặc đường dẫn cục bộ.

Skill manifest (`SKILL.toml`) hỗ trợ `prompts` và `[[tools]]`; cả hai được đưa vào system prompt của agent khi chạy, giúp model có thể tuân theo hướng dẫn skill mà không cần đọc thủ công.

### `migrate`

- `llamafarm migrate openclaw [--source <path>] [--dry-run]`

### `config`

- `llamafarm config schema`

`config schema` xuất JSON Schema (draft 2020-12) cho toàn bộ hợp đồng `config.toml` ra stdout.

### `completions`

- `llamafarm completions bash`
- `llamafarm completions fish`
- `llamafarm completions zsh`
- `llamafarm completions powershell`
- `llamafarm completions elvish`

`completions` chỉ xuất ra stdout để script có thể được source trực tiếp mà không bị lẫn log/cảnh báo.

### `hardware`

- `llamafarm hardware discover`
- `llamafarm hardware introspect <path>`
- `llamafarm hardware info [--chip <chip_name>]`

### `peripheral`

- `llamafarm peripheral list`
- `llamafarm peripheral add <board> <path>`
- `llamafarm peripheral flash [--port <serial_port>]`
- `llamafarm peripheral setup-uno-q [--host <ip_or_host>]`
- `llamafarm peripheral flash-nucleo`

## Kiểm tra nhanh

Để xác minh nhanh tài liệu với binary hiện tại:

```bash
llamafarm --help
llamafarm <command> --help
```
