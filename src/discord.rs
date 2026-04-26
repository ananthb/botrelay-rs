//! Discord Bot API + Interactions client.
//!
//! Scope:
//! - [`DiscordBot::create_message`] — post a bot message to a channel, with
//!   optional action-row components (a "Reply" button is the typical pattern).
//! - [`DiscordBot::verify_interaction`] — verify the Ed25519 signature on
//!   an incoming interaction using WebCrypto (Cloudflare Workers).
//! - [`DiscordBot::respond_interaction`] — respond to an interaction
//!   (open a modal, post a message, acknowledge a component click).
//! - [`DiscordBot::followup_interaction`] — post a follow-up message using
//!   the interaction token after an async reply.
//!
//! Only the fields a relay needs are modeled; unknown fields are ignored.

use serde::{Deserialize, Serialize};
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use worker::*;

use crate::multipart::MultipartBuilder;

const API_BASE: &str = "https://discord.com/api/v10";

/// A Discord bot client scoped to one application / bot token.
#[derive(Clone, Debug)]
pub struct DiscordBot {
    token: String,
    application_id: String,
    public_key_hex: String,
}

impl DiscordBot {
    pub fn new(
        token: impl Into<String>,
        application_id: impl Into<String>,
        public_key_hex: impl Into<String>,
    ) -> Self {
        Self {
            token: token.into(),
            application_id: application_id.into(),
            public_key_hex: public_key_hex.into(),
        }
    }

    pub fn application_id(&self) -> &str {
        &self.application_id
    }

    /// Post a bot message to a channel.
    pub async fn create_message(
        &self,
        channel_id: &str,
        params: CreateMessage,
    ) -> Result<Message> {
        let url = format!("{API_BASE}/channels/{channel_id}/messages");
        let body = serde_json::to_string(&params)
            .map_err(|e| Error::from(format!("discord: encode createMessage: {e}")))?;
        let resp = self.call_api(Method::Post, &url, Some(body)).await?;
        decode(resp)
    }

    /// Post a bot message with one or more file attachments. The
    /// [`CreateMessage`] is sent as the multipart `payload_json` field; each
    /// [`Attachment`] becomes a `files[N]` part. Discord auto-attaches files
    /// when the `attachments` array is omitted from `payload_json`.
    pub async fn create_message_with_attachments(
        &self,
        channel_id: &str,
        params: CreateMessage,
        attachments: &[Attachment],
    ) -> Result<Message> {
        let url = format!("{API_BASE}/channels/{channel_id}/messages");
        let mut mp = MultipartBuilder::new();
        mp.add_json("payload_json", &params)?;
        for (i, att) in attachments.iter().enumerate() {
            let field = format!("files[{i}]");
            let ct = if att.content_type.is_empty() {
                "application/octet-stream"
            } else {
                &att.content_type
            };
            mp.add_file(&field, &att.filename, ct, &att.bytes);
        }
        let resp = self.call_api_multipart(&url, mp).await?;
        decode(resp)
    }

    /// Respond to an incoming interaction (ping, button click, modal submit).
    /// Discord requires a response within 3 seconds.
    pub async fn respond_interaction(
        &self,
        interaction_id: &str,
        token: &str,
        response: &InteractionResponse,
    ) -> Result<()> {
        let url = format!("{API_BASE}/interactions/{interaction_id}/{token}/callback");
        let body = serde_json::to_string(response)
            .map_err(|e| Error::from(format!("discord: encode response: {e}")))?;
        let _ = self.call_api(Method::Post, &url, Some(body)).await?;
        Ok(())
    }

    /// Post a follow-up message to an interaction (useful when the initial
    /// response was a deferred ack).
    pub async fn followup_interaction(
        &self,
        token: &str,
        params: CreateMessage,
    ) -> Result<Message> {
        let url = format!(
            "{API_BASE}/webhooks/{}/{token}",
            self.application_id
        );
        let body = serde_json::to_string(&params)
            .map_err(|e| Error::from(format!("discord: encode followup: {e}")))?;
        let resp = self.call_api(Method::Post, &url, Some(body)).await?;
        decode(resp)
    }

