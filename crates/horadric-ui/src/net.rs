//! HTTP GET through WinHTTP, which ships with Windows, follows GitHub's
//! redirects to its storage host and uses the system's proxy settings.
//! The updater is its only user, so it is blocking and runs on a thread.

use std::ffi::c_void;

use horadric_core::release::split_url;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Networking::WinHttp::{
    WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders,
    WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WINHTTP_OPEN_REQUEST_FLAGS,
    WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
};

/// Closes a WinHTTP handle when it goes, so every early return is clean.
struct Handle(*mut c_void);

impl Handle {
    fn new(raw: *mut c_void, what: &str) -> Result<Handle, String> {
        if raw.is_null() {
            Err(format!("{what}: {}", windows::core::Error::from_thread()))
        } else {
            Ok(Handle(raw))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

/// The body of `url`, which must answer 200 and send at most `limit`
/// bytes. A slow server gives up after half a minute rather than hang the
/// thread for good.
pub fn get(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    let parts = split_url(url).ok_or_else(|| format!("`{url}` is not an http or https URL"))?;
    let agent = format!("Horadric/{}", env!("CARGO_PKG_VERSION"));
    unsafe {
        let session = Handle::new(
            WinHttpOpen(
                &HSTRING::from(agent),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                PCWSTR::null(),
                PCWSTR::null(),
                0,
            ),
            "WinHttpOpen",
        )?;
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 30_000, 30_000)
            .map_err(|e| format!("WinHttpSetTimeouts: {e}"))?;
        let connection = Handle::new(
            WinHttpConnect(
                session.0,
                &HSTRING::from(parts.host.as_str()),
                parts.port,
                0,
            ),
            "cannot connect",
        )?;
        let flags = if parts.secure {
            WINHTTP_FLAG_SECURE
        } else {
            WINHTTP_OPEN_REQUEST_FLAGS(0)
        };
        let request = Handle::new(
            WinHttpOpenRequest(
                connection.0,
                w!("GET"),
                &HSTRING::from(parts.path.as_str()),
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                flags,
            ),
            "WinHttpOpenRequest",
        )?;
        WinHttpSendRequest(request.0, None, None, 0, 0, 0)
            .map_err(|e| format!("cannot reach {}: {e}", parts.host))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut())
            .map_err(|e| format!("no answer from {}: {e}", parts.host))?;

        let mut status = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut size,
            std::ptr::null_mut(),
        )
        .map_err(|e| format!("no status: {e}"))?;
        if status != 200 {
            return Err(format!("{url} answered {status}"));
        }

        let mut body = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr() as *mut c_void,
                chunk.len() as u32,
                &mut read,
            )
            .map_err(|e| format!("the download broke off: {e}"))?;
            if read == 0 {
                return Ok(body);
            }
            if body.len() + read as usize > limit {
                return Err(format!("{url} is larger than {limit} bytes"));
            }
            body.extend_from_slice(&chunk[..read as usize]);
        }
    }
}
