//! Windows' Remote Desktop control, hosted in a child window and driven
//! by name. The `windows` crate has none of its interfaces, and it is an
//! ActiveX control, so `atl.dll` (part of Windows) is its container and
//! `IDispatch` the way to its properties. The one interface declared by
//! hand is the one that is not reachable by name, and it holds the
//! property the whole desk rests on: `ConnectToChildSession`.

use std::cell::Cell;
use std::ffi::c_void;
use std::mem::ManuallyDrop;

use windows::core::{w, IUnknown, Interface, BSTR, GUID, HRESULT, HSTRING, PCSTR, PCWSTR};
use windows::Win32::Foundation::{
    DISP_E_UNKNOWNNAME, E_NOINTERFACE, E_NOTIMPL, E_POINTER, HMODULE, HWND, LPARAM, S_OK,
    VARIANT_BOOL, WPARAM,
};
use windows::Win32::System::Com::{
    IConnectionPoint, IConnectionPointContainer, IDispatch, DISPATCH_FLAGS, DISPATCH_METHOD, DISPATCH_PROPERTYGET, DISPATCH_PROPERTYPUT, DISPPARAMS,
    EXCEPINFO,
};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleW, GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::System::Ole::{OleInitialize, DISPID_PROPERTYPUT};
use windows::Win32::System::Variant::{
    VariantClear, VARENUM, VARIANT, VT_BOOL, VT_BSTR, VT_DISPATCH, VT_I4, VT_UI4,
};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, PostMessageW, SetWindowPos, SWP_NOACTIVATE, SWP_NOZORDER,
    WINDOW_EX_STYLE, WS_CHILD, WS_CLIPCHILDREN, WS_VISIBLE,
};

/// MsRdpClient, the one that is not safe for scripting: only that one
/// takes a password.
const CONTROL: PCWSTR = w!("{A0C63C30-F08D-4AB4-907C-34905D770C7D}");

/// The control's events, as their dispatch ids.
pub(super) const ON_CONNECTED: usize = 2;
pub(super) const ON_LOGIN_COMPLETE: usize = 3;
pub(super) const ON_DISCONNECTED: usize = 4;
pub(super) const ON_FATAL_ERROR: usize = 10;
pub(super) const ON_LOGON_ERROR: usize = 22;

/// The settings that have no name `IDispatch` knows:
/// `IMsRdpExtendedSettings`. Its `put_Property(BSTR, VARIANT*)` is the
/// first method after `IUnknown`'s three.
const EXTENDED: GUID = GUID::from_u128(0x302D8188_0052_4807_806A_362B628F9AC5);
const PUT_PROPERTY: usize = 3;
type PutProperty = unsafe extern "system" fn(*mut c_void, *const u16, *const VARIANT) -> HRESULT;

/// `IMsTscAxEvents`. The control asks the sink for it by this id before
/// it accepts it, so a plain `IDispatch` is turned away.
const EVENTS: GUID = GUID::from_u128(0x336D5562_EFA8_482E_8CB3_C5C0FC7A7DB6);

/// Passes the control's events on as posted messages. They are not
/// handled where they are raised: closing the control from inside its
/// own event frees the code that is still running.
///
/// A COM object written out by hand, vtable and all: the `windows`
/// crate's `implement` macro needs `windows-core` as a dependency of
/// this crate's own, and one small object is not worth one.
#[repr(C)]
struct Sink {
    vtable: &'static SinkVtable,
    // Only ever touched on the UI thread, the control's apartment.
    refs: Cell<u32>,
    to: isize,
    message: u32,
    mark: u16,
}

#[repr(C)]
struct SinkVtable {
    query: unsafe extern "system" fn(*mut Sink, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut Sink) -> u32,
    release: unsafe extern "system" fn(*mut Sink) -> u32,
    type_info_count: unsafe extern "system" fn(*mut Sink, *mut u32) -> HRESULT,
    type_info: unsafe extern "system" fn(*mut Sink, u32, u32, *mut *mut c_void) -> HRESULT,
    ids_of_names:
        unsafe extern "system" fn(*mut Sink, *const GUID, *const PCWSTR, u32, u32, *mut i32) -> HRESULT,
    invoke: unsafe extern "system" fn(
        *mut Sink,
        i32,
        *const GUID,
        u32,
        u16,
        *const DISPPARAMS,
        *mut VARIANT,
        *mut EXCEPINFO,
        *mut u32,
    ) -> HRESULT,
}

