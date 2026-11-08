//! What the markdown editor asks of the host besides writes: a block of
//! markdown drawn as the saved page draws it (`render`), and a picture
//! pasted or dropped into the page kept beside the file (`save-image`).
//! The protocol is in assets/forge/README.md, "Editing the whole file".

use std::io::Write;
use std::path::{Path, PathBuf};

use horadric_core::forge::Kind;
use serde_json::{json, Value};

/// Past this much text a render goes to a thread of its own, so a whole
/// long document drawn again never holds the UI thread. A block is far
/// smaller and is drawn at once.
const RENDER_INLINE: usize = 32 << 10;

/// The largest picture a page may save, in bytes once decoded.
const IMAGE_MAX: usize = 20 << 20;

/// The folder beside the file that pictures go into.
const ASSETS: &str = "assets";

/// A `forge: "render"`: the markdown in `text` drawn by the core's
/// renderer, answered as `rendered` with the same `id`.
pub fn on_render(key: &str, tab: u64, msg: &Value) {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let markdown = super::super::file_of(key, tab)
        .is_some_and(|f| super::super::kind(&f) == Some(Kind::Markdown));
    let text = msg
        .get("text")
        .and_then(Value::as_str)
        .filter(|t| t.len() <= super::WRITE_MAX);
    let (true, Some(text)) = (markdown, text) else {
        let why = if markdown {
            "the text is missing or too large"
        } else {
            "only markdown is drawn here"
        };
        super::super::post(
            key,
            tab,
            &json!({"forge": "rendered", "id": id, "ok": false, "why": why}),
        );
        return;
    };
    if text.len() <= RENDER_INLINE {
        super::super::post(key, tab, &rendered(&id, text));
        return;
    }
    let (key, text) = (key.to_string(), text.to_string());
    let _ = std::thread::Builder::new()
        .name("forge-render".into())
        .spawn(move || {
            let msg = rendered(&id, &text);
            if let Ok(mut d) = super::DONE.lock() {
                d.push((key, tab, msg));
            }
            super::super::wake();
        });
}

/// The answer to a render. A diagram in it needs Mermaid, which the page
/// loads only when it had one already; it is told where Mermaid is and how
/// to dress it in Horadric's look.
fn rendered(id: &Value, text: &str) -> Value {
    let html = horadric_core::forge::markdown_body(text);
    let mut msg = json!({"forge": "rendered", "id": id, "ok": true});
    if html.contains("<pre class=\"mermaid\"") {
        msg["mermaid"] = json!({
            "src": format!("{}mermaid.js", super::super::runtime::base()),
            "look": super::super::runtime::mermaid_look(),
        });
    }
    msg["html"] = json!(html);
    msg
}

/// A `forge: "save-image"`: the picture in `data` (base64) written into
/// `assets/` beside the page's markdown file under a name of its own,
/// answered as `image-saved` with the path to write in the markdown.
pub fn on_save_image(key: &str, tab: u64, msg: &Value) {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let refuse = |why: &str| {
        super::super::post(
            key,
            tab,
            &json!({"forge": "image-saved", "id": id, "ok": false, "why": why}),
        );
    };
    if !super::may_write(key, tab) {
        return refuse("the page may not write now");
    }
    let page_file = msg.get("file").and_then(Value::as_str).unwrap_or_default();
    let Some(file) = super::own_file(key, tab, page_file) else {
        return refuse("a page can only change its own file");
    };
    if super::super::kind(&file) != Some(Kind::Markdown) {
        return refuse("pictures are saved beside markdown only");
    }
    let Some(root) = super::super::TABS.with(|t| {
        t.borrow()
            .iter()
            .find(|f| f.key == key && f.id == tab)
            .map(|f| f.root.clone())
    }) else {
        return refuse("the tab is gone");
    };
    // Checked before decoding, so a page cannot make us hold a huge copy.
    let data = msg.get("data").and_then(Value::as_str).unwrap_or_default();
    let data = data.split_once(',').map_or(data, |(_, b)| b);
    if data.is_empty() || data.len() > IMAGE_MAX / 3 * 4 + 4 {
        return refuse("the picture is missing or larger than 20 MB");
    }
    let name = msg
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let data = data.to_string();
    let key = key.to_string();
    // Decoding and the disk are off the UI thread.
    let _ = std::thread::Builder::new()
        .name("forge-image".into())
        .spawn(move || {
            let mut out = json!({"forge": "image-saved", "id": id});
            match horadric_core::release::base64_decode(&data)
                .ok_or_else(|| "the picture is not base64".to_string())
                .and_then(|bytes| save_beside(&file, &root, &name, &bytes))
            {
                Ok(path) => {
                    out["ok"] = json!(true);
                    out["path"] = json!(path);
                }
                Err(why) => {
                    out["ok"] = json!(false);
                    out["why"] = json!(why);
                }
            }
            if let Ok(mut d) = super::DONE.lock() {
                d.push((key, tab, out));
            }
            super::super::wake();
        });
}

