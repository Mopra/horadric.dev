//! Dictation: Ctrl+Space in a stage pane listens to the microphone, and
//! what was said is typed where the keyboard is, the Folio's composer or
//! the session's prompt. Nothing is sent: the human reads it and presses
//! Enter.
//!
//! Tapped, Ctrl+Space listens until it is pressed again. Held, it listens
//! until it is let go, like a walkie talkie. Esc throws away what was
//! heard. Fifteen seconds without a word ends it too. Whisper runs on this
//! machine ([`horadric_memory::speech`]), so the words never leave it.
//!
//! The words show while they are said, dimmed until they are final: in
//! the Folio's composer at the caret, in a session's header otherwise.
//! Whisper does not stream, so the speech is cut into phrases at each
//! pause. A phrase that ended is written down for good while the next is
//! spoken, and the phrase being spoken is written again every moment for
//! the draft. When the key is let go only the last phrase is left to
//! write, which is why the words come quickly however long the dictation.
//!
//! The first press on a machine without the model fetches it instead,
//! 332 MB with a bar counting up, and says when it is ready: speech
//! recorded then would wait minutes to be written down, or be lost if the
//! fetch failed.
//!
//! Three threads: the UI's, which owns the state below; one per recording,
//! which owns the microphone and cuts the phrases; and one that keeps the
//! model loaded between dictations and writes down what each recording
//! heard. The last two reach the pane with [`WM_DICTATE`].

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use horadric_memory::speech::{self, Listener};
use windows::core::{GUID, HSTRING};
use windows::Win32::Foundation::{HWND, LPARAM, PROPERTYKEY, WPARAM};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
    DEVICE_STATE_ACTIVE, WAVEFORMATEX,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_USER};

/// Posted to the pane that is dictating when what it shows changes, and
/// when the words are ready to [`take`].
pub const WM_DICTATE: u32 = WM_USER + 11;

/// Held at least this long, letting go of Ctrl+Space ends the dictation.
/// A shorter press was a tap, and the next tap ends it.
const HOLD: Duration = Duration::from_millis(400);

/// A recording stops by itself after this, in case the human walked away.
const LONGEST: Duration = Duration::from_secs(180);

/// What WASAPI is asked for: Whisper's own rate, mono, as floats. Shared
/// mode converts whatever the microphone gives, so nothing is resampled
/// here.
const RATE: u32 = speech::SAMPLE_RATE as u32;

/// Samples in `ms` milliseconds.
const fn ms(ms: usize) -> usize {
    speech::SAMPLE_RATE * ms / 1000
}

/// A quiet this long after speech ends a phrase. Shorter cuts words that
/// trail off; longer leaves more to write when the key is let go.
const PAUSE: usize = ms(600);

/// Of the quiet that ended a phrase, this much stays with it, so its last
/// word is not clipped.
const TAIL: usize = ms(250);

/// Quiet before a phrase is kept up to this, so its first word is whole.
const LEAD: usize = ms(400);

/// A phrase spoken without a pause is cut here anyway: Whisper reads 30
/// seconds at most, and the draft is written again from its start.
const LONGEST_PHRASE: usize = ms(20_000);

/// No word for this long ends the dictation, as Claude Code's does.
const HUSH: usize = ms(15_000);

/// How often the phrase being spoken is written again for the draft, at
/// most: the writer skips a turn while it is still on the last one.
const DRAFT_EVERY: Duration = Duration::from_millis(500);

/// Below this RMS nothing at all came in: a muted microphone or a line in
/// with nothing plugged into it.
const NO_SOUND: f32 = 0.000_3;

/// Speech is at least this loud, and well above the room's own noise.
const VOICE: f32 = 0.006;

/// How many times the room's noise a stretch must be to count as speech.
const OVER_FLOOR: f32 = 3.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Listening,
    /// The model is being fetched or loaded before it can write.
    Fetching,
    Writing,
}

struct Now {
    pane: isize,
    /// Which dictation, so a late draft of the last one is not shown.
    id: u64,
    phase: Phase,
    since: Instant,
    /// Set to end the recording; `keep` says whether what was heard is
    /// written down or thrown away.
    stop: Arc<AtomicBool>,
    keep: Arc<AtomicBool>,
    /// The words so far, the final and the draft, shown dimmed.
    draft: String,
}

/// How long a failure, or word that the model is ready, stays shown.
const AFTER_FOR: Duration = Duration::from_secs(8);