static SINK: SinkVtable = SinkVtable {
    query: Sink::query,
    add_ref: Sink::add_ref,
    release: Sink::release,
    type_info_count: Sink::type_info_count,
    type_info: Sink::type_info,
    ids_of_names: Sink::ids_of_names,
    invoke: Sink::invoke,
};

impl Sink {
    /// A sink with one reference, which the `IUnknown` returned owns.
    fn new(to: HWND, message: u32, mark: u16) -> IUnknown {
        let sink = Box::new(Sink {
            vtable: &SINK,
            refs: Cell::new(1),
            to: to.0 as isize,
            message,
            mark,
        });
        unsafe { IUnknown::from_raw(Box::into_raw(sink) as *mut c_void) }
    }

    unsafe extern "system" fn query(
        this: *mut Sink,
        iid: *const GUID,
        out: *mut *mut c_void,
    ) -> HRESULT {
        if out.is_null() || iid.is_null() {
            return E_POINTER;
        }
        if [IUnknown::IID, IDispatch::IID, EVENTS].contains(&*iid) {
            Sink::add_ref(this);
            *out = this as *mut c_void;
            S_OK
        } else {
            *out = std::ptr::null_mut();
            E_NOINTERFACE
        }
    }

    unsafe extern "system" fn add_ref(this: *mut Sink) -> u32 {
        let refs = &(*this).refs;
        refs.set(refs.get() + 1);
        refs.get()
    }

    unsafe extern "system" fn release(this: *mut Sink) -> u32 {
        let left = (*this).refs.get() - 1;
        (*this).refs.set(left);
        if left == 0 {
            drop(Box::from_raw(this));
        }
        left
    }

    unsafe extern "system" fn type_info_count(_: *mut Sink, count: *mut u32) -> HRESULT {
        if !count.is_null() {
            *count = 0;
        }
        S_OK
    }

    unsafe extern "system" fn type_info(
        _: *mut Sink,
        _: u32,
        _: u32,
        _: *mut *mut c_void,
    ) -> HRESULT {
        E_NOTIMPL
    }

    unsafe extern "system" fn ids_of_names(
        _: *mut Sink,
        _: *const GUID,
        _: *const PCWSTR,
        _: u32,
        _: u32,
        _: *mut i32,
    ) -> HRESULT {
        DISP_E_UNKNOWNNAME
    }

    unsafe extern "system" fn invoke(
        this: *mut Sink,
        id: i32,
        _: *const GUID,
        _: u32,
        _: u16,
        params: *const DISPPARAMS,
        _: *mut VARIANT,
        _: *mut EXCEPINFO,
        _: *mut u32,
    ) -> HRESULT {
        // The events worth passing on carry one number or nothing.
        let number = params
            .as_ref()
            .filter(|p| p.cArgs == 1 && !p.rgvarg.is_null())
            .map(|p| &(*p.rgvarg).Anonymous.Anonymous)
            .filter(|v| v.vt == VT_I4)
            .map_or(0, |v| v.Anonymous.lVal);
        let _ = PostMessageW(
            Some(HWND((*this).to as *mut c_void)),
            (*this).message,
            WPARAM(((*this).mark as usize) << 16 | (id as usize & 0xFFFF)),
            LPARAM(number as isize),
        );
        S_OK
    }
}

/// What to sign in to the child session with.
pub(super) struct SignIn<'a> {
    pub user: &'a str,
    pub password: &'a str,
    /// The session's size, in physical pixels.
    pub size: (i32, i32),
    /// The monitor's scale in percent, so the desk's text is as large as
    /// the human's.
    pub scale: u32,
}

/// The hosted control. Dropping it closes it.
pub(super) struct Rdp {
    host: HWND,
    control: Disp,
    events: Option<(IConnectionPoint, u32)>,
}

