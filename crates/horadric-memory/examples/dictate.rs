//! Transcribes a WAV file with the dictation model, to try it without the
//! app: `cargo run --release -p horadric-memory --example dictate -- <models dir> <file.wav>`.
//! The file is 16 bit mono at 16 kHz, as Windows' Sound Recorder or
//! `System.Speech` can write it.

use std::time::Instant;

use horadric_memory::speech::{self, Listener};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [models, wav] = args.as_slice() else {
        eprintln!("usage: dictate <models dir> <file.wav>");
        std::process::exit(2);
    };
    let pcm = read_wav(&std::fs::read(wav).expect("read the wav"));
    let dir = speech::ensure(models.as_ref(), curl).expect("fetch the model");
    let at = Instant::now();
    let mut listener = Listener::load(&dir).expect("load");
    println!("loaded in {:?}", at.elapsed());
    let at = Instant::now();
    let text = listener.transcribe(&pcm).expect("transcribe");
    println!(
        "{:.1} s of sound in {:?}: {text}",
        pcm.len() as f32 / 16000.0,
        at.elapsed()
    );
}

/// The samples of a 16 bit PCM WAV, found by its `data` chunk.
fn read_wav(bytes: &[u8]) -> Vec<f32> {
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        if id == b"data" {
            let end = (at + 8 + len).min(bytes.len());
            return bytes[at + 8..end]
                .chunks_exact(2)
                .map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0)
                .collect();
        }
        at += 8 + len + (len & 1);
    }
    panic!("no data chunk");
}

fn curl(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    println!("fetching {url}");
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "--max-filesize", &limit.to_string(), url])
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "curl failed on {url}: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(out.stdout)
}
