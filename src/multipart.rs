//! Tiny `multipart/form-data` encoder used by the Telegram `sendPhoto` and
//! Discord file-upload paths. The browser-compatible `FormData` shim works
//! in Workers but is awkward to drive from Rust; assembling the body by
//! hand is shorter and lets the bot clients pass plain bytes to `Fetch`.

use serde::Serialize;
use worker::*;

/// Builder for a `multipart/form-data` body. Append text and file parts in
/// the order you want them, then call [`MultipartBuilder::finish`].
pub struct MultipartBuilder {
    boundary: String,
    body: Vec<u8>,
}

impl MultipartBuilder {
    /// Create a new builder with a random boundary.
    pub fn new() -> Self {
        Self {
            boundary: random_boundary(),
            body: Vec::new(),
        }
    }

    /// `multipart/form-data; boundary={…}` — set this as the request's
    /// `Content-Type` header.
    pub fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={}", self.boundary)
    }

    /// Append a text-valued form field.
    pub fn add_text(&mut self, name: &str, value: &str) {
        self.write_part_header(name, None, None);
        self.body.extend_from_slice(value.as_bytes());
        self.body.extend_from_slice(b"\r\n");
    }

    /// Append the JSON encoding of `value` as a form field. Returns an error
    /// if serialization fails.
    pub fn add_json<T: Serialize>(&mut self, name: &str, value: &T) -> Result<()> {
        let s = serde_json::to_string(value)
            .map_err(|e| Error::from(format!("multipart: encode {name}: {e}")))?;
        self.write_part_header(name, None, Some("application/json"));
        self.body.extend_from_slice(s.as_bytes());
        self.body.extend_from_slice(b"\r\n");
        Ok(())
    }

    /// Append a file-valued form field. `content_type` defaults to
    /// `application/octet-stream` if empty.
    pub fn add_file(&mut self, name: &str, filename: &str, content_type: &str, bytes: &[u8]) {
        let ct = if content_type.is_empty() {
            "application/octet-stream"
        } else {
            content_type
        };
        self.write_part_header(name, Some(filename), Some(ct));
        self.body.extend_from_slice(bytes);
        self.body.extend_from_slice(b"\r\n");
    }

    /// Close the body and return the assembled bytes.
    pub fn finish(mut self) -> Vec<u8> {
        self.body.extend_from_slice(b"--");
        self.body.extend_from_slice(self.boundary.as_bytes());
        self.body.extend_from_slice(b"--\r\n");
        self.body
    }

    fn write_part_header(&mut self, name: &str, filename: Option<&str>, content_type: Option<&str>) {
        self.body.extend_from_slice(b"--");
        self.body.extend_from_slice(self.boundary.as_bytes());
        self.body.extend_from_slice(b"\r\n");
        let safe_name = sanitize_header_value(name);
        let mut disposition = format!("Content-Disposition: form-data; name=\"{safe_name}\"");
        if let Some(fname) = filename {
            let safe_fname = sanitize_header_value(fname);
            disposition.push_str(&format!("; filename=\"{safe_fname}\""));
        }
        disposition.push_str("\r\n");
        self.body.extend_from_slice(disposition.as_bytes());
        if let Some(ct) = content_type {
            self.body
                .extend_from_slice(format!("Content-Type: {ct}\r\n").as_bytes());
        }
        self.body.extend_from_slice(b"\r\n");
    }
}

impl Default for MultipartBuilder {
    fn default() -> Self {
        Self::new()
    }
}

fn sanitize_header_value(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '"' | '\r' | '\n' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect()
}

/// Random 16-hex-char suffix. Not crypto-strong, but a multipart boundary
/// only needs to be unique within a single request body.
fn random_boundary() -> String {
    let n: f64 = js_sys::Math::random();
    let bits = (n.abs() * 1.844_674_407_370_955e19_f64) as u64;
    format!("----botrelay-{bits:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boundary_of(builder: &MultipartBuilder) -> &str {
        &builder.boundary
    }

    #[test]
    fn text_part_round_trip_shape() {
        let mut mp = MultipartBuilder {
            boundary: "TEST".into(),
            body: Vec::new(),
        };
        mp.add_text("chat_id", "-100");
        let body = mp.finish();
        let s = std::str::from_utf8(&body).unwrap();
        assert!(s.contains("--TEST\r\n"));
        assert!(s.contains("Content-Disposition: form-data; name=\"chat_id\"\r\n\r\n-100\r\n"));
        assert!(s.ends_with("--TEST--\r\n"));
    }

    #[test]
    fn file_part_includes_filename_and_content_type() {
        let mut mp = MultipartBuilder {
            boundary: "B".into(),
            body: Vec::new(),
        };
        mp.add_file("photo", "email.png", "image/png", &[1, 2, 3, 4]);
        let body = mp.finish();
        let s = std::str::from_utf8(&body).unwrap();
        assert!(s.contains(
            "Content-Disposition: form-data; name=\"photo\"; filename=\"email.png\"\r\n"
        ));
        assert!(s.contains("Content-Type: image/png\r\n"));
        // Body bytes appear verbatim before the closing boundary.
        let expected_chunk = b"\r\n\r\n\x01\x02\x03\x04\r\n--B--\r\n";
        assert!(body.windows(expected_chunk.len()).any(|w| w == expected_chunk));
    }

    #[test]
    fn sanitizes_quotes_and_newlines_in_filename() {
        let mut mp = MultipartBuilder {
            boundary: "B".into(),
            body: Vec::new(),
        };
        mp.add_file("f", "bad\"name\r\n.txt", "text/plain", b"x");
        let body = mp.finish();
        let s = std::str::from_utf8(&body).unwrap();
        assert!(s.contains("filename=\"bad_name__.txt\""));
    }

    #[test]
    fn json_part_serializes_value() {
        let mut mp = MultipartBuilder {
            boundary: "B".into(),
            body: Vec::new(),
        };
        mp.add_json("payload_json", &serde_json::json!({"content": "hi"}))
            .unwrap();
        let body = mp.finish();
        let s = std::str::from_utf8(&body).unwrap();
        assert!(s.contains("name=\"payload_json\""));
        assert!(s.contains("Content-Type: application/json"));
        assert!(s.contains("{\"content\":\"hi\"}"));
    }

    #[test]
    fn content_type_header_includes_boundary() {
        let mp = MultipartBuilder {
            boundary: "ABCD".into(),
            body: Vec::new(),
        };
        assert_eq!(mp.content_type(), "multipart/form-data; boundary=ABCD");
        assert_eq!(boundary_of(&mp), "ABCD");
    }
}
