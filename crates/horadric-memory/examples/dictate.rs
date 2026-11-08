//! Transcribes WAV files with a dictation model, to try it without the
//! app: `cargo run --release -p horadric-memory --example dictate --
//! <models dir> <model folder>[:<language>] <file.wav>...`, the model
//! one of `speech::Model`'s folders (distil-small.en, distil-large-v3,
//! whisper-small, whisper-large-v3-turbo). Each file is heard twice and
//! the second time is the one told, so the first run's warming up is not
//! counted. The files are 16 bit mono at 16 kHz, as Windows' Sound
//! Recorder or `System.Speech` can write them.

use std::time::Instant;

use horadric_memory::speech::{Listener, Model};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [models, model, wavs @ ..] = args.as_slice() else {
        eprintln!("usage: dictate <models dir> <model>[:<language>] <file.wav>...");
        std::process::exit(2);
    };
    let (model, language) = model.split_once(':').unwrap_or((model, "en"));
    let model = Model::from_folder(model).expect("a model's folder");
    let dir = model.ensure(models.as_ref(), curl).expect("fetch the model");
    let at = Instant::now();
    let mut listener = Listener::load(&dir).expect("load");
    listener.set_language(language).expect("language");
    println!("{} loaded in {:?}", model.folder(), at.elapsed());
    for wav in wavs {
        let pcm = read_wav(&std::fs::read(wav).expect("read the wav"));
        listener.transcribe(&pcm, "").expect("transcribe");
        let at = Instant::now();
        let text = listener.transcribe(&pcm, "").expect("transcribe");
        println!(
            "{wav}: {:.1} s of sound in {:.2} s: {text}",
            pcm.len() as f32 / 16000.0,
            at.elapsed().as_secs_f32()
        );
    }
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
                .as_chunks::<2>()
                .0
                .iter()
                .map(|s| i16::from_le_bytes(*s) as f32 / 32768.0)
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