    /// Verify the Ed25519 signature of an interaction webhook request.
    /// Required before trusting the body.
    pub async fn verify_interaction(
        &self,
        signature_hex: &str,
        timestamp: &str,
        body: &str,
    ) -> Result<bool> {
        verify_ed25519(&self.public_key_hex, signature_hex, timestamp, body).await
    }

    /// List all text channels in a guild (filters `type == 0`, sorted by
    /// position).
    pub async fn list_guild_text_channels(&self, guild_id: &str) -> Result<Vec<GuildChannel>> {
        let url = format!("{API_BASE}/guilds/{guild_id}/channels");
        let resp = self.call_api(Method::Get, &url, None).await?;
        let mut channels: Vec<GuildChannel> = decode(resp)?;
        channels.retain(|c| c.channel_type == 0);
        channels.sort_by_key(|c| c.position);
        Ok(channels)
    }

    /// Fetch a guild object. Returns `None` on any error (useful when you
    /// want a display name and don't care about failures).
    pub async fn fetch_guild(&self, guild_id: &str) -> Option<Guild> {
        let url = format!("{API_BASE}/guilds/{guild_id}");
        let resp = self.call_api(Method::Get, &url, None).await.ok()?;
        serde_json::from_str(&resp).ok()
    }

    /// Make the bot leave a guild. Best-effort; errors are returned but not
    /// typically acted upon by callers.
    pub async fn leave_guild(&self, guild_id: &str) -> Result<()> {
        let url = format!("{API_BASE}/users/@me/guilds/{guild_id}");
        let _ = self.call_api(Method::Delete, &url, None).await?;
        Ok(())
    }

    async fn call_api(&self, method: Method, url: &str, body: Option<String>) -> Result<String> {
        let headers = Headers::new();
        headers.set("Authorization", &format!("Bot {}", self.token))?;
        headers.set("Content-Type", "application/json")?;

        let mut init = RequestInit::new();
        init.with_method(method).with_headers(headers);
        if let Some(b) = body {
            init.with_body(Some(b.into()));
        }
        let req = Request::new_with_init(url, &init)?;
        let mut resp = Fetch::Request(req).send().await?;
        let text = resp.text().await?;
        if resp.status_code() >= 400 {
            return Err(Error::from(format!(
                "discord API {}: {}",
                resp.status_code(),
                text
            )));
        }
        Ok(text)
    }

    async fn call_api_multipart(&self, url: &str, mp: MultipartBuilder) -> Result<String> {
        let headers = Headers::new();
        headers.set("Authorization", &format!("Bot {}", self.token))?;
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
        let text = resp.text().await?;
        if resp.status_code() >= 400 {
            return Err(Error::from(format!(
                "discord API {}: {}",
                resp.status_code(),
                text
            )));
        }
        Ok(text)
    }
}

/// A file to upload alongside a message via
/// [`DiscordBot::create_message_with_attachments`].
#[derive(Clone, Debug)]
pub struct Attachment {
    /// Filename shown in the chat. Quotes and control characters are
    /// sanitized when emitted into the multipart header.
    pub filename: String,
    /// MIME type (e.g. `image/png`). Defaults to `application/octet-stream`
    /// when empty.
    pub content_type: String,
    pub bytes: Vec<u8>,
}

fn decode<T: serde::de::DeserializeOwned>(body: String) -> Result<T> {
    serde_json::from_str(&body).map_err(|e| Error::from(format!("discord: decode: {e}")))
}

/// Parse an interaction webhook body into an [`Interaction`]. Call
/// [`DiscordBot::verify_interaction`] *first*.
pub fn parse_interaction(body: &[u8]) -> Result<Interaction> {
    serde_json::from_slice(body)
        .map_err(|e| Error::from(format!("discord: decode interaction: {e}")))
}

// ----- Request bodies -----

#[derive(Serialize, Default, Clone, Debug)]
pub struct CreateMessage {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub content: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ActionRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub embeds: Vec<Embed>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<u64>,
}

/// Discord rich embed.
#[derive(Serialize, Default, Clone, Debug)]
pub struct Embed {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 24-bit RGB integer (e.g. `0x5865F2`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<EmbedField>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footer: Option<EmbedFooter>,
}

#[derive(Serialize, Clone, Debug)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub inline: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct EmbedFooter {
    pub text: String,
}