/// The extension for a picture, by its first bytes, never by what the
/// page called it.
fn image_ext(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else {
        None
    }
}

/// A file name's stem made safe to write and to put in markdown unquoted:
/// letters, digits, `-` and `_` only, never empty.
fn image_stem(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let stem = base.rsplit_once('.').map_or(base, |(s, _)| s);
    let mut out = String::new();
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            out.push(c.to_ascii_lowercase());
        } else if (c == '-' || c.is_whitespace() || c == '.') && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 48 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "image".into()
    } else {
        out
    }
}

/// Writes `bytes` into `assets/` beside `file` under a name no file has
/// yet, and answers the path from the file to it. The folder must be in
/// `root`, the folder the tab serves, however it is reached (a junction
/// named `assets` pointing elsewhere is refused).
fn save_beside(file: &Path, root: &Path, name: &str, bytes: &[u8]) -> Result<String, String> {
    if bytes.len() > IMAGE_MAX {
        return Err("the picture is larger than 20 MB".into());
    }
    let ext = image_ext(bytes).ok_or("only PNG, JPEG, GIF and WebP pictures are saved")?;
    let parent = file.parent().ok_or("the file has no folder")?;
    let outside = || format!("{ASSETS} is outside the folder the Forge shows");
    if !inside(parent, root) {
        return Err(outside());
    }
    let dir = parent.join(ASSETS);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot make {ASSETS}: {e}"))?;
    if !inside(&dir, root) {
        return Err(outside());
    }
    let stem = image_stem(name);
    for n in 1..10_000 {
        let fname = if n == 1 {
            format!("{stem}.{ext}")
        } else {
            format!("{stem}-{n}.{ext}")
        };
        let path: PathBuf = dir.join(&fname);
        // Made new or not at all, so two saves never take the same name.
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut f) => {
                if let Err(e) = f.write_all(bytes) {
                    drop(f);
                    let _ = std::fs::remove_file(&path);
                    return Err(format!("cannot write the picture: {e}"));
                }
                return Ok(format!("{ASSETS}/{fname}"));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("cannot write the picture: {e}")),
        }
    }
    Err("too many pictures of that name".into())
}

/// Whether `dir`, followed through any links, is `root` or below it.
fn inside(dir: &Path, root: &Path) -> bool {
    match (std::fs::canonicalize(dir), std::fs::canonicalize(root)) {
        (Ok(d), Ok(r)) => d.starts_with(r),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n rest of it";

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("forge-page-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn forge_render_draws_a_block_as_the_page_does() {
        let msg = rendered(&json!(4), "# Title\n\n- [ ] todo\n");
        assert_eq!(msg["id"], 4);
        assert_eq!(
            msg["html"],
            horadric_core::forge::markdown_body("# Title\n\n- [ ] todo\n")
        );
        assert!(msg["html"].as_str().unwrap().contains("data-lines=\"1-1\""));
        assert!(msg.get("mermaid").is_none());
        let diagram = rendered(&json!(5), "```mermaid\ngraph TD\n  A-->B\n```\n");
        assert!(diagram["mermaid"]["src"]
            .as_str()
            .unwrap()
            .ends_with("mermaid.js"));
        assert!(!diagram["mermaid"]["look"].as_str().unwrap().is_empty());
    }

    #[test]
    fn forge_image_names_are_safe() {
        assert_eq!(image_stem("Screen Shot 2026.png"), "screen-shot-2026");
        assert_eq!(image_stem("../../etc/passwd"), "passwd");
        assert_eq!(image_stem("..\\..\\win.ini"), "win");
        assert_eq!(image_stem("ÆØÅ.png"), "image");
        assert_eq!(image_stem(""), "image");
        assert_eq!(image_stem("a<b>|c:d*.png"), "abcd");
        assert_eq!(image_ext(PNG), Some("png"));
        assert_eq!(image_ext(b"<svg onload=x>"), None);
    }

    #[test]
    fn forge_images_go_beside_the_file_under_new_names() {
        let root = scratch("save");
        let file = root.join("notes").join("a.md");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "# a\n").unwrap();
        assert_eq!(
            save_beside(&file, &root, "shot.png", PNG).unwrap(),
            "assets/shot.png"
        );
        assert_eq!(
            save_beside(&file, &root, "shot.png", PNG).unwrap(),
            "assets/shot-2.png"
        );
        assert_eq!(
            std::fs::read(root.join("notes/assets/shot.png")).unwrap(),
            PNG
        );
        // The name is the page's, the extension the bytes'.
        assert_eq!(
            save_beside(&file, &root, "x.exe", PNG).unwrap(),
            "assets/x.png"
        );
        assert!(save_beside(&file, &root, "x.png", b"MZ not a picture").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn forge_images_stay_in_the_served_folder() {
        let root = scratch("root");
        let outside = scratch("outside");
        // A file the tab claims but that is not under its folder.
        let file = outside.join("b.md");
        std::fs::write(&file, "b\n").unwrap();
        assert!(save_beside(&file, &root, "p.png", PNG).is_err());
        assert!(!outside.join("assets").exists());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