const READY: &str = "Dictation is ready. Press Ctrl+Space and talk.";

const TO_STOP: &str = "Listening. Ctrl+Space to stop, Esc to drop it.";

/// The microphone picked in Settings, by its endpoint id. None, or one
/// unplugged since, is the one Windows records from by default.
static MIC: Mutex<Option<String>> = Mutex::new(None);

thread_local! {
    static NOW: RefCell<Option<Now>> = const { RefCell::new(None) };
    /// What the last dictation left to say, by pane, until the next one
    /// starts: why it failed, or that the model is ready.
    static AFTER: RefCell<Option<(isize, String, Instant)>> = const { RefCell::new(None) };
}

/// Bytes of the model on disk while it is fetched, for the bar.
static FETCHED: AtomicUsize = AtomicUsize::new(0);

/// How loud the microphone is now, 0 to 1, as f32 bits.
static LEVEL: AtomicU32 = AtomicU32::new(0);

/// The writer is on a draft, so the recording sends none until it is done.
static DRAFTING: AtomicBool = AtomicBool::new(false);

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// What the background threads tell the UI, by pane.
enum Note {
    Phase(Phase),
    /// The model was fetched and loaded on the first press.
    Ready,
    /// The words of dictation `id` so far.
    Draft(u64, String),
    Done(Result<String, String>),
}

/// What a pane dictating shows beyond words: a lit microphone, a bar.
#[derive(Clone, Copy, PartialEq)]
pub enum Shown {
    /// How loud the microphone is, 0 to 1.
    Listening(f32),
    /// How much of the model is here, 0 to 1. At 1 it is being loaded.
    Fetching(f32),
    Writing,
}

enum Job {
    /// Fetch and load the model, nothing to write yet.
    Prepare(isize),
    /// A dictation began: load the model while the human talks, and keep
    /// the names its speech may use.
    Begin { id: u64, pane: isize, hints: String },
    /// The phrase being spoken so far, written for the draft.
    Draft { id: u64, pane: isize, pcm: Vec<f32> },
    /// A phrase that ended, written for good. The last one finishes the
    /// dictation, with why nothing was heard should nothing have been.
    Phrase {
        id: u64,
        pane: isize,
        pcm: Vec<f32>,
        last: Option<Option<String>>,
    },
    /// Esc: what was written of `id` is thrown away.
    Drop(u64),
}

static NOTES: Mutex<Vec<(isize, Note)>> = Mutex::new(Vec::new());

fn note(pane: isize, n: Note) {
    if let Ok(mut q) = NOTES.lock() {
        q.push((pane, n));
    }
    poke(pane);
}