/// Standard Discord button style values. Using the raw integers is fine too
/// — this enum just documents which is which.
pub mod button_style {
    pub const PRIMARY: u8 = 1;
    pub const SECONDARY: u8 = 2;
    pub const SUCCESS: u8 = 3;
    pub const DANGER: u8 = 4;
    pub const LINK: u8 = 5;
}

/// The ephemeral message flag (MESSAGE_FLAGS_EPHEMERAL = 1 << 6).
pub const MESSAGE_FLAG_EPHEMERAL: u64 = 64;

/// A row of interactive components. Discord requires buttons/selects be
/// inside an action row (`type: 1`).
#[derive(Serialize, Clone, Debug)]
pub struct ActionRow {
    #[serde(rename = "type")]
    component_type: u8,
    components: Vec<Component>,
}

impl ActionRow {
    pub fn new(components: Vec<Component>) -> Self {
        Self {
            component_type: 1,
            components,
        }
    }
}

/// Discord message component. Use the constructors below rather than
/// setting fields directly — Discord's `type` numbers are non-obvious
/// (2 = Button, 4 = Text Input).
#[derive(Serialize, Clone, Debug)]
pub struct Component {
    #[serde(rename = "type")]
    pub component_type: u8,
    pub custom_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

impl Component {
    /// A button with the given style (see [`button_style`]).
    pub fn button(
        style: u8,
        custom_id: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Component {
            component_type: 2,
            custom_id: custom_id.into(),
            style: Some(style),
            label: Some(label.into()),
            value: None,
            required: None,
        }
    }
    /// Shortcut for [`button_style::PRIMARY`].
    pub fn primary_button(custom_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::button(button_style::PRIMARY, custom_id, label)
    }
    /// Shortcut for [`button_style::SUCCESS`] (green).
    pub fn success_button(custom_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::button(button_style::SUCCESS, custom_id, label)
    }
    /// Shortcut for [`button_style::DANGER`] (red).
    pub fn danger_button(custom_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self::button(button_style::DANGER, custom_id, label)
    }
    /// A paragraph-style text input for use inside a modal.
    pub fn paragraph_input(custom_id: impl Into<String>, label: impl Into<String>) -> Self {
        Component {
            component_type: 4,
            custom_id: custom_id.into(),
            style: Some(2),
            label: Some(label.into()),
            value: None,
            required: Some(true),
        }
    }
}

/// The body of a `POST /interactions/{id}/{token}/callback`.
#[derive(Serialize, Clone, Debug)]
#[serde(untagged)]
pub enum InteractionResponse {
    /// `type: 1` — reply to a ping.
    Pong {
        #[serde(rename = "type")]
        kind: u8,
    },
    /// `type: 9` — show a modal dialog.
    Modal {
        #[serde(rename = "type")]
        kind: u8,
        data: ModalData,
    },
    /// `type: 4` — respond with a message (visible to everyone by default;
    /// set `flags = 64` for ephemeral).
    Message {
        #[serde(rename = "type")]
        kind: u8,
        data: MessageData,
    },
    /// `type: 6` — acknowledge a component click without sending anything.
    DeferredUpdate {
        #[serde(rename = "type")]
        kind: u8,
    },
}

impl InteractionResponse {
    pub fn pong() -> Self {
        InteractionResponse::Pong { kind: 1 }
    }
    pub fn modal(custom_id: impl Into<String>, title: impl Into<String>, components: Vec<ActionRow>) -> Self {
        InteractionResponse::Modal {
            kind: 9,
            data: ModalData {
                custom_id: custom_id.into(),
                title: title.into(),
                components,
            },
        }
    }
    pub fn ephemeral_message(content: impl Into<String>) -> Self {
        InteractionResponse::Message {
            kind: 4,
            data: MessageData {
                content: content.into(),
                flags: Some(64),
            },
        }
    }
    pub fn deferred_update() -> Self {
        InteractionResponse::DeferredUpdate { kind: 6 }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct ModalData {
    pub custom_id: String,
    pub title: String,
    pub components: Vec<ActionRow>,
}

#[derive(Serialize, Clone, Debug)]
pub struct MessageData {
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flags: Option<u64>,
}

// ----- Incoming interaction types -----

#[derive(Deserialize, Clone, Debug)]
pub struct Interaction {
    pub id: String,
    pub token: String,
    #[serde(rename = "type")]
    pub kind: u8,
    #[serde(default)]
    pub data: Option<InteractionData>,
    #[serde(default)]
    pub message: Option<Message>,
    /// Guild member object when the interaction happened inside a guild.
    /// Carried as raw JSON so callers can pull out the fields they need
    /// (e.g. `member["permissions"]` to check a bit like MANAGE_GUILD).
    #[serde(default)]
    pub member: Option<serde_json::Value>,
    #[serde(default)]
    pub guild_id: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
}

impl Interaction {
    pub fn is_ping(&self) -> bool {
        self.kind == 1
    }
    pub fn is_application_command(&self) -> bool {
        self.kind == 2
    }
    pub fn is_component_click(&self) -> bool {
        self.kind == 3
    }
    pub fn is_modal_submit(&self) -> bool {
        self.kind == 5
    }
    /// Returns true if `member.permissions` has all of the given permission
    /// bits set. Permissions are a bitmask; common values:
    /// `MANAGE_GUILD = 0x20`, `ADMINISTRATOR = 0x08`.
    pub fn member_has_permissions(&self, bits: u64) -> bool {
        let perms = self
            .member
            .as_ref()
            .and_then(|m| m.get("permissions"))
            .and_then(|p| p.as_str())
            .and_then(|p| p.parse::<u64>().ok())
            .unwrap_or(0);
        perms & bits == bits
    }
    /// For modal submits: extract the text the user entered into the given
    /// input `custom_id`.
    pub fn modal_text(&self, input_custom_id: &str) -> Option<&str> {
        let components = self.data.as_ref()?.components.as_ref()?;
        for row in components {
            for comp in &row.components {
                if comp.custom_id == input_custom_id {
                    return comp.value.as_deref();
                }
            }
        }
        None
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct InteractionData {
    #[serde(default)]
    pub custom_id: Option<String>,
    #[serde(default)]
    pub components: Option<Vec<IncomingActionRow>>,
    /// Slash-command name (for APPLICATION_COMMAND interactions).
    #[serde(default)]
    pub name: Option<String>,
    /// Slash-command options (recursive: subcommands/groups carry nested options).
    #[serde(default)]
    pub options: Option<Vec<CommandOption>>,
}

/// A single option on a slash command. Subcommands and subcommand groups
/// carry their own nested `options` list.
#[derive(Deserialize, Clone, Debug)]
pub struct CommandOption {
    pub name: String,
    #[serde(default)]
    pub value: Option<serde_json::Value>,
    #[serde(default)]
    pub options: Vec<CommandOption>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct IncomingActionRow {
    pub components: Vec<IncomingComponent>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct IncomingComponent {
    pub custom_id: String,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Message {
    pub id: String,
    #[serde(default)]
    pub content: Option<String>,
    pub channel_id: String,
}

/// A text channel in a guild, as returned by `GET /guilds/{id}/channels`.
#[derive(Deserialize, Clone, Debug)]
pub struct GuildChannel {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub position: i32,
    #[serde(rename = "type")]
    pub channel_type: u32,
}

#[derive(Deserialize, Clone, Debug)]
pub struct Guild {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
}

// ----- Ed25519 via WebCrypto -----

async fn verify_ed25519(
    public_key_hex: &str,
    signature_hex: &str,
    timestamp: &str,
    body: &str,
) -> Result<bool> {
    let public_key_bytes = hex_decode(public_key_hex)?;
    let signature_bytes = hex_decode(signature_hex)?;
    let message = format!("{timestamp}{body}");

    let crypto = get_subtle()?;

    let algorithm = js_sys::Object::new();
    js_sys::Reflect::set(
        &algorithm,
        &JsValue::from_str("name"),
        &JsValue::from_str("NODE-ED25519"),
    )
    .map_err(|_| Error::from("algorithm name"))?;
    js_sys::Reflect::set(
        &algorithm,
        &JsValue::from_str("namedCurve"),
        &JsValue::from_str("NODE-ED25519"),
    )
    .map_err(|_| Error::from("algorithm namedCurve"))?;

    let usages = js_sys::Array::new();
    usages.push(&JsValue::from_str("verify"));

    let key_data = js_sys::Uint8Array::from(public_key_bytes.as_slice());
    let key_promise = crypto
        .import_key_with_object("raw", &key_data.buffer(), &algorithm, true, &usages)
        .map_err(|e| Error::from(format!("importKey call: {e:?}")))?;
    let crypto_key: web_sys::CryptoKey = wasm_bindgen_futures::JsFuture::from(key_promise)
        .await
        .map_err(|e| Error::from(format!("importKey await: {e:?}")))?
        .dyn_into()
        .map_err(|_| Error::from("cast CryptoKey"))?;

    let sig_array = js_sys::Uint8Array::from(signature_bytes.as_slice());
    let msg_array = js_sys::Uint8Array::from(message.as_bytes());

    let verify_promise = crypto
        .verify_with_object_and_buffer_source_and_buffer_source(
            &algorithm,
            &crypto_key,
            &sig_array,
            &msg_array,
        )
        .map_err(|e| Error::from(format!("verify call: {e:?}")))?;
    let ok = wasm_bindgen_futures::JsFuture::from(verify_promise)
        .await
        .map_err(|e| Error::from(format!("verify await: {e:?}")))?;
    Ok(ok.as_bool().unwrap_or(false))
}

fn get_subtle() -> Result<web_sys::SubtleCrypto> {
    let global = js_sys::global();
    let crypto = js_sys::Reflect::get(&global, &JsValue::from_str("crypto"))
        .map_err(|_| Error::from("no crypto global"))?;
    let crypto: web_sys::Crypto = crypto
        .dyn_into()
        .map_err(|_| Error::from("crypto not Crypto"))?;
    Ok(crypto.subtle())
}

fn hex_decode(hex: &str) -> Result<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return Err(Error::from("hex: odd length"));
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map_err(|_| Error::from("hex: invalid")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_decode_ok() {
        assert_eq!(hex_decode("00ff10").unwrap(), vec![0, 0xff, 0x10]);
    }

    #[test]
    fn hex_decode_rejects_odd_length() {
        assert!(hex_decode("abc").is_err());
    }

    #[test]
    fn hex_decode_rejects_non_hex() {
        assert!(hex_decode("zz").is_err());
    }

    #[test]
    fn interaction_kind_helpers() {
        let ping = Interaction {
            id: "1".into(),
            token: "t".into(),
            kind: 1,
            data: None,
            message: None,
            member: None,
            guild_id: None,
            channel_id: None,
        };
        assert!(ping.is_ping());
        assert!(!ping.is_component_click());
        assert!(!ping.is_modal_submit());

        let component = Interaction { kind: 3, ..ping.clone() };
        assert!(component.is_component_click());

        let modal = Interaction { kind: 5, ..ping };
        assert!(modal.is_modal_submit());
    }

    #[test]
    fn parse_modal_submit() {
        let body = br#"{
            "id": "1",
            "token": "t",
            "type": 5,
            "channel_id": "c",
            "data": {
                "custom_id": "reply:tg:abc",
                "components": [
                    {"components": [{"custom_id": "reply_text", "value": "My reply"}]}
                ]
            }
        }"#;
        let interaction = parse_interaction(body).unwrap();
        assert!(interaction.is_modal_submit());
        assert_eq!(
            interaction.data.as_ref().unwrap().custom_id.as_deref(),
            Some("reply:tg:abc")
        );
        assert_eq!(interaction.modal_text("reply_text"), Some("My reply"));
        assert_eq!(interaction.modal_text("missing"), None);
    }

    #[test]
    fn parse_button_click_keeps_message() {
        let body = br#"{
            "id": "1",
            "token": "t",
            "type": 3,
            "channel_id": "c",
            "message": {"id": "m1", "channel_id": "c", "content": "hello"},
            "data": {"custom_id": "reply:dc:xyz"}
        }"#;
        let i = parse_interaction(body).unwrap();
        assert!(i.is_component_click());
        assert_eq!(i.message.as_ref().unwrap().id, "m1");
        assert_eq!(
            i.data.as_ref().unwrap().custom_id.as_deref(),
            Some("reply:dc:xyz")
        );
    }

    #[test]
    fn serialize_pong() {
        let r = InteractionResponse::pong();
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j, serde_json::json!({"type": 1}));
    }

    #[test]
    fn serialize_ephemeral_message() {
        let r = InteractionResponse::ephemeral_message("done");
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(
            j,
            serde_json::json!({"type": 4, "data": {"content": "done", "flags": 64}})
        );
    }

    #[test]
    fn serialize_modal() {
        let r = InteractionResponse::modal(
            "reply:tg:abc",
            "Reply",
            vec![ActionRow::new(vec![Component::paragraph_input(
                "reply_text",
                "Your reply",
            )])],
        );
        let j = serde_json::to_value(&r).unwrap();
        assert_eq!(j["type"], 9);
        assert_eq!(j["data"]["custom_id"], "reply:tg:abc");
        assert_eq!(j["data"]["title"], "Reply");
        assert_eq!(j["data"]["components"][0]["type"], 1);
        assert_eq!(j["data"]["components"][0]["components"][0]["custom_id"], "reply_text");
    }

    #[test]
    fn create_message_serializes_with_button() {
        let m = CreateMessage {
            content: "From: alice".into(),
            components: vec![ActionRow::new(vec![Component::primary_button(
                "reply:tg:msg42",
                "Reply",
            )])],
            embeds: vec![],
            flags: None,
        };
        let j = serde_json::to_value(&m).unwrap();
        assert_eq!(j["content"], "From: alice");
        assert_eq!(j["components"][0]["type"], 1);
        assert_eq!(j["components"][0]["components"][0]["type"], 2);
        assert_eq!(j["components"][0]["components"][0]["style"], 1);
        assert_eq!(j["components"][0]["components"][0]["custom_id"], "reply:tg:msg42");
        assert_eq!(j["components"][0]["components"][0]["label"], "Reply");
    }

    #[test]
    fn create_message_with_embed_serializes() {
        let m = CreateMessage {
            content: String::new(),
            embeds: vec![Embed {
                title: Some("New message from alice".into()),
                description: Some("Order confirmed".into()),
                color: Some(0xF38020),
                fields: vec![EmbedField {
                    name: "From".into(),
                    value: "alice@example.com".into(),
                    inline: true,
                }],
                footer: Some(EmbedFooter {
                    text: "Rule: catch-all".into(),
                }),
            }],
            ..Default::default()
        };
        let j = serde_json::to_value(&m).unwrap();
        // content is empty + skipped
        assert!(j.get("content").is_none());
        assert_eq!(j["embeds"][0]["title"], "New message from alice");
        assert_eq!(j["embeds"][0]["color"], 0xF38020);
        assert_eq!(j["embeds"][0]["fields"][0]["name"], "From");
        assert_eq!(j["embeds"][0]["fields"][0]["inline"], true);
        assert_eq!(j["embeds"][0]["footer"]["text"], "Rule: catch-all");
    }

    #[test]
    fn member_permissions_check() {
        let mut i = Interaction {
            id: "1".into(),
            token: "t".into(),
            kind: 2,
            data: None,
            message: None,
            member: Some(serde_json::json!({"permissions": "32"})),
            guild_id: Some("g".into()),
            channel_id: None,
        };
        assert!(i.member_has_permissions(0x20));
        assert!(!i.member_has_permissions(0x40));
        i.member = None;
        assert!(!i.member_has_permissions(0x20));
    }

    #[test]
    fn paragraph_input_serializes_as_text_input() {
        let c = Component::paragraph_input("reply_text", "Your reply");
        let j = serde_json::to_value(&c).unwrap();
        assert_eq!(j["type"], 4);
        assert_eq!(j["style"], 2);
        assert_eq!(j["custom_id"], "reply_text");
        assert_eq!(j["required"], true);
        // value omitted when None
        assert!(j.get("value").is_none());
    }

    #[test]
    fn empty_components_skipped_in_json() {
        let m = CreateMessage {
            content: "plain".into(),
            ..Default::default()
        };
        let j = serde_json::to_string(&m).unwrap();
        assert!(!j.contains("components"));
    }
}
