//! The artefact runtime (React, Tailwind's classes, Mermaid), built into
//! the binary and served on every file tab's host under
//! `/__forge/runtime/<version>/`, so artefacts work offline.
//!
//! Embedded rather than loaded from a folder beside the binary: about 6 MB
//! on a binary of over 40, and one file to ship keeps the updater simple.
//! The version in the path is a hash of the files, so the year long cache
//! the files are served with never outlives a Horadric update.

use std::sync::OnceLock;

use super::serve::Reply;

const RUNTIME_JS: &[u8] = include_bytes!("../../assets/forge/runtime/runtime.js");
const RUNTIME_CSS: &[u8] = include_bytes!("../../assets/forge/runtime/runtime.css");
const MERMAID_JS: &[u8] = include_bytes!("../../assets/forge/runtime/mermaid.js");
const ARTEFACT_HTML: &str = include_str!("../../assets/forge/runtime/artefact.html");

/// Where the runtime's files are served, ending in a slash.
pub fn base() -> &'static str {
    static BASE: OnceLock<String> = OnceLock::new();
    BASE.get_or_init(|| {
        // FNV-1a, as the hosts use: fast enough for 6 MB once.
        let mut h: u32 = 0x811c_9dc5;
        for part in [RUNTIME_JS, RUNTIME_CSS, MERMAID_JS] {
            for &b in part {
                h ^= u32::from(b);
                h = h.wrapping_mul(0x0100_0193);
            }
        }
        format!("/__forge/runtime/{h:08x}/")
    })
}

/// The answer to `/__forge/runtime/<rest>`. Any version is served the
/// files this build has: a page from before an update still works.
pub fn asset(rest: &str) -> Option<Reply> {
    let name = rest.rsplit('/').next()?;
    let (body, ty): (&'static [u8], &'static str) = match name {
        "runtime.js" => (RUNTIME_JS, "text/javascript; charset=utf-8"),
        "mermaid.js" => (MERMAID_JS, "text/javascript; charset=utf-8"),
        "runtime.css" => (RUNTIME_CSS, "text/css; charset=utf-8"),
        "artefact.html" => (ARTEFACT_HTML.as_bytes(), "text/html; charset=utf-8"),
        _ => return None,
    };
    Some(Reply::ok(body.into(), ty).cached())
}

/// The page for a `.jsx`, `.tsx` or `.mmd` file, served at the file's own
/// address so the tab stays the file's. `path` is that address's path,
/// as requested; the page fetches the source from it with `?raw`.
pub fn artefact_page(path: &str) -> String {
    let src = format!("{path}?raw");
    let head = format!(
        "<head>\n<base href=\"{}\">\n<meta name=\"forge-src\" content=\"{}\">",
        base(),
        attr(&src)
    );
    ARTEFACT_HTML.replacen("<head>", &head, 1)
}

/// Rendered markdown with Mermaid drawn in it, when it holds a diagram.
/// Only then is the 4 MB of Mermaid loaded.
pub fn with_mermaid(html: String) -> String {
    if !html.contains("<pre class=\"mermaid\"") {
        return html;
    }
    let scripts = format!(
        "<script src=\"{}mermaid.js\"></script>\n<script>{MERMAID_LOOK}\
         window.__forgeMermaid.render(document.body, function (e) {{\
         if (window.__forge && window.__forge.report) window.__forge.report(e);\
         }});</script>\n",
        base()
    );
    match html.rfind("</body>") {
        Some(at) => {
            let mut out = html;
            out.insert_str(at, &scripts);
            out
        }
        None => html + &scripts,
    }
}

/// The script that sets Mermaid's look, for a page that loads Mermaid
/// after it was made (the markdown editor drawing a new diagram).
pub fn mermaid_look() -> &'static str {
    MERMAID_LOOK
}

/// Mermaid drawn in the page's Horadric look rather than its own light
/// default: keys' faces for nodes, the project's colour round them, the
/// legends' ink for lines. Read from the page's `--hd-` properties, so a
/// diagram follows the theme as the page does.
const MERMAID_LOOK: &str = r##"(function () {
  var s = getComputedStyle(document.documentElement);
  var d = { screen: "#060709", plate: "#151619", surface: "#1b1d21", text: "#e8e9ed",
    read: "#d4d5d9", dim: "#8f939e", legend: "#6e727c", accent: "#a78bfa" };
  var v = function (n) { return s.getPropertyValue("--hd-" + n).trim() || d[n]; };
  window.__forgeMermaid.mermaid.initialize({ startOnLoad: false, securityLevel: "strict",
    layout: "dagre", theme: "base", themeVariables: { darkMode: true,
    fontFamily: '"Segoe UI Variable Text", "Segoe UI", sans-serif', fontSize: "14px",
    background: v("screen"), mainBkg: v("surface"), primaryColor: v("surface"),
    primaryTextColor: v("text"), primaryBorderColor: v("accent"), nodeBorder: v("accent"),
    secondaryColor: v("plate"), secondaryTextColor: v("read"), secondaryBorderColor: v("legend"),
    tertiaryColor: v("plate"), tertiaryTextColor: v("read"), tertiaryBorderColor: v("legend"),
    lineColor: v("dim"), textColor: v("read"), titleColor: v("text"),
    clusterBkg: v("plate"), clusterBorder: v("legend"), edgeLabelBackground: v("screen"),
    noteBkgColor: v("surface"), noteTextColor: v("read"), noteBorderColor: v("legend"),
    actorBkg: v("surface"), actorBorder: v("accent"), actorTextColor: v("text"),
    actorLineColor: v("legend"), signalColor: v("dim"), signalTextColor: v("read"),
    labelBoxBkgColor: v("surface"), labelBoxBorderColor: v("legend"), labelTextColor: v("text"),
    loopTextColor: v("read") } });
})();
"##;

/// Text made safe for a double quoted attribute.
pub fn attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_artefact_page_loads_the_runtime_and_its_file() {
        let page = artefact_page("/reports/my%20chart.jsx");
        assert!(page.contains(&format!("<base href=\"{}\">", base())));
        assert!(page.contains("content=\"/reports/my%20chart.jsx?raw\""));
        // Relative to the base, so the runtime comes from the reserved path.
        assert!(page.contains("<script src=\"runtime.js\">"));
        assert!(base().starts_with("/__forge/runtime/") && base().ends_with('/'));
    }

    #[test]
    fn the_runtime_files_are_served_with_their_types() {
        let js = asset("0000/runtime.js").unwrap();
        assert!(js.content_type.starts_with("text/javascript"));
        assert!(
            matches!(&js.body, super::super::serve::Body::Bytes(b) if b.len() == RUNTIME_JS.len())
        );
        assert!(asset("0000/mermaid.js").is_some());
        assert!(asset("0000/runtime.css")
            .unwrap()
            .content_type
            .starts_with("text/css"));
        assert!(asset("0000/secret.txt").is_none());
    }

    #[test]
    fn mermaid_is_loaded_only_for_a_diagram() {
        let plain = "<html><body><p>hi</p></body></html>".to_string();
        assert_eq!(with_mermaid(plain.clone()), plain);
        let md =
            horadric_core::forge::markdown_html("```mermaid\ngraph TD\n  A-->B\n```\n", "d.md");
        let out = with_mermaid(md);
        let script = out.find("mermaid.js\"></script>").unwrap();
        assert!(script < out.rfind("</body>").unwrap());
        assert!(out.contains("__forgeMermaid.render"));
    }
}
