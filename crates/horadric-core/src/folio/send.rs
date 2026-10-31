//! The lines a Folio writes to the CLI's stdin: prompts, with images or
//! queued, and the control requests behind the composer's controls. Each
//! is checked against the real CLI (tests/fixtures/headless).

use serde_json::{json, Value};

/// An image pasted into a prompt, as the Messages API takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageData {
    /// Such as `image/png`.
    pub media_type: String,
    /// The image's bytes in standard base64, no line breaks.
    pub base64: String,
}

impl ImageData {
    /// An image from its file's bytes, its type known from their first
    /// bytes. None for a format the API does not take.
    pub fn from_bytes(bytes: &[u8]) -> Option<ImageData> {
        let media_type = image_type(bytes)?;
        Some(ImageData {
            media_type: media_type.to_string(),
            base64: crate::release::base64_encode(bytes),
        })
    }
}

/// The media type of an image the API takes (PNG, JPEG, GIF, WebP), from
/// its magic bytes.
pub fn image_type(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

/// When a prompt sent while Claude works is taken up. Without one the CLI
/// treats it as `Next`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Stops the turn running now and starts one with this prompt.
    Now,
    /// Steers the turn running now: the CLI folds it in after the next tool
    /// step, or starts a turn with it if that turn ends with no more steps.
    Next,
    /// After everything else queued.
    Later,
}

impl Priority {
    fn as_str(self) -> &'static str {
        match self {
            Priority::Now => "now",
            Priority::Next => "next",
            Priority::Later => "later",
        }
    }
}

fn user_line(content: Value, uuid: Option<&str>, priority: Option<Priority>) -> String {
    let mut line = json!({
        "type": "user",
        "message": {"role": "user", "content": content},
        "parent_tool_use_id": null,
        "session_id": "",
    });
    if let Some(u) = uuid {
        line["uuid"] = json!(u);
    }
    if let Some(p) = priority {
        line["priority"] = json!(p.as_str());
    }
    line.to_string()
}

/// A prompt of plain text.
pub fn prompt_line(text: &str) -> String {
    user_line(json!(text), None, None)
}

/// A prompt with images, the images first as the terminal sends them.
pub fn prompt_line_with_images(text: &str, images: &[ImageData]) -> String {
    user_line(content_of(text, images), None, None)
}

/// A prompt's content: plain text, or blocks when it has images.
fn content_of(text: &str, images: &[ImageData]) -> Value {
    if images.is_empty() {
        return json!(text);
    }
    let mut content: Vec<Value> = images
        .iter()
        .map(|i| {
            json!({
                "type": "image",
                "source": {"type": "base64", "media_type": i.media_type, "data": i.base64},
            })
        })
        .collect();
    if !text.is_empty() {
        content.push(json!({"type": "text", "text": text}));
    }
    Value::Array(content)
}

/// A prompt sent while a turn runs, with the uuid the Folio knows it by,
/// so its pending block can be matched when the CLI takes it up.
pub fn queued_prompt_line(
    text: &str,
    images: &[ImageData],
    uuid: &str,
    priority: Priority,
) -> String {
    user_line(content_of(text, images), Some(uuid), Some(priority))
}

/// A `control_request` of `subtype` with `fields` beside it.
pub fn control_request_line(request_id: &str, subtype: &str, fields: Value) -> String {
    let mut request = json!({"subtype": subtype});
    if let (Some(r), Some(f)) = (request.as_object_mut(), fields.as_object()) {
        for (k, v) in f {
            r.insert(k.clone(), v.clone());
        }
    }
    json!({"type": "control_request", "request_id": request_id, "request": request}).to_string()
}

/// A `success` control_response to a request the CLI made.
pub fn control_success(request_id: &str, response: Value) -> String {
    json!({
        "type": "control_response",
        "response": {"subtype": "success", "request_id": request_id, "response": response},
    })
    .to_string()
}

/// What a line typed into a session asks of a Folio. A terminal takes
/// `/model` and `/effort` as typed; headless, the model switches through
/// a control request, and effort is a prompt the composer must hear of to
/// show the level picked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed<'a> {
    Model(&'a str),
    Effort(&'a str),
    Prompt(&'a str),
}

/// Reads `text` as [`Typed`]. A bare `/model` or `/effort` opens a picker
/// in the terminal, which headless has not, so it stays a prompt.
pub fn typed(text: &str) -> Typed<'_> {
    let text = text.trim();
    let mut words = text.splitn(2, char::is_whitespace);
    let (command, value) = (words.next().unwrap_or(""), words.next().map(str::trim));
    match (command, value) {
        ("/model", Some(m)) if !m.is_empty() => Typed::Model(m),
        ("/effort", Some(l)) if !l.is_empty() => Typed::Effort(l),
        _ => Typed::Prompt(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_and_effort_are_read_from_what_is_typed() {
        assert_eq!(typed("/model claude-opus-5-5"), Typed::Model("claude-opus-5-5"));
        assert_eq!(typed(" /effort  high \r\n"), Typed::Effort("high"));
        assert_eq!(typed("/model"), Typed::Prompt("/model"));
        assert_eq!(typed("/models x"), Typed::Prompt("/models x"));
        assert_eq!(typed("go on\r"), Typed::Prompt("go on"));
    }

    #[test]
    fn a_prompt_with_images_puts_them_first() {
        let png = [0x89, b'P', b'N', b'G', 0, 1];
        let img = ImageData::from_bytes(&png).unwrap();
        assert_eq!(img.media_type, "image/png");
        let v: Value = serde_json::from_str(&prompt_line_with_images("what?", &[img])).unwrap();
        let c = &v["message"]["content"];
        assert_eq!(c[0]["type"], "image");
        assert_eq!(c[0]["source"]["type"], "base64");
        assert_eq!(c[0]["source"]["media_type"], "image/png");
        assert_eq!(c[0]["source"]["data"], "iVBORwAB");
        assert_eq!(c[1]["text"], "what?");
        assert_eq!(v["type"], "user");
        assert!(v["parent_tool_use_id"].is_null());
    }

    #[test]
    fn without_images_a_prompt_is_plain_text() {
        let v: Value = serde_json::from_str(&prompt_line_with_images("hi", &[])).unwrap();
        assert_eq!(v["message"]["content"], "hi");
    }

    #[test]
    fn images_are_known_by_their_first_bytes() {
        assert_eq!(image_type(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(image_type(b"GIF89a"), Some("image/gif"));
        assert_eq!(image_type(b"RIFF\0\0\0\0WEBPVP8"), Some("image/webp"));
        assert_eq!(image_type(b"BM"), None);
    }

    #[test]
    fn a_queued_prompt_carries_its_uuid_and_priority() {
        let v: Value =
            serde_json::from_str(&queued_prompt_line("x", &[], "u1", Priority::Now)).unwrap();
        assert_eq!(v["uuid"], "u1");
        assert_eq!(v["priority"], "now");
    }

    #[test]
    fn a_control_request_carries_its_fields() {
        let v: Value = serde_json::from_str(&control_request_line(
            "r1",
            "set_permission_mode",
            json!({"mode": "plan"}),
        ))
        .unwrap();
        assert_eq!(v["type"], "control_request");
        assert_eq!(v["request_id"], "r1");
        assert_eq!(v["request"]["subtype"], "set_permission_mode");
        assert_eq!(v["request"]["mode"], "plan");
    }
}