fn poke(pane: isize) {
    unsafe {
        let _ = PostMessageW(
            Some(HWND(pane as *mut c_void)),
            WM_DICTATE,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

fn models() -> Option<PathBuf> {
    Some(horadric_hooks::state_dir()?.join("models"))
}

/// A recording device plugged in now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Microphone {
    pub id: String,
    pub name: String,
}

/// Dictation listens to the microphone `id` from now on, None for
/// Windows' default.
pub fn use_microphone(id: Option<String>) {
    if let Ok(mut m) = MIC.lock() {
        *m = id;
    }
}

/// The microphones plugged in, and which of them Windows records from by
/// default. On a thread of its own, since COM there is ours to set up
/// whatever the caller's thread has done with it.
pub fn microphones() -> (Vec<Microphone>, Option<String>) {
    thread::spawn(|| unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let found = list_microphones().unwrap_or_default();
        CoUninitialize();
        found
    })
    .join()
    .unwrap_or_default()
}

unsafe fn list_microphones() -> windows::core::Result<(Vec<Microphone>, Option<String>)> {
    let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let all = devices.EnumAudioEndpoints(eCapture, DEVICE_STATE_ACTIVE)?;
    let mut mics = Vec::new();
    for i in 0..all.GetCount()? {
        let device = all.Item(i)?;
        if let Some(id) = device_id(&device) {
            mics.push(Microphone {
                name: device_name(&device).unwrap_or_else(|| id.clone()),
                id,
            });
        }
    }
    let default = devices
        .GetDefaultAudioEndpoint(eCapture, eConsole)
        .ok()
        .and_then(|d| device_id(&d));
    Ok((mics, default))
}

unsafe fn device_id(device: &IMMDevice) -> Option<String> {
    let id = device.GetId().ok()?;
    let s = id.to_string().ok();
    CoTaskMemFree(Some(id.0 as *const c_void));
    s
}

/// Its name as Windows' Sound settings show it, "Microphone (StreamCam)".
unsafe fn device_name(device: &IMMDevice) -> Option<String> {
    // PKEY_Device_FriendlyName.
    let key = PROPERTYKEY {
        fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
        pid: 14,
    };
    let value = device.OpenPropertyStore(STGM_READ).ok()?.GetValue(&key).ok()?;
    let text = PropVariantToStringAlloc(&value).ok()?;
    let s = text.to_string().ok();
    CoTaskMemFree(Some(text.0 as *const c_void));
    s.filter(|s| !s.is_empty())
}

/// The microphone picked if it is still plugged in, else Windows' default.
unsafe fn microphone(devices: &IMMDeviceEnumerator) -> windows::core::Result<IMMDevice> {
    let picked = MIC.lock().ok().and_then(|m| m.clone());
    let plugged = picked
        .and_then(|id| devices.GetDevice(&HSTRING::from(id)).ok())
        .filter(|d| d.GetState().is_ok_and(|s| s == DEVICE_STATE_ACTIVE));
    match plugged {
        Some(d) => Ok(d),
        None => devices.GetDefaultAudioEndpoint(eCapture, eConsole),
    }
}

/// Ctrl+Space went down in `pane`, or its microphone was clicked. `dir`
/// is the project's folder and `before` the text ahead of the caret, for
/// Whisper to go on from.
pub fn press(pane: HWND, dir: Option<&Path>, before: &str) {
    let busy = NOW.with(|n| {
        let mut n = n.borrow_mut();
        match n.as_ref() {
            Some(now) if now.phase == Phase::Listening => {
                now.stop.store(true, Ordering::Release);
                true
            }
            Some(_) => true,
            None => {
                AFTER.with(|f| f.borrow_mut().take());
                let here = models().is_some_and(|m| speech::is_fetched(&m));
                *n = Some(if here {
                    start(pane.0 as isize, hints(dir, before))
                } else {
                    prepare(pane.0 as isize)
                });
                false
            }
        }
    });
    if !busy {
        changed(pane);
    }
}

/// Ctrl+Space, or Space, came up: a hold ends here.
pub fn release(pane: HWND) {
    NOW.with(|n| {
        if let Some(now) = n.borrow().as_ref() {
            if now.pane == pane.0 as isize
                && now.phase == Phase::Listening
                && now.since.elapsed() >= HOLD
            {
                now.stop.store(true, Ordering::Release);
            }
        }
    });
}

/// Esc while listening or writing: what was heard is dropped. False when
/// `pane` is not dictating, so Esc does what it always does.
pub fn cancel(pane: HWND) -> bool {
    NOW.with(|n| {
        let mut n = n.borrow_mut();
        let Some(now) = n.as_ref() else { return false };
        if now.pane != pane.0 as isize || now.phase == Phase::Fetching {
            return false;
        }
        now.keep.store(false, Ordering::Release);
        now.stop.store(true, Ordering::Release);
        let _ = writer().send(Job::Drop(now.id));
        *n = None;
        true
    })
}

/// The words heard so far in `pane`, not yet typed, for the composer to
/// show dimmed at its caret. None when it is not dictating or nothing
/// has been made out yet.
pub fn draft(pane: HWND) -> Option<String> {
    NOW.with(|n| {
        n.borrow()
            .as_ref()
            .filter(|now| now.pane == pane.0 as isize && !now.draft.is_empty())
            .map(|now| now.draft.clone())
    })
}

/// What `pane`'s header shows while it dictates, where its detail would
/// be: how loud the microphone is, and the end of the words so far.
pub fn status(pane: HWND) -> Option<String> {
    let said = draft(pane).map(|d| tail(&d, 90));
    match shown(pane) {
        Some(Shown::Listening(level)) => Some(match said {
            Some(said) => format!("{} {said}", meter(level)),
            None => format!("{} {TO_STOP}", meter(level)),
        }),
        Some(Shown::Writing) => Some(match said {
            Some(said) => format!("Writing it down: {said}"),
            None => "Writing it down.".to_string(),
        }),
        _ => notice(pane),
    }
}

/// What the Folio says above its composer while `pane` dictates. The
/// words themselves are in the composer.
pub fn notice(pane: HWND) -> Option<String> {
    let after = AFTER.with(|f| {
        f.borrow()
            .as_ref()
            .filter(|(p, _, at)| *p == pane.0 as isize && at.elapsed() < AFTER_FOR)
            .map(|(_, e, _)| e.clone())
    });
    let Some(shown) = shown(pane) else {
        return after;
    };
    Some(match shown {
        Shown::Listening(_) => TO_STOP.to_string(),
        Shown::Fetching(f) if f < 1.0 => {
            let mb = speech::download_size() / 1_000_000;
            format!(
                "Dictation needs its speech model first, downloaded once: {} of {mb} MB.",
                (f * mb as f32) as usize
            )
        }
        Shown::Fetching(_) => "Loading the speech model.".to_string(),
        Shown::Writing => "Writing it down.".to_string(),
    })
}

/// A bar of block letters as tall as `level`, for a header that has only
/// text to show it with.
fn meter(level: f32) -> char {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    BARS[((level * BARS.len() as f32) as usize).min(BARS.len() - 1)]
}

/// The last `most` letters of `s` or so, from a word's start, so the words
/// being said stay in sight in a line too short for all of them.
fn tail(s: &str, most: usize) -> String {
    let count = s.chars().count();
    if count <= most {
        return s.to_string();
    }
    let cut: String = s.chars().skip(count - most).collect();
    let from = cut.find(' ').map_or(0, |i| i + 1);
    format!("…{}", &cut[from..])
}

/// Where `pane` is in a dictation, None when it is not dictating.
pub fn shown(pane: HWND) -> Option<Shown> {
    let phase = NOW.with(|n| {
        n.borrow()
            .as_ref()
            .filter(|now| now.pane == pane.0 as isize)
            .map(|now| now.phase)
    })?;
    Some(match phase {
        Phase::Listening => Shown::Listening(f32::from_bits(LEVEL.load(Ordering::Relaxed))),
        Phase::Fetching => {
            let total = speech::download_size().max(1);
            Shown::Fetching((FETCHED.load(Ordering::Relaxed) as f32 / total as f32).min(1.0))
        }
        Phase::Writing => Shown::Writing,
    })
}

/// On [`WM_DICTATE`]: the words heard in `pane`, once they are ready.
/// Everything else posted to it is applied to the state here.
pub fn take(pane: HWND) -> Option<Result<String, String>> {
    let notes: Vec<Note> = {
        let mut q = NOTES.lock().ok()?;
        let (mine, rest) = q.drain(..).partition(|(p, _)| *p == pane.0 as isize);
        *q = rest;
        mine.into_iter().map(|(_, n)| n).collect()
    };
    let mut done = None;
    for n in notes {
        match n {
            Note::Phase(p) => NOW.with(|n| {
                if let Some(now) = n.borrow_mut().as_mut() {
                    if now.pane == pane.0 as isize {
                        now.phase = p;
                    }
                }
            }),
            Note::Draft(id, text) => NOW.with(|n| {
                if let Some(now) = n.borrow_mut().as_mut() {
                    if now.pane == pane.0 as isize && now.id == id {
                        now.draft = text;
                    }
                }
            }),
            Note::Ready => {
                end(pane);
                linger(pane, READY.to_string());
            }
            Note::Done(r) => {
                end(pane);
                if let Err(e) = &r {
                    linger(pane, e.clone());
                }
                done = Some(r);
            }
        }
    }
    done
}

fn end(pane: HWND) {
    NOW.with(|n| {
        let mut n = n.borrow_mut();
        if n.as_ref().is_some_and(|now| now.pane == pane.0 as isize) {
            *n = None;
        }
    });
}

/// Shows `said` in `pane` for a while, and has it looked at again once
/// that is over so it goes.
fn linger(pane: HWND, said: String) {
    let p = pane.0 as isize;
    AFTER.with(|f| *f.borrow_mut() = Some((p, said, Instant::now())));
    thread::spawn(move || {
        thread::sleep(AFTER_FOR + Duration::from_millis(100));
        poke(p);
    });
}

fn changed(pane: HWND) {
    unsafe {
        let _ = PostMessageW(Some(pane), WM_DICTATE, WPARAM(0), LPARAM(0));
    }
}

/// The words Whisper is told come before the speech: Claude, the
/// project's name and branch, and the end of what is already typed. Kept
/// short: given a paragraph, distil-whisper loops or drops a sentence, so
/// Claude's last reply is not in it, much as it would help.
fn hints(dir: Option<&Path>, before: &str) -> String {
    let mut out = vec!["Claude".to_string()];
    if let Some(dir) = dir {
        if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
            out.push(name.to_string());
        }
        if let Some(branch) = branch(dir) {
            out.push(branch);
        }
    }
    let mut said = format!("{}.", out.join(", "));
    let before = tail(before.trim(), 100);
    let before = before.trim_start_matches('…');
    if !before.is_empty() {
        said.push(' ');
        said.push_str(before);
    }
    said
}

