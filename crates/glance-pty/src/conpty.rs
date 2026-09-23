//! One child process attached to one pseudo console.

use std::ffi::c_void;
use std::fs::File;
use std::io::{self, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::sync::Mutex;
use std::thread;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, INFINITE,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

use crate::{command_line, environment_block, wide};

/// What to run and how big the console starts.
#[derive(Debug, Clone)]
pub struct Command {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Added to the inherited environment, replacing any existing value.
    pub env_set: Vec<(String, String)>,
    /// Removed from the inherited environment.
    pub env_remove: Vec<&'static str>,
    pub cols: u16,
    pub rows: u16,
}

/// A running child in a pseudo console.
///
/// Output is read from the `File` returned by [`Pty::spawn`], on a thread of
/// the caller's choosing. Input goes through [`Pty::write`], which never
/// blocks: a writer thread owns the pipe, so a child that stops reading can
/// not freeze the UI thread that typed into it.
pub struct Pty {
    /// Zero once closed. Closing ends the child and lets the output pipe
    /// reach end of file, which it never does on its own.
    console: Mutex<HPCON>,
    process: OwnedHandle,
    input: Sender<Vec<u8>>,
    pid: u32,
}

impl Pty {
    /// Starts the child. Returns the handle and the output stream.
    pub fn spawn(cmd: &Command) -> io::Result<(Pty, File)> {
        unsafe {
            let (in_read, in_write) = pipe()?;
            let (out_read, out_write) = pipe()?;
            let size = COORD {
                X: cmd.cols.max(1) as i16,
                Y: cmd.rows.max(1) as i16,
            };
            let console = CreatePseudoConsole(
                size,
                HANDLE(in_read.as_raw_handle()),
                HANDLE(out_write.as_raw_handle()),
                0,
            )?;
            // The console host holds its own copies now.
            drop(in_read);
            drop(out_write);

            let started = start(cmd, console);
            let (process, pid) = match started {
                Ok(p) => p,
                Err(e) => {
                    ClosePseudoConsole(console);
                    return Err(e);
                }
            };

            let (input, rx) = mpsc::channel::<Vec<u8>>();
            let mut writer = File::from(in_write);
            thread::spawn(move || {
                for bytes in rx {
                    if writer.write_all(&bytes).is_err() {
                        break;
                    }
                }
            });

            let pty = Pty {
                console: Mutex::new(console),
                process,
                input,
                pid,
            };
            Ok((pty, File::from(out_read)))
        }
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Queues bytes for the child's input. Silently dropped once it exited.
    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.input.send(bytes.into());
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let console = self.console.lock().map_err(|_| poisoned())?;
        if console.0 == 0 {
            return Ok(());
        }
        let size = COORD {
            X: cols.max(1) as i16,
            Y: rows.max(1) as i16,
        };
        unsafe { ResizePseudoConsole(*console, size)? };
        Ok(())
    }

    /// Blocks until the child exits, then closes the console so the output
    /// stream ends. Returns the exit code.
    pub fn wait(&self) -> u32 {
        let handle = HANDLE(self.process.as_raw_handle());
        let mut code = 0u32;
        unsafe {
            WaitForSingleObject(handle, INFINITE);
            let _ = GetExitCodeProcess(handle, &mut code);
        }
        self.close();
        code
    }

    /// Ends the child now.
    pub fn kill(&self) {
        unsafe {
            let _ = TerminateProcess(HANDLE(self.process.as_raw_handle()), 1);
        }
    }

    fn close(&self) {
        if let Ok(mut console) = self.console.lock() {
            if console.0 != 0 {
                unsafe { ClosePseudoConsole(*console) };
                *console = HPCON(0);
            }
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.close();
    }
}

unsafe fn pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    CreatePipe(&mut read, &mut write, None, 0)?;
    Ok((
        OwnedHandle::from_raw_handle(read.0),
        OwnedHandle::from_raw_handle(write.0),
    ))
}

/// Creates the process with the pseudo console attached.
unsafe fn start(cmd: &Command, console: HPCON) -> io::Result<(OwnedHandle, u32)> {
    // Ask for the size, then initialise in a buffer of that size. The first
    // call fails by design.
    let mut size = 0usize;
    let _ = InitializeProcThreadAttributeList(None, 1, None, &mut size);
    // usize elements keep the buffer pointer aligned.
    let mut buf = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr() as *mut c_void);
    InitializeProcThreadAttributeList(Some(list), 1, None, &mut size)?;

    let result = (|| {
        UpdateProcThreadAttribute(
            list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
            Some(console.0 as *const c_void),
            std::mem::size_of::<HPCON>(),
            None,
            None,
        )?;

        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        // Without this a parent with redirected stdio hands those handles to
        // the child, which then writes around the console instead of into it.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
        si.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
        si.lpAttributeList = list;

        let mut line: Vec<u16> = wide(command_line(&cmd.program, &cmd.args).as_ref());
        let env = environment_block(std::env::vars_os(), &cmd.env_set, &cmd.env_remove);
        let cwd = wide(cmd.cwd.as_os_str());
        let mut pi = PROCESS_INFORMATION::default();
        CreateProcessW(
            PCWSTR::null(),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            Some(env.as_ptr() as *const c_void),
            PCWSTR(cwd.as_ptr()),
            &si.StartupInfo,
            &mut pi,
        )?;
        drop(OwnedHandle::from_raw_handle(pi.hThread.0));
        Ok((OwnedHandle::from_raw_handle(pi.hProcess.0), pi.dwProcessId))
    })();

    DeleteProcThreadAttributeList(list);
    result
}

fn poisoned() -> io::Error {
    io::Error::other("pseudo console lock poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn runs_a_child_and_reads_its_output() {
        let comspec = std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into());
        let cmd = Command {
            program: comspec.into(),
            args: vec!["/c".into(), "echo %GLANCE_PTY_TEST%".into()],
            cwd: std::env::temp_dir(),
            env_set: vec![("GLANCE_PTY_TEST".into(), "pty-says-hi".into())],
            env_remove: vec![],
            cols: 80,
            rows: 24,
        };
        let (pty, mut out) = Pty::spawn(&cmd).unwrap();
        let reader = thread::spawn(move || {
            let mut all = Vec::new();
            let _ = out.read_to_end(&mut all);
            String::from_utf8_lossy(&all).to_string()
        });
        pty.resize(100, 30).unwrap();
        assert_eq!(pty.wait(), 0);
        let text = reader.join().unwrap();
        assert!(text.contains("pty-says-hi"), "output was {text:?}");
    }
}
