//! Named pipes for session hosts, both ends.
//!
//! Every handle is opened overlapped even though each call here blocks.
//! A reader thread sits in a read for the life of the connection while
//! another thread writes, and on a handle opened for synchronous I/O
//! Windows runs one call at a time, so the write would wait for output
//! that may never come.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    LocalFree, ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_MORE_DATA, ERROR_PIPE_BUSY,
    ERROR_PIPE_CONNECTED, ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE, HLOCAL,
    INVALID_HANDLE_VALUE,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FindClose, FindFirstFileW, FindNextFileW, FlushFileBuffers, ReadFile, WriteFile,
    FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING,
    PIPE_ACCESS_DUPLEX, WIN32_FIND_DATAW,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::Threading::{CreateEventW, GetCurrentProcess, OpenProcessToken};
use windows::Win32::System::IO::{GetOverlappedResult, OVERLAPPED};

use crate::wide;

const BUFFER: u32 = 64 * 1024;

/// One end of a connected pipe.
pub struct Pipe {
    handle: OwnedHandle,
}

impl Pipe {
    /// A new server instance of `name`, waiting for [`Pipe::accept`].
    /// `first` refuses when the name exists already, so nobody can sit on
    /// a session's name before its host does. Only the current user can
    /// connect, and only from this machine.
    pub fn serve(name: &str, first: bool) -> io::Result<Pipe> {
        let sd = user_only()?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: sd.0 .0,
            bInheritHandle: false.into(),
        };
        let mut mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED;
        if first {
            mode |= FILE_FLAG_FIRST_PIPE_INSTANCE;
        }
        let name = wide(name.as_ref());
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                mode,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                PIPE_UNLIMITED_INSTANCES,
                BUFFER,
                BUFFER,
                0,
                Some(&sa),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(Pipe {
            handle: unsafe { OwnedHandle::from_raw_handle(handle.0) },
        })
    }

    /// Waits for a client on a server instance.
    pub fn accept(&self) -> io::Result<()> {
        let event = Event::new()?;
        let mut ov = OVERLAPPED {
            hEvent: event.handle(),
            ..Default::default()
        };
        match unsafe { ConnectNamedPipe(self.raw(), Some(&mut ov)) } {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => Ok(()),
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {
                let mut n = 0u32;
                unsafe { GetOverlappedResult(self.raw(), &ov, &mut n, true) }?;
                Ok(())
            }
            Err(e) => Err(os(e)),
        }
    }

    /// Connects to a host's pipe. Fails at once when there is no host, and
    /// waits a moment when every instance is busy.
    pub fn connect(name: &str) -> io::Result<Pipe> {
        let wname = wide(name.as_ref());
        for _ in 0..10 {
            let opened = unsafe {
                CreateFileW(
                    PCWSTR(wname.as_ptr()),
                    (GENERIC_READ | GENERIC_WRITE).0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    None,
                )
            };
            match opened {
                Ok(h) => {
                    return Ok(Pipe {
                        handle: unsafe { OwnedHandle::from_raw_handle(h.0) },
                    })
                }
                Err(e) if e.code() == ERROR_PIPE_BUSY.to_hresult() => {
                    let _ = unsafe { WaitNamedPipeW(PCWSTR(wname.as_ptr()), 500) };
                }
                Err(e) => return Err(os(e)),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the pipe stayed busy",
        ))
    }

    /// Reads what has arrived, at least one byte. Zero means the other end
    /// is gone.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let event = Event::new()?;
        let mut ov = OVERLAPPED {
            hEvent: event.handle(),
            ..Default::default()
        };
        let mut n = 0u32;
        let started = unsafe { ReadFile(self.raw(), Some(buf), None, Some(&mut ov)) };
        let done = match started {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => Ok(()),
            Err(e) => Err(e),
        }
        .and_then(|()| unsafe { GetOverlappedResult(self.raw(), &ov, &mut n, true) });
        match done {
            Ok(()) => Ok(n as usize),
            Err(e) if e.code() == ERROR_MORE_DATA.to_hresult() => Ok(n as usize),
            Err(e)
                if e.code() == ERROR_BROKEN_PIPE.to_hresult()
                    || e.code() == ERROR_PIPE_NOT_CONNECTED.to_hresult() =>
            {
                Ok(0)
            }
            Err(e) => Err(os(e)),
        }
    }

    /// Fills `buf` exactly, or fails.
    pub fn read_exact(&self, buf: &mut [u8]) -> io::Result<()> {
        let mut at = 0;
        while at < buf.len() {
            match self.read(&mut buf[at..])? {
                0 => return Err(io::ErrorKind::UnexpectedEof.into()),
                n => at += n,
            }
        }
        Ok(())
    }

    pub fn write_all(&self, mut bytes: &[u8]) -> io::Result<()> {
        let event = Event::new()?;
        while !bytes.is_empty() {
            let mut ov = OVERLAPPED {
                hEvent: event.handle(),
                ..Default::default()
            };
            let mut n = 0u32;
            let started = unsafe { WriteFile(self.raw(), Some(bytes), None, Some(&mut ov)) };
            match started {
                Ok(()) => {}
                Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {}
                Err(e) => return Err(os(e)),
            }
            unsafe { GetOverlappedResult(self.raw(), &ov, &mut n, true) }?;
            if n == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            bytes = &bytes[n as usize..];
        }
        Ok(())
    }

    /// Waits until the other end has read everything written.
    pub fn flush(&self) {
        let _ = unsafe { FlushFileBuffers(self.raw()) };
    }

    /// Server side: drops the client, which makes its reads and ours end.
    pub fn disconnect(&self) {
        let _ = unsafe { DisconnectNamedPipe(self.raw()) };
    }

    fn raw(&self) -> HANDLE {
        HANDLE(self.handle.as_raw_handle())
    }
}

