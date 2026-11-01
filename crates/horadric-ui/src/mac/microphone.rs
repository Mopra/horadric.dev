//! The Mac's microphone for dictation, through AVAudioEngine: the same
//! `record` the Windows face has over WASAPI, so the model side
//! ([`horadric_memory::speech`]) and the phrase cutting are shared.
//!
//! AVAudioEngine hands out the hardware's own rate (44.1 or 48 kHz) and
//! its channels; they are mixed to mono and brought to Whisper's 16 kHz
//! here ([`Resampler`]). The first start asks the human for the
//! microphone (Info.plist says why); refused, the input reads silence,
//! which dictation already words as "no sound came from" it.
//!
//! Spoken to with `msg_send!` rather than a crate of AVFAudio bindings:
//! six methods do not justify a dependency.
//!
//! Not wired to a key yet: the Mac stage has no dictation key or draft to
//! show (docs/plan/dictation.md). Written against the API without a Mac
//! to build on, so the first build there is its test.

#![allow(dead_code)]

use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use block2::RcBlock;
use horadric_memory::speech::{self, resample::Resampler};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool};

// AVAudioEngine lives in AVFAudio, which AVFoundation re-exports on every
// macOS the app runs on (11 and up).
#[link(name = "AVFoundation", kind = "framework")]
extern "C" {}

/// A recording stops by itself after this, as on Windows.
const LONGEST: Duration = Duration::from_secs(180);

/// Frames a tap hands over at once, about a tenth of a second.
const TAP_FRAMES: u32 = 4096;

/// The microphone until `stop` is set or `heard` says it is done, each
/// stretch of samples handed to `heard` at 16 kHz, mono. The
/// microphone's name when it ends.
pub fn record(stop: &AtomicBool, heard: &mut dyn FnMut(&[f32]) -> bool) -> Result<String, String> {
    let (tx, rx) = mpsc::channel::<Vec<f32>>();
    // SAFETY: the messages are AVAudioEngine's documented ones, sent to
    // the objects it returned, and the tap is removed before the engine
    // and the block go.
    unsafe {
        let class = AnyClass::get(c"AVAudioEngine").ok_or("This Mac has no AVAudioEngine")?;
        let engine: Retained<AnyObject> = msg_send![class, new];
        let input: *mut AnyObject = msg_send![&*engine, inputNode];
        if input.is_null() {
            return Err("This Mac has no microphone".into());
        }
        let format: *mut AnyObject = msg_send![input, outputFormatForBus: 0usize];
        let rate: f64 = msg_send![format, sampleRate];
        let channels: u32 = msg_send![format, channelCount];
        if rate <= 0.0 || channels == 0 {
            return Err("No microphone is plugged in".into());
        }
        let block = RcBlock::new(move |buffer: *mut AnyObject, _when: *mut AnyObject| {
            let frames: u32 = msg_send![buffer, frameLength];
            let stride: usize = msg_send![buffer, stride];
            let data: *const *const f32 = msg_send![buffer, floatChannelData];
            if data.is_null() || frames == 0 {
                return;
            }
            let stride = stride.max(1);
            let mono: Vec<f32> = (0..frames as usize)
                .map(|i| {
                    let sum: f32 = (0..channels as usize)
                        .map(|c| *(*data.add(c)).add(i * stride))
                        .sum();
                    sum / channels as f32
                })
                .collect();
            let _ = tx.send(mono);
        });
        let _: () = msg_send![
            input,
            installTapOnBus: 0usize,
            bufferSize: TAP_FRAMES,
            format: format,
            block: &*block
        ];
        let _: () = msg_send![&*engine, prepare];
        let mut error: *mut AnyObject = ptr::null_mut();
        let started: Bool = msg_send![&*engine, startAndReturnError: &mut error];
        if !started.as_bool() {
            let _: () = msg_send![input, removeTapOnBus: 0usize];
            return Err("The microphone could not be read".into());
        }
        let mut resampler = Resampler::new(rate, speech::SAMPLE_RATE as f64);
        let mut chunk = Vec::new();
        let began = Instant::now();
        while !stop.load(Ordering::Acquire) && began.elapsed() < LONGEST {
            let Ok(samples) = rx.recv_timeout(Duration::from_millis(20)) else {
                continue;
            };
            chunk.clear();
            resampler.push(&samples, &mut chunk);
            if !chunk.is_empty() && heard(&chunk) {
                break;
            }
        }
        let _: () = msg_send![input, removeTapOnBus: 0usize];
        let _: () = msg_send![&*engine, stop];
        drop(block);
    }
    Ok("the Mac's microphone".to_string())
}
