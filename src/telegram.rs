//! Telegram Bot API client — minimal surface for forwarding and reply webhooks.
//!
//! This wraps the subset of the [Bot API](https://core.telegram.org/bots/api)
//! needed for a typical relay worker:
//!
//! - [`TelegramBot::send_message`] — post a message to a chat
//! - [`TelegramBot::set_webhook`] — register a webhook URL (one-time setup)
//! - [`parse_update`] — parse the JSON body of a webhook POST into an [`Update`]
//!
//! Only the fields a relay needs are modeled. Unknown fields are ignored
//! by `serde` so future additions don't break the crate.

use serde::{Deserialize, Serialize};
use worker::*;

use crate::multipart::MultipartBuilder;

const API_BASE: &str = "https://api.telegram.org";

/// A Telegram Bot API client, scoped to a single bot token.
#[derive(Clone, Debug)]
pub struct TelegramBot {
    token: String,
}

impl TelegramBot {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }

    fn method_url(&self, method: &str) -> String {
        format!("{API_BASE}/bot{}/{}", self.token, method)
    }

    /// Send a message to a chat and return the created [`Message`].
    pub async fn send_message(&self, params: SendMessage) -> Result<Message> {
        let body = serde_json::to_string(&params)
            .map_err(|e| Error::from(format!("telegram: encode sendMessage: {e}")))?;
        let resp = self.call_api(&self.method_url("sendMessage"), body).await?;
        extract_ok(resp)
    }

    /// Upload a photo to a chat. The photo bytes are sent as the `photo`
    /// multipart part; everything else goes through as form fields. Returns
    /// the created [`Message`] on success.
    pub async fn send_photo(&self, params: SendPhoto) -> Result<Message> {
        let SendPhoto {
            chat_id,
            photo,
            photo_filename,
            photo_content_type,
            caption,
            parse_mode,
            disable_notification,
            reply_to_message_id,
        } = params;

        let mut mp = MultipartBuilder::new();
        mp.add_text("chat_id", &chat_id);
        if let Some(c) = caption.as_deref() {
            mp.add_text("caption", c);
        }
        if let Some(pm) = parse_mode {
            mp.add_text("parse_mode", parse_mode_wire(pm));
        }
        if let Some(d) = disable_notification {
            mp.add_text("disable_notification", if d { "true" } else { "false" });
        }
        if let Some(id) = reply_to_message_id {
            mp.add_text("reply_to_message_id", &id.to_string());
        }
        let filename = if photo_filename.is_empty() {
            "photo.png"
        } else {
            &photo_filename
        };
        let content_type = if photo_content_type.is_empty() {
            "image/png"
        } else {
            &photo_content_type
        };
        mp.add_file("photo", filename, content_type, &photo);

        let resp = self
            .call_api_multipart(&self.method_url("sendPhoto"), mp)
            .await?;
        extract_ok(resp)
    }

    /// Register a webhook URL with Telegram so it posts [`Update`]s to you.
    ///
    /// Pass `secret_token` to have Telegram include an
    /// `X-Telegram-Bot-Api-Secret-Token` header on every request — verify it
    /// in your webhook handler to reject spoofed calls.
    pub async fn set_webhook(&self, url: &str, secret_token: Option<&str>) -> Result<()> {
        let mut body = serde_json::json!({ "url": url });
        if let Some(tok) = secret_token {
            body["secret_token"] = serde_json::json!(tok);
        }
        let resp = self
            .call_api(&self.method_url("setWebhook"), body.to_string())
            .await?;
        let _: serde_json::Value = extract_ok(resp)?;
        Ok(())
    }

    async fn call_api(&self, url: &str, body: String) -> Result<ApiEnvelope<serde_json::Value>> {
        let headers = Headers::new();
        headers.set("Content-Type", "application/json")?;
        let req = Request::new_with_init(
            url,
            RequestInit::new()
                .with_method(Method::Post)
                .with_headers(headers)
                .with_body(Some(body.into())),
        )?;
        let mut resp = Fetch::Request(req).send().await?;
        resp.json()
            .await
            .map_err(|e| Error::from(format!("telegram: decode response: {e}")))
    }

    async fn call_api_multipart(
        &self,
        url: &str,
        mp: MultipartBuilder,
    ) -> Result<ApiEnvelope<serde_json::Value>> {
        let headers = Headers::new();
        headers.set("Content-Type", &mp.content_type())?;
        let bytes = mp.finish();
        let body = js_sys::Uint8Array::from(bytes.as_slice()).buffer();
        let req = Request::new_with_init(
            url,
            RequestInit::new()
                .with_method(Method::Post)
                .with_headers(headers)
                .with_body(Some(body.into())),
        )?;
        let mut resp = Fetch::Request(req).send().await?;
        resp.json()
            .await
            .map_err(|e| Error::from(format!("telegram: decode response: {e}")))
    }
}

fn parse_mode_wire(mode: ParseMode) -> &'static str {
    match mode {
        ParseMode::Html => "HTML",
        ParseMode::MarkdownV2 => "MarkdownV2",
    }
}

/// Parse a webhook body into an [`Update`].
pub fn parse_update(body: &[u8]) -> Result<Update> {
    serde_json::from_slice(body)
        .map_err(|e| Error::from(format!("telegram: decode update: {e}")))
}