impl Rdp {
    /// Creates the control as a child of `parent`, filling `size`. Its
    /// events come to `parent` as `message`, with the event's id in
    /// wParam's low word, `mark` in the word above it and the event's
    /// number in lParam. The mark tells this control's events from those
    /// of one before it that are still on their way.
    pub(super) fn create(
        parent: HWND,
        size: (i32, i32),
        message: u32,
        mark: u16,
    ) -> Result<Rdp, String> {
        let atl = Atl::load()?;
        unsafe {
            // In place activation needs OLE, which is more than the COM
            // the app starts. A second call on the same thread is fine.
            let _ = OleInitialize(None);
            if (atl.init)() == 0 {
                return Err("atl.dll could not register its window class".into());
            }
            let host = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("AtlAxWin"),
                CONTROL,
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN,
                0,
                0,
                size.0,
                size.1,
                Some(parent),
                None,
                GetModuleHandleW(None).ok().map(Into::into),
                None,
            )
            .map_err(|e| {
                format!(
                    "Windows' Remote Desktop control could not be created: {}",
                    e.message()
                )
            })?;
            let mut raw: *mut c_void = std::ptr::null_mut();
            let got = (atl.control)(host, &mut raw);
            if got.is_err() || raw.is_null() {
                let _ = DestroyWindow(host);
                return Err("Windows' Remote Desktop control did not start".into());
            }
            let unknown = IUnknown::from_raw(raw);
            let control = match unknown.cast::<IDispatch>() {
                Ok(control) => Disp(control),
                Err(e) => {
                    // No `Rdp` owns the host yet, so nothing else would
                    // take it away before its parent goes.
                    let _ = DestroyWindow(host);
                    return Err(e.message());
                }
            };
            let mut rdp = Rdp {
                host,
                control,
                events: None,
            };
            rdp.events = rdp.listen(parent, message, mark);
            Ok(rdp)
        }
    }

    fn listen(&self, to: HWND, message: u32, mark: u16) -> Option<(IConnectionPoint, u32)> {
        let sink = Sink::new(to, message, mark);
        unsafe {
            let points: IConnectionPointContainer = self.control.0.cast().ok()?;
            let point = points.FindConnectionPoint(&EVENTS).ok()?;
            let cookie = point.Advise(&sink).ok()?;
            Some((point, cookie))
        }
    }

    /// Whether the control's events reach the window. Without them a
    /// refused sign-in would never be heard of.
    pub(super) fn listening(&self) -> bool {
        self.events.is_some()
    }

    /// Starts connecting to this user's child session. The answer comes
    /// as an event.
    pub(super) fn connect(&self, s: &SignIn) -> Result<(), String> {
        let c = &self.control;
        c.put("Server", Var::str("localhost"))?;
        c.put("DesktopWidth", Var::int(s.size.0))?;
        c.put("DesktopHeight", Var::int(s.size.1))?;
        c.put("UserName", Var::str(s.user))?;
        let advanced = c.get("AdvancedSettings7")?.disp()?;
        advanced.put("EnableCredSspSupport", Var::bool(true))?;
        // The server is this PC, so there is no certificate to check.
        advanced.put("AuthenticationLevel", Var::int(0))?;
        let mut password = [Var::str(s.password)];
        let given = advanced.invoke("ClearTextPassword", DISPATCH_PROPERTYPUT, &mut password);
        password[0].wipe();
        given?;
        // The desk has a clipboard of its own, which is half the point:
        // what the agent copies must not land in the human's.
        let _ = advanced.put("RedirectClipboard", Var::bool(false));
        let _ = advanced.put("RedirectPrinters", Var::bool(false));
        // Until the session has taken a new size, the picture is scaled.
        let _ = advanced.put("SmartSizing", Var::bool(true));
        let extended = unsafe {
            let mut raw: *mut c_void = std::ptr::null_mut();
            c.0.query(&EXTENDED, &mut raw)
                .ok()
                .map_err(|e| format!("this Windows has no child sessions: {}", e.message()))?;
            // Held as an IUnknown so it is released like any other.
            IUnknown::from_raw(raw)
        };
        let set = |name: &str, value: Var| unsafe {
            let name = BSTR::from(name);
            let this = extended.as_raw();
            let vtable = *(this as *const *const *const c_void);
            let put: PutProperty = std::mem::transmute(*vtable.add(PUT_PROPERTY));
            put(this, name.as_ptr(), &value.0)
        };
        set("ConnectToChildSession", Var::bool(true))
            .ok()
            .map_err(|e| format!("this Windows has no child sessions: {}", e.message()))?;
        let _ = set("DesktopScaleFactor", Var::uint(s.scale));
        let _ = set("DeviceScaleFactor", Var::uint(100));
        c.call("Connect", vec![]).map(|_| ())
    }

    /// 0 for not connected, 1 for connected, 2 for on the way.
    pub(super) fn connected(&self) -> i32 {
        self.control
            .get("Connected")
            .ok()
            .and_then(|v| v.to_int())
            .unwrap_or(0)
    }

    pub(super) fn disconnect(&self) {
        let _ = self.control.call("Disconnect", vec![]);
    }

    /// What Windows says about a disconnect, in the human's language.
    pub(super) fn describe(&self, reason: i32) -> Option<String> {
        let extended = self
            .control
            .get("ExtendedDisconnectReason")
            .ok()
            .and_then(|v| v.to_int())
            .unwrap_or(0);
        let said = self
            .control
            .call(
                "GetErrorDescription",
                vec![Var::int(reason), Var::int(extended)],
            )
            .ok()?
            .to_text()?;
        let said = said.split_whitespace().collect::<Vec<_>>().join(" ");
        (!said.is_empty()).then_some(said)
    }

    /// Fits the control to its parent's new size, and asks the session
    /// to take that size once the human let go of the edge.
    pub(super) fn resize(&self, size: (i32, i32), settled: bool, scale: u32) {
        unsafe {
            let _ = SetWindowPos(
                self.host,
                None,
                0,
                0,
                size.0,
                size.1,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        if settled && self.connected() == 1 {
            let _ = self.control.call(
                "UpdateSessionDisplaySettings",
                vec![
                    Var::int(size.0),
                    Var::int(size.1),
                    Var::int(size.0),
                    Var::int(size.1),
                    Var::int(0),
                    Var::int(scale as i32),
                    Var::int(100),
                ],
            );
        }
    }

    /// Hands the keyboard to the desk.
    pub(super) fn focus(&self) {
        unsafe {
            let _ = SetFocus(Some(self.host));
        }
    }
}

impl Drop for Rdp {
    fn drop(&mut self) {
        unsafe {
            if let Some((point, cookie)) = self.events.take() {
                let _ = point.Unadvise(cookie);
            }
            let _ = DestroyWindow(self.host);
        }
    }
}

/// The two functions of `atl.dll` that host a control in a window. They
/// are looked up at run time, so a Windows without the file gets a
/// message and not a Horadric that does not start.
struct Atl {
    init: unsafe extern "system" fn() -> i32,
    control: unsafe extern "system" fn(HWND, *mut *mut c_void) -> HRESULT,
}

impl Atl {
    fn load() -> Result<Atl, String> {
        unsafe {
            let dll: HMODULE = LoadLibraryExW(w!("atl.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32)
                .map_err(|e| format!("atl.dll is missing from this Windows: {}", e.message()))?;
            let find = |name: PCSTR| {
                GetProcAddress(dll, name).ok_or_else(|| "atl.dll is not the one expected".to_string())
            };
            Ok(Atl {
                init: std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    unsafe extern "system" fn() -> i32,
                >(find(windows::core::s!("AtlAxWinInit"))?),
                control: std::mem::transmute::<
                    unsafe extern "system" fn() -> isize,
                    unsafe extern "system" fn(HWND, *mut *mut c_void) -> HRESULT,
                >(find(windows::core::s!("AtlAxGetControl"))?),
            })
        }
    }
}

/// en-US, since the member names are the English ones.
const LCID: u32 = 0x0409;

/// An `IDispatch` called by name.
struct Disp(IDispatch);

impl Disp {
    fn get(&self, name: &str) -> Result<Var, String> {
        self.invoke(name, DISPATCH_PROPERTYGET, &mut [])
    }

    fn call(&self, name: &str, mut args: Vec<Var>) -> Result<Var, String> {
        self.invoke(name, DISPATCH_METHOD | DISPATCH_PROPERTYGET, &mut args)
    }

    fn put(&self, name: &str, value: Var) -> Result<(), String> {
        self.invoke(name, DISPATCH_PROPERTYPUT, &mut [value])
            .map(|_| ())
    }

    /// `args` stay the caller's, so a password among them can be wiped
    /// after the call.
    fn invoke(
        &self,
        name: &str,
        flags: DISPATCH_FLAGS,
        args: &mut [Var],
    ) -> Result<Var, String> {
        let fail = |why: String| format!("the Remote Desktop control refused {name}: {why}");
        let wide = HSTRING::from(name);
        let mut id = 0i32;
        unsafe {
            self.0
                .GetIDsOfNames(&GUID::zeroed(), &PCWSTR(wide.as_ptr()), 1, LCID, &mut id)
        }
        .map_err(|e| fail(e.message()))?;
        // IDispatch takes its arguments last first.
        args.reverse();
        let mut put_id = DISPID_PROPERTYPUT;
        let named = flags == DISPATCH_PROPERTYPUT;
        let params = DISPPARAMS {
            rgvarg: args.as_mut_ptr() as *mut VARIANT,
            rgdispidNamedArgs: if named {
                &mut put_id
            } else {
                std::ptr::null_mut()
            },
            cArgs: args.len() as u32,
            cNamedArgs: named as u32,
        };
        let mut result = Var(VARIANT::default());
        let mut excep = EXCEPINFO::default();
        let mut arg_err = 0u32;
        let called = unsafe {
            self.0.Invoke(
                id,
                &GUID::zeroed(),
                LCID,
                flags,
                &params,
                Some(&mut result.0),
                Some(&mut excep),
                Some(&mut arg_err),
            )
        };
        let description = unsafe {
            let d = ManuallyDrop::take(&mut excep.bstrDescription).to_string();
            ManuallyDrop::drop(&mut excep.bstrSource);
            ManuallyDrop::drop(&mut excep.bstrHelpFile);
            d
        };
        match called {
            Ok(()) => Ok(result),
            Err(_) if !description.is_empty() => Err(fail(description)),
            Err(e) => Err(fail(e.message())),
        }
    }
}

/// A `VARIANT` that clears itself, so a BSTR or object in it is freed.
#[repr(transparent)]
struct Var(VARIANT);

impl Drop for Var {
    fn drop(&mut self) {
        let _ = unsafe { VariantClear(&mut self.0) };
    }
}

impl Var {
    fn with(
        vt: VARENUM,
        fill: impl FnOnce(&mut windows::Win32::System::Variant::VARIANT_0_0_0),
    ) -> Var {
        let mut v = VARIANT::default();
        unsafe {
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = vt;
            fill(&mut inner.Anonymous);
        }
        Var(v)
    }

    fn int(i: i32) -> Var {
        Var::with(VT_I4, |u| u.lVal = i)
    }

    fn uint(i: u32) -> Var {
        Var::with(VT_UI4, |u| u.ulVal = i)
    }

    fn bool(b: bool) -> Var {
        Var::with(VT_BOOL, |u| {
            u.boolVal = VARIANT_BOOL(if b { -1 } else { 0 })
        })
    }

    fn str(s: &str) -> Var {
        Var::with(VT_BSTR, |u| u.bstrVal = ManuallyDrop::new(BSTR::from(s)))
    }

    fn vt(&self) -> VARENUM {
        unsafe { self.0.Anonymous.Anonymous.vt }
    }

    /// Overwrites the text it holds, for a password: freeing a string
    /// leaves its letters in memory.
    fn wipe(&mut self) {
        if self.vt() != VT_BSTR {
            return;
        }
        unsafe {
            let text = &self.0.Anonymous.Anonymous.Anonymous.bstrVal;
            let at = text.as_ptr() as *mut u16;
            if !at.is_null() {
                std::slice::from_raw_parts_mut(at, text.len()).fill(0);
            }
        }
    }

    fn disp(&self) -> Result<Disp, String> {
        if self.vt() != VT_DISPATCH {
            return Err("the Remote Desktop control gave no settings".into());
        }
        unsafe { (*self.0.Anonymous.Anonymous.Anonymous.pdispVal).clone() }
            .map(Disp)
            .ok_or_else(|| "the Remote Desktop control gave no settings".to_string())
    }

    fn to_int(&self) -> Option<i32> {
        let v = unsafe { &self.0.Anonymous.Anonymous };
        match v.vt {
            VT_I4 => Some(unsafe { v.Anonymous.lVal }),
            VT_UI4 => Some(unsafe { v.Anonymous.ulVal } as i32),
            windows::Win32::System::Variant::VT_I2 => Some(unsafe { v.Anonymous.iVal } as i32),
            VT_BOOL => Some((unsafe { v.Anonymous.boolVal }.0 != 0) as i32),
            _ => None,
        }
    }

    fn to_text(&self) -> Option<String> {
        (self.vt() == VT_BSTR)
            .then(|| unsafe { (*self.0.Anonymous.Anonymous.Anonymous.bstrVal).to_string() })
    }
}