/// The branch checked out in `dir`, read from `.git/HEAD` so no git
/// starts for it. None in a worktree, where `.git` is a file.
fn branch(dir: &Path) -> Option<String> {
    let head = std::fs::read_to_string(dir.join(".git").join("HEAD")).ok()?;
    let name = head.trim().strip_prefix("ref: refs/heads/")?;
    Some(name.replace(['-', '_', '/'], " "))
}

fn start(pane: isize, hints: String) -> Now {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let stop = Arc::new(AtomicBool::new(false));
    let keep = Arc::new(AtomicBool::new(true));
    let (s, k) = (stop.clone(), keep.clone());
    LEVEL.store(0, Ordering::Relaxed);
    let _ = writer().send(Job::Begin { id, pane, hints });
    thread::spawn(move || listen(id, pane, &s, &k));
    Now {
        pane,
        id,
        phase: Phase::Listening,
        since: Instant::now(),
        stop,
        keep,
        draft: String::new(),
    }
}

/// The recording thread: the microphone until `stop`, each phrase sent
/// to the writer as it ends and the one being spoken now and then for
/// the draft.
fn listen(id: u64, pane: isize, stop: &AtomicBool, keep: &AtomicBool) {
    let mut pcm: Vec<f32> = Vec::with_capacity(RATE as usize * 30);
    let mut ear = Ear::default();
    let mut drafted = (Instant::now(), 0usize);
    let mut shown = 0usize;
    let send = |job: Job| {
        let _ = writer().send(job);
    };
    let heard = record(stop, &mut |chunk: &[f32]| {
        let at = pcm.len();
        pcm.extend_from_slice(chunk);
        if let Some(phrase) = ear.hear(at, chunk) {
            send(Job::Phrase {
                id,
                pane,
                pcm: pcm[phrase].to_vec(),
                last: None,
            });
        }
        // Speech sits around 0.05 to 0.2; the square root spreads it over
        // the bar rather than leaving it at the bottom.
        let level = (speech::loudness(chunk) * 8.0).sqrt().min(1.0);
        LEVEL.store(level.to_bits(), Ordering::Relaxed);
        let step = (level * 8.0) as usize;
        if step != shown {
            shown = step;
            poke(pane);
        }
        let end = pcm.len();
        if ear.voiced
            && end > drafted.1
            && drafted.0.elapsed() >= DRAFT_EVERY
            && !DRAFTING.swap(true, Ordering::AcqRel)
        {
            drafted = (Instant::now(), end);
            send(Job::Draft {
                id,
                pane,
                pcm: pcm[ear.start..end].to_vec(),
            });
        }
        ear.hushed(end)
    });
    LEVEL.store(0, Ordering::Relaxed);
    if !keep.load(Ordering::Acquire) {
        return;
    }
    match heard {
        Ok(mic) => {
            note(pane, Note::Phase(Phase::Writing));
            let rest = ear.rest(pcm.len());
            let why = ear.why_nothing(&mic);
            send(Job::Phrase {
                id,
                pane,
                pcm: if ear.spoke { pcm[rest].to_vec() } else { Vec::new() },
                last: Some(why),
            });
        }
        Err(e) => {
            send(Job::Drop(id));
            note(pane, Note::Done(Err(e)));
        }
    }
}