/// The `windows` crate turns an error into an `io::Error` holding the
/// HRESULT, whose kind is always `Other`. The Win32 code inside it keeps
/// `NotFound` a `NotFound`, which is how a missing host is told apart.
fn os(e: windows::core::Error) -> io::Error {
    let code = e.code().0 as u32;
    if code & 0xFFFF_0000 == 0x8007_0000 {
        io::Error::from_raw_os_error((code & 0xFFFF) as i32)
    } else {
        e.into()
    }
}

/// The session ids of every host pipe whose name starts with `prefix`.
pub fn list(prefix: &str) -> Vec<String> {
    let pattern = wide(r"\\.\pipe\*".as_ref());
    let mut data = WIN32_FIND_DATAW::default();
    let Ok(find) = (unsafe { FindFirstFileW(PCWSTR(pattern.as_ptr()), &mut data) }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    loop {
        let len = data
            .cFileName
            .iter()
            .position(|&c| c == 0)
            .unwrap_or(data.cFileName.len());
        let name = String::from_utf16_lossy(&data.cFileName[..len]);
        if let Some(id) = name.strip_prefix(prefix) {
            if !id.is_empty() && !out.iter().any(|o| o == id) {
                out.push(id.to_string());
            }
        }
        if unsafe { FindNextFileW(find, &mut data) }.is_err() {
            break;
        }
    }
    let _ = unsafe { FindClose(find) };
    out
}

/// A manual reset event for one overlapped call.
struct Event(OwnedHandle);

impl Event {
    fn new() -> io::Result<Event> {
        let h = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }?;
        Ok(Event(unsafe { OwnedHandle::from_raw_handle(h.0) }))
    }

    fn handle(&self) -> HANDLE {
        HANDLE(self.0.as_raw_handle())
    }
}

/// A security descriptor freed with `LocalFree`.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0 .0)));
        }
    }
}

/// Full access for the user this process runs as, and for nobody else.
/// The default for a pipe lets everyone read, which would be everyone
/// reading the agent's screen.
fn user_only() -> io::Result<Descriptor> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let token = OwnedHandle::from_raw_handle(token.0);
        let mut len = 0u32;
        let _ = GetTokenInformation(HANDLE(token.as_raw_handle()), TokenUser, None, 0, &mut len);
        // usize elements keep the SID pointer inside aligned.
        let mut buf = vec![0usize; (len as usize).div_ceil(std::mem::size_of::<usize>())];
        GetTokenInformation(
            HANDLE(token.as_raw_handle()),
            TokenUser,
            Some(buf.as_mut_ptr() as *mut c_void),
            len,
            &mut len,
        )?;
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut sid = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid)?;
        let text = sid.to_string().unwrap_or_default();
        let _ = LocalFree(Some(HLOCAL(sid.0 as *mut c_void)));
        let sddl = wide(format!("D:P(A;;GA;;;{text})").as_ref());
        let mut sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            None,
        )?;
        Ok(Descriptor(sd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn a_client_and_a_server_talk_both_ways_at_once() {
        let name = format!(r"\\.\pipe\horadric-test-{}", std::process::id());
        let server = Pipe::serve(&name, true).unwrap();
        assert!(Pipe::serve(&name, true).is_err(), "the name is taken");
        let missing = Pipe::connect(r"\\.\pipe\horadric-test-nobody-here").err();
        assert_eq!(missing.map(|e| e.kind()), Some(io::ErrorKind::NotFound));
        let client = thread::spawn({
            let name = name.clone();
            move || Pipe::connect(&name).unwrap()
        });
        server.accept().unwrap();
        let client = std::sync::Arc::new(client.join().unwrap());

        // The client blocks in a read while it writes from another thread.
        let reader = {
            let client = std::sync::Arc::clone(&client);
            thread::spawn(move || {
                let mut buf = [0u8; 5];
                client.read_exact(&mut buf).unwrap();
                buf
            })
        };
        thread::sleep(std::time::Duration::from_millis(50));
        client.write_all(b"ping").unwrap();
        let mut got = [0u8; 4];
        server.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"ping");
        server.write_all(b"pong!").unwrap();
        assert_eq!(&reader.join().unwrap(), b"pong!");

        assert!(list("horadric-test-").contains(&std::process::id().to_string()));
        server.disconnect();
        let mut buf = [0u8; 1];
        assert_eq!(client.read(&mut buf).unwrap_or(0), 0);
    }
}