/// Parameters for `sendMessage`. Fill `chat_id` and `text` at minimum.
#[derive(Serialize, Default, Clone, Debug)]
pub struct SendMessage {
    pub chat_id: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parse_mode: Option<ParseMode>,
    #[serde(
        rename = "disable_web_page_preview",
        skip_serializing_if = "Option::is_none"
    )]
    pub disable_preview: Option<bool>,
    #[serde(rename = "reply_to_message_id", skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum ParseMode {
    #[serde(rename = "HTML")]
    Html,
    MarkdownV2,
}

/// Parameters for `sendPhoto`. `photo` is the raw image bytes; everything
/// else is optional. `photo_filename` and `photo_content_type` populate the
/// multipart file part (defaults: `photo.png`, `image/png`).
#[derive(Default, Clone, Debug)]
pub struct SendPhoto {
    pub chat_id: String,
    pub photo: Vec<u8>,
    pub photo_filename: String,
    pub photo_content_type: String,
    pub caption: Option<String>,
    pub parse_mode: Option<ParseMode>,
    pub disable_notification: Option<bool>,
    pub reply_to_message_id: Option<i64>,
}

/// Envelope around every Bot API response — we unwrap `result` via
/// [`extract_ok`].
#[derive(Deserialize, Debug)]
struct ApiEnvelope<T> {
    ok: bool,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    result: Option<T>,
}

fn extract_ok<T: serde::de::DeserializeOwned>(env: ApiEnvelope<serde_json::Value>) -> Result<T> {
    if !env.ok {
        return Err(Error::from(format!(
            "telegram API error: {}",
            env.description.as_deref().unwrap_or("unknown")
        )));
    }
    let value = env
        .result
        .ok_or_else(|| Error::from("telegram API: missing result"))?;
    serde_json::from_value(value)
        .map_err(|e| Error::from(format!("telegram: decode result: {e}")))
}

// ----- Update / Message types -----
//
// Only fields we consume are modeled. `serde(default)` + field-level
// `#[serde(default)]` keep forward-compat with new fields.

/// A single incoming update (webhook body).
#[derive(Deserialize, Clone, Debug)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
    pub edited_message: Option<Message>,
}

impl Update {
    /// Return the primary message, whether new or edited.
    pub fn any_message(&self) -> Option<&Message> {
        self.message.as_ref().or(self.edited_message.as_ref())
    }
}

/// A Telegram message. `reply_to_message` is what we key replies on.
#[derive(Deserialize, Clone, Debug)]
pub struct Message {
    pub message_id: i64,
    pub chat: Chat,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub reply_to_message: Option<Box<Message>>,
    #[serde(default)]
    pub from: Option<User>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Chat {
    pub id: i64,
    #[serde(rename = "type")]
    pub chat_type: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct User {
    pub id: i64,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reply_update() {
        let body = br#"{
            "update_id": 1,
            "message": {
                "message_id": 42,
                "chat": {"id": -10042, "type": "group", "title": "Inbox"},
                "text": "Thanks!",
                "from": {"id": 1, "first_name": "A"},
                "reply_to_message": {
                    "message_id": 17,
                    "chat": {"id": -10042, "type": "group"},
                    "text": "(forwarded) Subject: Order"
                }
            }
        }"#;
        let update = parse_update(body).unwrap();
        let msg = update.any_message().expect("message");
        assert_eq!(msg.text.as_deref(), Some("Thanks!"));
        let replied = msg.reply_to_message.as_ref().expect("reply");
        assert_eq!(replied.message_id, 17);
        assert_eq!(replied.chat.id, -10042);
    }

    #[test]
    fn parse_update_ignores_unknown_fields() {
        let body = br#"{
            "update_id": 2,
            "message": {
                "message_id": 1,
                "chat": {"id": 1, "type": "private"},
                "forwarded_from_chat": {"id": 99, "type": "channel"},
                "newtype_tomorrow": [1,2,3]
            }
        }"#;
        let update = parse_update(body).unwrap();
        assert_eq!(update.update_id, 2);
        assert_eq!(update.any_message().unwrap().chat.id, 1);
    }

    #[test]
    fn parse_edited_message() {
        let body = br#"{
            "update_id": 3,
            "edited_message": {
                "message_id": 1,
                "chat": {"id": 1, "type": "private"},
                "text": "edit"
            }
        }"#;
        let update = parse_update(body).unwrap();
        assert!(update.message.is_none());
        assert_eq!(update.any_message().unwrap().text.as_deref(), Some("edit"));
    }

    #[test]
    fn api_envelope_error() {
        let env = ApiEnvelope::<serde_json::Value> {
            ok: false,
            description: Some("Bad Request".into()),
            result: None,
        };
        let err = extract_ok::<serde_json::Value>(env).unwrap_err();
        let s = err.to_string();
        assert!(s.contains("Bad Request"), "unexpected: {s}");
    }

    #[test]
    fn send_message_serializes_minimal() {
        let sm = SendMessage {
            chat_id: "-1001".into(),
            text: "hello".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&sm).unwrap();
        assert!(json.contains(r#""chat_id":"-1001""#));
        assert!(json.contains(r#""text":"hello""#));
        assert!(!json.contains("parse_mode"));
        assert!(!json.contains("reply_to_message_id"));
    }

    #[test]
    fn send_message_serializes_with_options() {
        let sm = SendMessage {
            chat_id: "42".into(),
            text: "body".into(),
            parse_mode: Some(ParseMode::Html),
            disable_preview: Some(true),
            reply_to_message_id: Some(100),
        };
        let json = serde_json::to_string(&sm).unwrap();
        assert!(json.contains(r#""parse_mode":"HTML""#));
        assert!(json.contains(r#""disable_web_page_preview":true"#));
        assert!(json.contains(r#""reply_to_message_id":100"#));
    }
}