/// Cuts a recording into phrases at its pauses, and tells speech from the
/// room's noise. Pure, so it is tested without a microphone.
#[derive(Default)]
struct Ear {
    /// The room's noise, the RMS of its quietest stretches lately.
    floor: Option<f32>,
    /// Where the phrase being heard starts, in samples.
    start: usize,
    /// Speech was heard in that phrase.
    voiced: bool,
    /// Where speech was last heard.
    last_voice: usize,
    /// Speech was heard at all, and the loudest any stretch was.
    spoke: bool,
    peak: f32,
}

impl Ear {
    /// `chunk` came in, starting at sample `at`: the phrase it ended, if
    /// it ended one.
    fn hear(&mut self, at: usize, chunk: &[f32]) -> Option<Range<usize>> {
        if chunk.is_empty() {
            return None;
        }
        let rms = speech::loudness(chunk);
        self.peak = self.peak.max(rms);
        // Falls at once to a quieter stretch, rises slowly, so speech
        // never lifts it far.
        let floor = match self.floor {
            Some(f) if rms > f => f + (rms - f) * 0.002,
            _ => rms,
        };
        self.floor = Some(floor);
        let end = at + chunk.len();
        if rms > VOICE.max(floor * OVER_FLOOR) {
            self.voiced = true;
            self.spoke = true;
            self.last_voice = end;
            if end - self.start >= LONGEST_PHRASE {
                return Some(self.cut(end));
            }
            return None;
        }
        if !self.voiced {
            // Silence before a phrase is not worth writing.
            self.start = self.start.max(end.saturating_sub(LEAD));
            return None;
        }
        (end - self.last_voice >= PAUSE).then(|| self.cut((self.last_voice + TAIL).min(end)))
    }

