//! Work handed to the main thread, where AppKit and the app live. Through
//! libdispatch's main queue, which the main run loop drains in every mode,
//! a menu's or a dialog's included.

use std::ffi::c_void;

#[repr(C)]
struct DispatchQueue {
    _opaque: [u8; 0],
}

extern "C" {
    /// What `dispatch_get_main_queue()` returns; the function is a macro.
    static _dispatch_main_q: DispatchQueue;
    fn dispatch_async_f(
        queue: *const DispatchQueue,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
}

type Job = Box<dyn FnOnce() + Send>;

/// Runs `f` on the main thread, soon, after whatever it is doing now.
pub fn post(f: impl FnOnce() + Send + 'static) {
    let job: Box<Job> = Box::new(Box::new(f));
    unsafe {
        dispatch_async_f(
            &raw const _dispatch_main_q,
            Box::into_raw(job).cast(),
            run_job,
        );
    }
}

extern "C" fn run_job(context: *mut c_void) {
    // SAFETY: made by `post` from a `Box<Job>` and run once.
    let job = unsafe { Box::from_raw(context.cast::<Job>()) };
    job();
}

extern "C" {
    fn dispatch_after_f(
        when: u64,
        queue: *const DispatchQueue,
        context: *mut c_void,
        work: extern "C" fn(*mut c_void),
    );
    fn dispatch_time(when: u64, delta: i64) -> u64;
}

/// `DISPATCH_TIME_NOW`.
const NOW: u64 = 0;

type MainJob = Box<dyn FnOnce()>;

/// Runs `f` on the main thread `wait` from now. Called on the main thread,
/// so `f` may hold what lives there.
pub fn after_main(wait: std::time::Duration, f: impl FnOnce() + 'static) {
    let job: Box<MainJob> = Box::new(Box::new(f));
    let nanos = wait.as_nanos().min(i64::MAX as u128) as i64;
    unsafe {
        dispatch_after_f(
            dispatch_time(NOW, nanos),
            &raw const _dispatch_main_q,
            Box::into_raw(job).cast(),
            run_main_job,
        );
    }
}

extern "C" fn run_main_job(context: *mut c_void) {
    // SAFETY: made by `after_main` on the main thread, run once there.
    let job = unsafe { Box::from_raw(context.cast::<MainJob>()) };
    job();
}
