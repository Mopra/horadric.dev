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
for dictation (decided 2026-10-30, no new crate):
Hugging Face's pure Rust ML crates, no DLL to ship and no service that
must be running, and the tokenizer is the one the models were trained
with (writing either is not the project), and in `horadric-link`, for
a Linux node on the relay, `ring`, `p256`, `rustls`, `webpki-roots`
and `tungstenite`: the AES-GCM, ECDH, TLS and WebSocket that Windows
has built in and Linux does not (writing crypto is not the project
either). Windows keeps CNG and WinHTTP; the crate is used there only
by a test that holds the two to the same bytes. `p256` has its `ecdsa`
feature too, for the signatures a brother's word carries (decided
2026-10-25), which brings `ecdsa`, `rfc6979`, `sha2` and `signature`.
Not a crate but vendored: `conpty.dll` and `OpenConsole.exe` from
Microsoft's ConPTY package, the pseudo console that passes `CSI c`
through so OSC 7501 works (see
`crates/horadric-pty/conpty/README.md`). Adding one is a decision, not a reflex.

On macOS (`horadric-ui/src/mac/`) the UI uses `objc2` with `objc2-foundation`, `objc2-app-kit`, `objc2-core-foundation`, `objc2-core-graphics` and `objc2-core-text`, the Objective-C and AppKit bindings that stand in for the `windows` crate. `libc` is used in `horadric`, `horadric-pty` and `horadric-ui` for the Unix side of the pty and process code.