    fn cut(&mut self, end: usize) -> Range<usize> {
        let phrase = self.start..end;
        self.start = end;
        self.voiced = false;
        phrase
    }

    /// The phrase still being heard when the recording stopped at `end`.
    fn rest(&self, end: usize) -> Range<usize> {
        self.start.min(end)..end
    }

    /// No word for [`HUSH`] at sample `end`: the human is done, or never
    /// began.
    fn hushed(&self, end: usize) -> bool {
        end.saturating_sub(self.last_voice) >= HUSH
    }

    /// Why nothing will be written, when nothing was said into `mic`.
    fn why_nothing(&self, mic: &str) -> Option<String> {
        if self.spoke {
            None
        } else if self.peak < NO_SOUND {
            Some(format!(
                "No sound came from {mic}. Pick your microphone in Settings, under General."
            ))
        } else {
            Some(format!(
                "Heard no speech from {mic}. Speak closer, or pick another microphone in Settings, under General."
            ))
        }
    }
}

fn prepare(pane: isize) -> Now {
    let _ = writer().send(Job::Prepare(pane));
    Now {
        pane,
        id: 0,
        phase: Phase::Fetching,
        since: Instant::now(),
        stop: Arc::new(AtomicBool::new(false)),
        keep: Arc::new(AtomicBool::new(false)),
        draft: String::new(),
    }
}

/// What the writer keeps of a dictation while it is spoken.
#[derive(Default)]
struct Take {
    hints: String,
    /// The phrases written for good.
    said: String,
}

impl Take {
    /// What Whisper is told came before the next phrase: the hints and
    /// the last phrase or so, which keeps the sentence going across a cut.
    fn prompt(&self) -> String {
        format!("{} {}", self.hints, tail(&self.said, 100).trim_start_matches('…'))
    }

    fn with(&self, more: &str) -> String {
        join(&self.said, more)
    }
}

fn join(a: &str, b: &str) -> String {
    match (a.trim(), b.trim()) {
        ("", b) => b.to_string(),
        (a, "") => a.to_string(),
        (a, b) => format!("{a} {b}"),
    }
}

