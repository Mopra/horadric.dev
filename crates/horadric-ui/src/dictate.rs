//! Dictation: Ctrl+Space in a stage pane listens to the microphone, and
//! what was said is typed where the keyboard is, the Folio's composer or
//! the session's prompt. Nothing is sent: the human reads it and presses
//! Enter.
//!
//! Tapped, Ctrl+Space listens until it is pressed again. Held, it listens
//! until it is let go, like a walkie talkie. Esc throws away what was
//! heard. Whisper runs on this machine ([`horadric_memory::speech`]), so
//! the words never leave it.
//!
//! The first press on a machine without the model fetches it instead,
//! 332 MB with a bar counting up, and says when it is ready: speech
//! recorded then would wait minutes to be written down, or be lost if the
//! fetch failed.
//!
//! Three threads: the UI's, which owns the state below; one per recording,
//! which owns the microphone; and one that keeps the model loaded between
//! dictations and writes down what each recording heard. The last two
//! reach the pane with [`WM_DICTATE`].

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Listening,
    /// The model is being fetched or loaded before it can write.
    Fetching,
    Writing,
}

struct Now {
    pane: isize,
    phase: Phase,
    since: Instant,
    /// Set to end the recording; `keep` says whether what was heard is
    /// written down or thrown away.
    stop: Arc<AtomicBool>,
    keep: Arc<AtomicBool>,
}

/// How long a failure, or word that the model is ready, stays shown.
const AFTER_FOR: Duration = Duration::from_secs(8);

const READY: &str = "Dictation is ready. Press Ctrl+Space and talk.";

/// Shorter than this, a recording was two taps and not an attempt to
/// talk, so its silence is not worth a word.
const SHORTEST: usize = speech::SAMPLE_RATE / 2;

/// The microphone picked in Settings, by its endpoint id. None, or one
/// unplugged since, is the one Windows records from by default.
static MIC: Mutex<Option<String>> = Mutex::new(None);

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

thread_local! {
    static NOW: RefCell<Option<Now>> = const { RefCell::new(None) };
    /// What the last dictation left to say, by pane, until the next one
    /// starts: why it failed, or that the model is ready.
    static AFTER: RefCell<Option<(isize, String, Instant)>> = const { RefCell::new(None) };
}

/// Bytes of the model on disk while it is fetched, for the bar.
static FETCHED: AtomicUsize = AtomicUsize::new(0);

/// What the background threads tell the UI, by pane.
enum Note {
    Phase(Phase),
    /// The model was fetched and loaded on the first press.
    Ready,
    Done(Result<String, String>),
}

/// What a pane dictating shows beyond words: a lit microphone, a bar.
#[derive(Clone, Copy, PartialEq)]
pub enum Shown {
    Listening,
    /// How much of the model is here, 0 to 1. At 1 it is being loaded.
    Fetching(f32),
    Writing,
}

