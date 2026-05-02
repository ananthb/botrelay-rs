# botrelay

Reusable primitives for Cloudflare Workers that need to:

1. Forward content (an email, a notification, anything) to a **Telegram** chat or **Discord** channel via a bot.
2. Route replies from those channels back to the original source (typically an email address).

The crate provides:

- `botrelay::telegram` — a minimal Telegram Bot API client (`sendMessage`, `setWebhook`) and typed `Update` parsing for the webhook endpoint.
- `botrelay::discord` — a minimal Discord Bot API client (`createMessage` with components), Ed25519 interaction-signature verification (WebCrypto), and typed `Interaction` parsing including modal submissions.
- `botrelay::reply` — a small `ReplyContext` type (alias, original sender, subject) you store per forwarded message so the webhook handler can look it up and send a reply back through whatever channel the original content came from.

It doesn't own storage or the send-reply side — each consumer plugs in its own KV and outbound mechanism. That's the whole point: both [cutout](https://github.com/ananthb/cutout) (email proxy) and [concierge-worker](https://github.com/ananthb/concierge-worker) (business messaging) embed the same primitives with different backends.

## Usage

```toml
[dependencies]
botrelay = "0.1"
```

```rust
use botrelay::telegram::{TelegramBot, SendMessage};

let bot = TelegramBot::new(std::env::var("TELEGRAM_BOT_TOKEN").unwrap());
let msg = bot.send_message(SendMessage {
    chat_id: "123456".into(),
    text: "Hello!".into(),
    ..Default::default()
}).await?;
```

See the crate docs for Discord and reply-routing.

## License

[AGPL-3.0](LICENSE)
