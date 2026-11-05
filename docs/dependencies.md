# Dependencies

Each one is justified on its own. Adding one is a decision, not a reflex.

Today: `serde`, `serde_json`,
`windows`, `windows-numerics`, `alacritty_terminal` for the terminal
grid (writing a VT parser is not the project), `syntect` for the
file viewer's colours (writing grammars is not either),
`webview2-com` for the browser pane (the WebView2 COM bindings, on the
same `windows` version), and `tree-sitter` with one grammar crate per
language (Rust, TypeScript and JavaScript, Python) for the lore's code
index (parsing languages is not the project either, and it finds the
calls a regex outline misses), and in `horadric-memory` `candle-core`,
`candle-nn` and `candle-transformers` with `tokenizers` (its `onig`
feature only) to run the embedding model that finds memory by meaning, and Whisper
for dictation (decided 2026-10-30, no new crate), with `rayon`, which
candle already runs on, named so the embedding gets a small pool of its
own at low priority instead of a thread per core (decided 2026-10-30):
Hugging Face's pure Rust ML crates, no DLL to ship and no service that
must be running, and the tokenizer is the one the models were trained
with (writing either is not the project), and in `horadric-link`, for
a Linux node and the Mac on the relay, `ring`, `p256`, `rustls`, `webpki-roots`
and `tungstenite`: the AES-GCM, ECDH, TLS and WebSocket that Windows
has built in and Linux does not (writing crypto is not the project
either; the Mac has CommonCrypto but no ECDH a stored key loads into, and
one crate for both unix machines beats two, decided 2026-11-01).
`serde_json` is a plain dependency there since its relay link reads the
relay's frames. Windows keeps CNG and WinHTTP; the crate is used there only
by a test that holds the two to the same bytes. `p256` has its `ecdsa`
feature too, for the signatures a brother's word carries (decided
2026-10-25), which brings `ecdsa`, `rfc6979`, `sha2` and `signature`.
Not a crate but vendored: `conpty.dll` and `OpenConsole.exe` from
Microsoft's ConPTY package, the pseudo console that passes `CSI c`
through so OSC 7501 works (see
`crates/horadric-pty/conpty/README.md`). Adding one is a decision, not a reflex.

On macOS (`horadric-ui/src/mac/`) the UI uses `objc2` with `objc2-foundation`, `objc2-app-kit`, `objc2-core-foundation`, `objc2-core-graphics` and `objc2-core-text`, the Objective-C and AppKit bindings that stand in for the `windows` crate, and `objc2-web-kit` for the browser pane, WKWebView as `webview2-com` is WebView2 (the Mac side of the one web view decided 2026-09-29, no new decision; it brings `objc2-javascript-core` and `objc2-security`, WebKit's own bindings). `libc` is used in `horadric`, `horadric-pty` and `horadric-ui` for the Unix side of the pty and process code.

## The Forge's artefact runtime

Not crates: JavaScript bundled into `crates/horadric-ui/assets/forge/runtime/`
and served to the browser pane, so the Forge shows Claude's React
artefacts offline (decided 2026-10-30 in [the Forge](plan/the-forge.md), the
one place that plan adds dependencies). Built by `tools/forge-runtime/` with
esbuild (a build tool only, not shipped); versions pinned exactly there.
Sizes are minified bytes in the bundle.

- `react` and `react-dom` 18.3.1 (130 KB): what artefacts are written in. 18,
  not 19, because that is what claude.ai's artefacts run on.
- `sucrase` 3.35.1 (180 KB): compiles JSX and TypeScript in the page. A fifth
  of Babel's size, much faster, and it keeps line numbers so an error points
  at the artefact's own line. esbuild-wasm would be 10 MB.
- `@unocss/runtime`, `@unocss/core`, `@unocss/preset-wind3` 66.12.0 and
  `unocss-preset-animations` 1.3.0 (190 KB together): compiles Tailwind
  classes in the page as the DOM changes. Chosen over Tailwind 4's own
  browser build (290 KB) because Claude writes Tailwind 3 classes and
  preset-wind3 keeps their meanings (`bg-opacity-50`, `shadow`, `rounded`,
  border colours) where Tailwind 4 changed them; the animations preset is
  shadcn's `tailwindcss-animate`. `@unocss/reset` gives Tailwind 3's
  preflight.
- `lucide-react` 1.69.0 (830 KB): the icons artefacts use. All of them, with
  their old names, since any may be imported; the biggest single part.
- `recharts` 3.10.1 (450 KB with its d3 parts): the charts artefacts use.
- `radix-ui` 1.8.1 (190 KB for the parts used, with floating-ui for placing popups): the primitives under
  shadcn/ui's select, dialog, tooltip, tabs, accordion, slider and the
  rest. Real Radix, not light local stand ins, so keyboard, focus and
  `asChild` behave as the artefact expects. The shadcn components
  themselves are copied in as source (shadcn is copied, never installed).
- `class-variance-authority` 0.7.1, `clsx` 2.1.1 and `tailwind-merge` 2.6.1
  (25 KB): what shadcn's components and `cn` are built on, and artefacts
  import them too. tailwind-merge 2, not 3, because 3 speaks Tailwind 4.
- `mermaid` 12.3.0 (3.7 MB, its own `mermaid.js`, loaded only when a diagram
  is drawn): `.mmd` files and mermaid blocks in markdown. Without ELK
  (a further 1.5 MB), with dagre as the layout as claude.ai and GitHub draw.

Total 6 MB, 1.6 MB gzipped, of which mermaid is 3.7 MB.