/// The thread that keeps the model and writes down each recording.
fn writer() -> &'static Sender<Job> {
    static WRITER: OnceLock<Sender<Job>> = OnceLock::new();
    WRITER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        thread::spawn(move || {
            let mut listener: Option<Listener> = None;
            let mut takes: HashMap<u64, Take> = HashMap::new();
            for job in rx {
                let pane = match &job {
                    Job::Prepare(p)
                    | Job::Begin { pane: p, .. }
                    | Job::Draft { pane: p, .. }
                    | Job::Phrase { pane: p, .. } => *p,
                    Job::Drop(id) => {
                        takes.remove(id);
                        continue;
                    }
                };
                if let Job::Begin { id, hints, .. } = &job {
                    takes.insert(
                        *id,
                        Take {
                            hints: hints.clone(),
                            said: String::new(),
                        },
                    );
                }
                if listener.is_none() {
                    match load(pane) {
                        Ok(l) => listener = Some(l),
                        Err(e) => {
                            if let Job::Begin { id, .. } | Job::Phrase { id, .. } = &job {
                                takes.remove(id);
                            }
                            DRAFTING.store(false, Ordering::Release);
                            note(pane, Note::Done(Err(e)));
                            continue;
                        }
                    }
                }
                let Some(l) = listener.as_mut() else { continue };
                match job {
                    Job::Prepare(_) => note(pane, Note::Ready),
                    Job::Begin { .. } => {}
                    Job::Draft { id, pcm, .. } => {
                        if let Some(take) = takes.get(&id) {
                            if let Ok(text) = l.transcribe(&pcm, &take.prompt()) {
                                note(pane, Note::Draft(id, take.with(&text)));
                            }
                        }
                        DRAFTING.store(false, Ordering::Release);
                    }
                    Job::Phrase { id, pcm, last, .. } => {
                        let Some(take) = takes.get_mut(&id) else {
                            continue;
                        };
                        let text = if pcm.is_empty() {
                            Ok(String::new())
                        } else {
                            l.transcribe(&pcm, &take.prompt())
                        };
                        match text {
                            Ok(text) => take.said = take.with(&text),
                            Err(e) => {
                                takes.remove(&id);
                                note(pane, Note::Done(Err(e)));
                                continue;
                            }
                        }
                        match last {
                            None => note(pane, Note::Draft(id, take.said.clone())),
                            Some(why) => {
                                let said = takes.remove(&id).map(|t| t.said).unwrap_or_default();
                                note(
                                    pane,
                                    Note::Done(match (said.is_empty(), why) {
                                        (false, _) => Ok(said),
                                        (true, Some(why)) => Err(why),
                                        (true, None) => Err(
                                            "Heard no words. Speak closer to the microphone, or pick another in Settings, under General."
                                                .to_string(),
                                        ),
                                    }),
                                );
                            }
                        }
                    }
                    Job::Drop(_) => {}
                }
            }
        });
        tx
    })
}

/// The model, fetched first if it is not here, the bar moved on at each
/// whole percent.
fn load(pane: isize) -> Result<Listener, String> {
    let models = models().ok_or("no state folder")?;
    if !speech::is_fetched(&models) {
        note(pane, Note::Phase(Phase::Fetching));
    }
    let total = speech::download_size().max(1);
    let done = Cell::new(speech::fetched_bytes(&models));
    FETCHED.store(done.get(), Ordering::Relaxed);
    let dir = speech::ensure(&models, |url, limit| {
        let base = done.get();
        let percent = Cell::new(base * 100 / total);
        let body = crate::net::get_counting(url, limit, &|n| {
            FETCHED.store(base + n, Ordering::Relaxed);
            if (base + n) * 100 / total != percent.get() {
                percent.set((base + n) * 100 / total);
                poke(pane);
            }
        });
        done.set(base + limit);
        body
    })?;
    FETCHED.store(total, Ordering::Relaxed);
    poke(pane);
    Listener::load(&dir)
}

/// The microphone until `stop` is set or `heard` says it is done, each
/// stretch of samples handed to `heard` at [`RATE`], mono. The
/// microphone's name when it ends.
fn record(stop: &AtomicBool, heard: &mut dyn FnMut(&[f32]) -> bool) -> Result<String, String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let mic =
            capture(stop, heard).map_err(|e| format!("The microphone could not be read: {e}"));
        CoUninitialize();
        mic
    }
}

