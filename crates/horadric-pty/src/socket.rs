//! Unix sockets for session hosts, both ends: the Mac's named pipes.
//!
//! A host listens on `/tmp/horadric-<uid>/<name>`. Not under Application
//! Support, where a long session id would overrun the 104 bytes a socket
//! path may have. The folder is the user's alone, and a host also holds a
//! lock on `<name>.lock` for as long as it lives, which is how a live host
//! is told from a socket file left behind by one that crashed, without
//! connecting to it (a connection would take the host from its UI).

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

/// What a Windows pipe name starts with, taken off for the socket's name.
const PIPE_ROOT: &str = r"\\.\pipe\";

/// The longest socket path the Mac takes, less its terminating NUL.
const MAX_PATH: usize = 103;

/// The folder every host of this user listens in. Made private to the
/// user, and refused when someone else made it first.
pub fn socket_dir() -> io::Result<PathBuf> {
    let uid = unsafe { libc::getuid() };
    let dir = PathBuf::from(format!("/tmp/horadric-{uid}"));
    match fs::create_dir(&dir) {
        Ok(()) => fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = fs::symlink_metadata(&dir)?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is not this user's alone", dir.display()),
        ));
    }
    Ok(dir)
}

/// The socket's file name for a pipe name as `wire::pipe_name` gives it.
pub fn file_name(name: &str) -> &str {
    name.strip_prefix(PIPE_ROOT).unwrap_or(name)
}

fn path_of(name: &str) -> io::Result<PathBuf> {
    let path = socket_dir()?.join(file_name(name));
    if path.as_os_str().len() > MAX_PATH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is too long for a socket", path.display()),
        ));
    }
    Ok(path)
}

/// A host's listening socket and the lock that says it lives.
struct Listening {
    listener: UnixListener,
    _lock: File,
}

/// Every name this process serves, so a second [`Pipe::serve`] of a name
/// waits on the same socket the first one bound.
fn served() -> &'static Mutex<HashMap<String, Arc<Listening>>> {
    static SERVED: OnceLock<Mutex<HashMap<String, Arc<Listening>>>> = OnceLock::new();
    SERVED.get_or_init(Default::default)
}

/// One end of a connected socket, or a server end waiting for one.
pub struct Pipe {
    listening: Option<Arc<Listening>>,
    stream: OnceLock<UnixStream>,
}

impl Pipe {
    /// A server end of `name`, waiting for [`Pipe::accept`]. `first`
    /// refuses when a live host serves the name already, so nobody can sit
    /// on a session's name before its host does.
    pub fn serve(name: &str, first: bool) -> io::Result<Pipe> {
        let mut map = served().lock().map_err(|_| io::Error::other("poisoned"))?;
        if !first {
            if let Some(l) = map.get(name) {
                return Ok(Pipe {
                    listening: Some(Arc::clone(l)),
                    stream: OnceLock::new(),
                });
            }
        }
        let path = path_of(name)?;
        let lock_path = lock_of(&path);
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("a host serves {name} already"),
            ));
        }
        // Left by a host that is gone, since its lock was free.
        let _ = fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let listening = Arc::new(Listening {
            listener,
            _lock: lock,
        });
        map.insert(name.to_string(), Arc::clone(&listening));
        Ok(Pipe {
            listening: Some(listening),
            stream: OnceLock::new(),
        })
    }

    /// Waits for a client on a server end.
    pub fn accept(&self) -> io::Result<()> {
        let l = self
            .listening
            .as_ref()
            .ok_or_else(|| io::Error::other("not a server end"))?;
        let (stream, _) = l.listener.accept()?;
        self.stream
            .set(stream)
            .map_err(|_| io::Error::other("accepted twice"))
    }

    /// Connects to a host's socket. NotFound when no host serves it.
    pub fn connect(name: &str) -> io::Result<Pipe> {
        let path = path_of(name)?;
        let stream = UnixStream::connect(&path).map_err(|e| match e.kind() {
            io::ErrorKind::ConnectionRefused => io::Error::new(io::ErrorKind::NotFound, e),
            _ => e,
        })?;
        Ok(Pipe {
            listening: None,
            stream: OnceLock::from(stream),
        })
    }

    fn stream(&self) -> io::Result<&UnixStream> {
        self.stream
            .get()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "not connected"))
    }

    /// Reads what has arrived, at least one byte. Zero means the other end
    /// is gone.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut s = self.stream()?;
        loop {
            match s.read(buf) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == io::ErrorKind::ConnectionReset => return Ok(0),
                r => return r,
            }
        }
    }

    /// Fills `buf` exactly, or fails.
    pub fn read_exact(&self, buf: &mut [u8]) -> io::Result<()> {
        let mut s = self.stream()?;
        s.read_exact(buf)
    }

    pub fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        let mut s = self.stream()?;
        s.write_all(bytes)
    }

    /// The kernel keeps what was written for the reader after a close, so
    /// there is nothing to wait for.
    pub fn flush(&self) {}

    /// Server side: drops the client, which makes its reads and ours end.
    pub fn disconnect(&self) {
        if let Ok(s) = self.stream() {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

/// The session ids of every live host whose socket name starts with
/// `prefix`.
pub fn list(prefix: &str) -> Vec<String> {
    let Ok(dir) = socket_dir() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_prefix(prefix) else {
            continue;
        };
        if id.is_empty() || id.ends_with(".lock") || !alive(&e.path()) {
            continue;
        }
        if !out.iter().any(|o| o == id) {
            out.push(id.to_string());
        }
    }
    out
}

/// The lock beside a socket. Appended rather than an extension, since a
/// session id may have a dot in it.
fn lock_of(socket: &std::path::Path) -> PathBuf {
    let mut p = socket.as_os_str().to_owned();
    p.push(".lock");
    PathBuf::from(p)
}

/// Whether a host holds the lock beside this socket.
fn alive(socket: &std::path::Path) -> bool {
    let Ok(lock) = File::open(lock_of(socket)) else {
        return false;
    };
    let free = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if free {
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
    }
    !free
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pipe_name_becomes_the_socket_file_name() {
        assert_eq!(
            file_name(r"\\.\pipe\horadric-43117-app-1"),
            "horadric-43117-app-1"
        );
        assert_eq!(file_name("horadric-1-x"), "horadric-1-x");
    }

    #[test]
    fn a_name_is_served_once_and_listed_while_it_lives() {
        let name = format!(r"\\.\pipe\horadric-test-{}-sock", std::process::id());
        let server = Pipe::serve(&name, true).unwrap();
        assert_eq!(
            Pipe::serve(&name, true).err().map(|e| e.kind()),
            Some(io::ErrorKind::AlreadyExists)
        );
        let prefix = format!("horadric-test-{}-", std::process::id());
        assert_eq!(list(&prefix), vec!["sock".to_string()]);
        let t = std::thread::spawn(move || {
            server.accept().unwrap();
            server.write_all(b"hi").unwrap();
            let mut b = [0u8; 2];
            server.read_exact(&mut b).unwrap();
            assert_eq!(&b, b"yo");
            server.disconnect();
        });
        let client = Pipe::connect(&name).unwrap();
        let mut b = [0u8; 2];
        client.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"hi");
        client.write_all(b"yo").unwrap();
        t.join().unwrap();
        assert_eq!(client.read(&mut b).unwrap(), 0);
        let missing = format!(r"\\.\pipe\horadric-test-{}-none", std::process::id());
        assert_eq!(
            Pipe::connect(&missing).err().map(|e| e.kind()),
            Some(io::ErrorKind::NotFound)
        );
    }
}