enum Job {
    /// Fetch and load the model, nothing to write yet.
    Prepare(isize),
    Write(isize, Vec<f32>),
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

/// Ctrl+Space went down in `pane`, or its microphone was clicked.
pub fn press(pane: HWND) {
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
                    start(pane.0 as isize)
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

/// Esc while listening: what was heard is dropped. False when `pane` is
/// not listening, so Esc does what it always does.
pub fn cancel(pane: HWND) -> bool {
    NOW.with(|n| {
        let mut n = n.borrow_mut();
        let Some(now) = n.as_ref() else { return false };
        if now.pane != pane.0 as isize || now.phase != Phase::Listening {
            return false;
        }
        now.keep.store(false, Ordering::Release);
        now.stop.store(true, Ordering::Release);
        *n = None;
        true
    })
}

/// What `pane` shows while it dictates, where its header's detail would
/// be, or the Folio's notice.
pub fn status(pane: HWND) -> Option<String> {
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
        Shown::Listening => "Listening. Ctrl+Space to stop, Esc to drop it.".to_string(),
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

/// Where `pane` is in a dictation, None when it is not dictating.
pub fn shown(pane: HWND) -> Option<Shown> {
    let phase = NOW.with(|n| {
        n.borrow()
            .as_ref()
            .filter(|now| now.pane == pane.0 as isize)
            .map(|now| now.phase)
    })?;
    Some(match phase {
        Phase::Listening => Shown::Listening,
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

fn start(pane: isize) -> Now {
    let stop = Arc::new(AtomicBool::new(false));
    let keep = Arc::new(AtomicBool::new(true));
    let (s, k) = (stop.clone(), keep.clone());
    thread::spawn(move || {
        let heard = record(&s);
        if !k.load(Ordering::Acquire) {
            return;
        }
        match heard {
            Ok((pcm, _)) if pcm.len() < SHORTEST => note(pane, Note::Done(Ok(String::new()))),
            // Whisper would make words up out of it, and nothing typed
            // with nothing said leaves the human wondering what broke.
            Ok((pcm, mic)) if speech::is_silent(&pcm) => note(
                pane,
                Note::Done(Err(format!(
                    "Heard nothing from {mic}. Pick your microphone in Settings, under General."
                ))),
            ),
            Ok((pcm, _)) => {
                note(pane, Note::Phase(Phase::Writing));
                let _ = writer().send(Job::Write(pane, pcm));
            }
            Err(e) => note(pane, Note::Done(Err(e))),
        }
    });
    Now {
        pane,
        phase: Phase::Listening,
        since: Instant::now(),
        stop,
        keep,
    }
}

fn prepare(pane: isize) -> Now {
    let _ = writer().send(Job::Prepare(pane));
    Now {
        pane,
        phase: Phase::Fetching,
        since: Instant::now(),
        stop: Arc::new(AtomicBool::new(false)),
        keep: Arc::new(AtomicBool::new(false)),
    }
}

/// The thread that keeps the model and writes down each recording.
fn writer() -> &'static Sender<Job> {
    static WRITER: OnceLock<Sender<Job>> = OnceLock::new();
    WRITER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        thread::spawn(move || {
            let mut listener: Option<Listener> = None;
            for job in rx {
                let pane = match &job {
                    Job::Prepare(p) | Job::Write(p, _) => *p,
                };
                if listener.is_none() {
                    match load(pane) {
                        Ok(l) => listener = Some(l),
                        Err(e) => {
                            note(pane, Note::Done(Err(e)));
                            continue;
                        }
                    }
                }
                let Some(l) = listener.as_mut() else { continue };
                match job {
                    Job::Prepare(_) => note(pane, Note::Ready),
                    Job::Write(_, pcm) => note(pane, Note::Done(l.transcribe(&pcm))),
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

/// The microphone's samples until `stop` is set, at [`RATE`], mono, and
/// its name.
fn record(stop: &AtomicBool) -> Result<(Vec<f32>, String), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let heard = capture(stop).map_err(|e| format!("The microphone could not be read: {e}"));
        CoUninitialize();
        heard
    }
}

unsafe fn capture(stop: &AtomicBool) -> windows::core::Result<(Vec<f32>, String)> {
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
    let mut pcm = Vec::with_capacity(RATE as usize * 30);
    while !stop.load(Ordering::Acquire) && began.elapsed() < LONGEST {
        thread::sleep(Duration::from_millis(20));
        while capture.GetNextPacketSize()? > 0 {
            let mut data = std::ptr::null_mut();
            let (mut frames, mut flags) = (0u32, 0u32);
            capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
            if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || data.is_null() {
                pcm.extend(std::iter::repeat_n(0.0, frames as usize));
            } else {
                pcm.extend_from_slice(std::slice::from_raw_parts(
                    data.cast::<f32>(),
                    frames as usize,
                ));
            }
            capture.ReleaseBuffer(frames)?;
        }
    }
    let _ = client.Stop();
    Ok((pcm, name))
}
