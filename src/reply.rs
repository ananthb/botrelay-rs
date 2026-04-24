//! Reply-routing context.
//!
//! When a Worker forwards content to a chat, it typically needs to remember
//! *who/where* it came from so a reply in the chat can be sent back. This
//! module defines a small serializable struct for that — the consumer is
//! responsible for its own storage (KV, D1, etc).

use serde::{Deserialize, Serialize};

/// Context stored per forwarded message. The consumer decides the key
/// (e.g. `tg:<chat_id>:<message_id>` for Telegram) and TTL.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ReplyContext {
    /// Address the original content was received on (e.g. the alias
    /// `shop.swizzles@kedi.dev`).
    pub alias: String,
    /// The original external sender to route a reply back to.
    pub original_sender: String,
    /// Subject line (or short label) — useful when a reply is composed in
    /// a chat modal and you want to echo the original context.
    pub subject: String,
}

impl ReplyContext {
    /// Suggested KV key prefix for Telegram contexts:
    /// `tg:<chat_id>:<message_id>`.
    pub fn telegram_key(chat_id: &str, message_id: i64) -> String {
        format!("tg:{chat_id}:{message_id}")
    }

    /// Suggested KV key prefix for Discord contexts:
    /// `dc:<channel_id>:<message_id>`.
    pub fn discord_key(channel_id: &str, message_id: &str) -> String {
        format!("dc:{channel_id}:{message_id}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_format() {
        assert_eq!(ReplyContext::telegram_key("-100123", 42), "tg:-100123:42");
        assert_eq!(
            ReplyContext::discord_key("987654321", "msg_abc"),
            "dc:987654321:msg_abc"
        );
    }

    #[test]
    fn roundtrip_json() {
        let ctx = ReplyContext {
            alias: "shop@kedi.dev".into(),
            original_sender: "noreply@swiggy.in".into(),
            subject: "Your order".into(),
        };
        let json = serde_json::to_string(&ctx).unwrap();
        let back: ReplyContext = serde_json::from_str(&json).unwrap();
        assert_eq!(back.alias, ctx.alias);
        assert_eq!(back.original_sender, ctx.original_sender);
        assert_eq!(back.subject, ctx.subject);
    }
}