unsafe fn capture(
    stop: &AtomicBool,
    heard: &mut dyn FnMut(&[f32]) -> bool,
) -> windows::core::Result<String> {
    let devices: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
    let mic = microphone(&devices)?;
    let name = device_name(&mic).unwrap_or_else(|| "the microphone".to_string());
    let client: IAudioClient = mic.Activate(CLSCTX_ALL, None)?;
    let format = WAVEFORMATEX {
        // WAVE_FORMAT_IEEE_FLOAT.
        wFormatTag: 3,
        nChannels: 1,
        nSamplesPerSec: RATE,
        nAvgBytesPerSec: RATE * 4,
        nBlockAlign: 4,
        wBitsPerSample: 32,
        cbSize: 0,
    };
    // A second of buffer, in 100 ns units; it is drained every 20 ms.
    client.Initialize(
        AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
        10_000_000,
        0,
        &format,
        None,
    )?;
    let capture: IAudioCaptureClient = client.GetService()?;
    client.Start()?;
    let began = Instant::now();
    let mut chunk = Vec::with_capacity(RATE as usize / 10);
    let mut done = false;
    while !done && !stop.load(Ordering::Acquire) && began.elapsed() < LONGEST {
        thread::sleep(Duration::from_millis(20));
        chunk.clear();
        while capture.GetNextPacketSize()? > 0 {
            let mut data = std::ptr::null_mut();
            let (mut frames, mut flags) = (0u32, 0u32);
            capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                chunk.extend(std::iter::repeat_n(0.0, frames as usize));
            } else {
                chunk.extend_from_slice(std::slice::from_raw_parts(
                    data.cast::<f32>(),
                    frames as usize,
                ));
            }
            capture.ReleaseBuffer(frames)?;
        }
        if !chunk.is_empty() {
            done = heard(&chunk);
        }
    }
    let _ = client.Stop();
    Ok(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `secs` of a tone `loud` RMS, in 20 ms chunks as WASAPI gives them.
    fn sound(loud: f32, secs: f32) -> Vec<Vec<f32>> {
        let amp = loud * std::f32::consts::SQRT_2;
        let n = (secs * RATE as f32) as usize;
        let all: Vec<f32> = (0..n).map(|i| (i as f32 * 0.3).sin() * amp).collect();
        all.chunks(ms(20)).map(<[f32]>::to_vec).collect()
    }

    /// The phrases `ear` cuts from the stretches heard from sample `at`
    /// on, and where they end.
    fn hear(ear: &mut Ear, mut at: usize, parts: &[(f32, f32)]) -> (Vec<Range<usize>>, usize) {
        let mut cuts = Vec::new();
        for &(loud, secs) in parts {
            for c in sound(loud, secs) {
                cuts.extend(ear.hear(at, &c));
                at += c.len();
            }
        }
        (cuts, at)
    }

    #[test]
    fn a_pause_ends_a_phrase() {
        let mut ear = Ear::default();
        let (cuts, end) = hear(&mut ear, 0, &[(0.001, 1.0), (0.1, 2.0), (0.001, 1.0), (0.1, 1.0)]);
        assert_eq!(cuts.len(), 1);
        // The silence before it is kept short, and a little after it.
        assert_eq!(cuts[0].start, ms(1000) - LEAD);
        assert_eq!(cuts[0].end, ms(3000) + TAIL);
        // The second phrase is still being spoken.
        assert!(ear.voiced);
        assert!(ear.rest(end).start >= cuts[0].end);
    }

    #[test]
    fn a_breath_does_not() {
        let mut ear = Ear::default();
        let (cuts, _) = hear(&mut ear, 0, &[(0.1, 1.0), (0.001, 0.3), (0.1, 1.0)]);
        assert!(cuts.is_empty());
    }

    #[test]
    fn a_long_phrase_is_cut_anyway() {
        let mut ear = Ear::default();
        // Words with a dip between them, as speech has, never a pause.
        let words: Vec<(f32, f32)> = (0..60).flat_map(|_| [(0.1, 0.3), (0.02, 0.1)]).collect();
        let (cuts, _) = hear(&mut ear, 0, &words);
        assert_eq!(cuts.len(), 1);
        assert!(cuts[0].len() >= LONGEST_PHRASE);
    }

    #[test]
    fn steady_noise_is_not_speech() {
        let mut ear = Ear::default();
        hear(&mut ear, 0, &[(0.004, 3.0)]);
        assert!(!ear.spoke);
        assert!(ear.why_nothing("Mic").is_some_and(|w| w.starts_with("Heard no speech")));
    }

    #[test]
    fn a_dead_line_is_told_from_a_quiet_room() {
        let mut ear = Ear::default();
        hear(&mut ear, 0, &[(0.0, 2.0)]);
        assert!(ear.why_nothing("Line In").is_some_and(|w| w.starts_with("No sound came from Line In")));
    }

    #[test]
    fn fifteen_quiet_seconds_end_it() {
        let mut ear = Ear::default();
        let (_, end) = hear(&mut ear, 0, &[(0.1, 1.0), (0.001, 10.0)]);
        assert!(!ear.hushed(end));
        let (_, end) = hear(&mut ear, end, &[(0.001, 6.0)]);
        assert!(ear.hushed(end));
    }

    #[test]
    fn the_tail_starts_at_a_word() {
        assert_eq!(tail("short", 10), "short");
        assert_eq!(tail("one two three four", 9), "…four");
    }

    #[test]
    fn phrases_join_with_one_space() {
        assert_eq!(join("", " hello "), "hello");
        assert_eq!(join("hello", ""), "hello");
        assert_eq!(join("hello", "world"), "hello world");
    }
}
