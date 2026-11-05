//! Office files for the Forge: a `.pptx` becomes one PNG per slide, a
//! `.docx` becomes a PDF, so the Forge can show them without an Office
//! renderer of its own.
//!
//! The converters, best first: PowerPoint or Word itself over COM (exact,
//! it is the program that made the file), else LibreOffice headless (close).
//! LibreOffice has no per slide PNG export, and rasterising a PDF would
//! need a dependency, so a deck converted by LibreOffice comes back as a
//! [`Converted::Pdf`] the browser pane shows instead.
//!
//! Every result is cached under `cache_root/<sha256 of the bytes>/`, so an
//! unchanged file is never converted twice. A cache made by LibreOffice
//! stays a PDF even if PowerPoint is installed later; clearing the cache
//! folder is the way to redo it.
//!
//! The work blocks. [`convert_async`] puts it on a thread of its own.
//! Office automation has no typelib here, so methods are called by name
//! through `IDispatch` ([`Disp`] in the Windows part).

use std::fmt;
use std::path::{Path, PathBuf};

/// What a file became.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Converted {
    /// One PNG per slide, in slide order.
    Slides(Vec<PathBuf>),
    Pdf(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Converter {
    PowerPoint,
    Word,
    LibreOffice,
}

#[derive(Debug)]
pub enum OfficeError {
    /// Not Windows: the COM converters do not exist and LibreOffice is not
    /// wired up there yet.
    Unsupported,
    /// The file is not a deck or a document this module knows.
    NotOffice(PathBuf),
    /// Nothing installed can convert this kind of file.
    NoConverter { deck: bool },
    Io(std::io::Error),
    /// A converter ran and failed, with what it said.
    Failed { by: Converter, why: String },
    /// A converter took longer than [`TIMEOUT`] and was given up on.
    TimedOut(Converter),
}

impl fmt::Display for OfficeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OfficeError::Unsupported => {
                write!(f, "Office files are only converted on Windows for now")
            }
            OfficeError::NotOffice(p) => write!(f, "{} is not a .pptx or .docx", p.display()),
            OfficeError::NoConverter { deck: true } => write!(
                f,
                "To show slides, install PowerPoint (exact) or LibreOffice (close)"
            ),
            OfficeError::NoConverter { deck: false } => write!(
                f,
                "To show documents, install Word (exact) or LibreOffice (close)"
            ),
            OfficeError::Io(e) => write!(f, "{e}"),
            OfficeError::Failed { by, why } => write!(f, "{by:?} could not convert it: {why}"),
            OfficeError::TimedOut(by) => write!(
                f,
                "{by:?} took more than {} seconds and was stopped",
                TIMEOUT.as_secs()
            ),
        }
    }
}

impl std::error::Error for OfficeError {}

impl From<std::io::Error> for OfficeError {
    fn from(e: std::io::Error) -> Self {
        OfficeError::Io(e)
    }
}

/// Long enough for a big deck on a slow disk, short enough that a modal
/// dialog in a hidden Office does not leave the Forge waiting for ever.
pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The width a slide is exported at; the height follows the deck's aspect.
pub const SLIDE_WIDTH: u32 = 1920;

/// The converters installed on this machine, best first.
pub fn available() -> Vec<Converter> {
    #[cfg(windows)]
    {
        win::available()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Converts `src`, or returns the cached result for the same bytes.
pub fn convert(src: &Path, cache_root: &Path) -> Result<Converted, OfficeError> {
    #[cfg(windows)]
    {
        win::convert(src, cache_root)
    }
    #[cfg(not(windows))]
    {
        let _ = (src, cache_root);
        Err(OfficeError::Unsupported)
    }
}

/// [`convert`] on a thread of its own, calling `done` there when finished.
pub fn convert_async(
    src: &Path,
    cache_root: &Path,
    done: impl FnOnce(Result<Converted, OfficeError>) + Send + 'static,
) {
    let src = src.to_path_buf();
    let cache_root = cache_root.to_path_buf();
    std::thread::Builder::new()
        .name("forge-office".into())
        .spawn(move || done(convert(&src, &cache_root)))
        .expect("spawning the Office conversion thread");
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Deck,
    Document,
}

impl Kind {
    fn of(path: &Path) -> Option<Kind> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "pptx" | "ppt" => Some(Kind::Deck),
            "docx" | "doc" => Some(Kind::Document),
            _ => None,
        }
    }
}

/// The file in a cache folder that says it is complete and what it holds.
/// Written last, so a folder without it is a conversion that never finished.
const DONE: &str = "done";
const PDF_NAME: &str = "doc.pdf";

fn slide_path(dir: &Path, n: usize) -> PathBuf {
    dir.join(format!("slide-{n:03}.png"))
}

fn done_text(c: &Converted) -> String {
    match c {
        Converted::Slides(s) => format!("slides {}", s.len()),
        Converted::Pdf(_) => "pdf".to_string(),
    }
}

/// Reads a finished cache folder back, or `None` when it is missing or
/// incomplete (a file deleted by hand counts as missing).
fn cached(dir: &Path) -> Option<Converted> {
    let text = std::fs::read_to_string(dir.join(DONE)).ok()?;
    let found = match text.trim() {
        "pdf" => Converted::Pdf(dir.join(PDF_NAME)),
        other => {
            let n: usize = other.strip_prefix("slides ")?.parse().ok()?;
            Converted::Slides((1..=n).map(|i| slide_path(dir, i)).collect())
        }
    };
    let all_there = match &found {
        Converted::Pdf(p) => p.is_file(),
        Converted::Slides(s) => !s.is_empty() && s.iter().all(|p| p.is_file()),
    };
    all_there.then_some(found)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(windows)]
mod win {
    use super::*;
    use std::mem::ManuallyDrop;
    use std::process::Command;
    use std::sync::{mpsc, Mutex};
    use std::time::Instant;

    use windows::core::{BSTR, GUID, HSTRING, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, DISP_E_PARAMNOTFOUND, VARIANT_BOOL};
    use windows::Win32::System::Com::{
        CLSIDFromProgID, CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch,
        CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, DISPATCH_FLAGS, DISPATCH_METHOD,
        DISPATCH_PROPERTYGET, DISPATCH_PROPERTYPUT, DISPPARAMS, EXCEPINFO,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Ole::DISPID_PROPERTYPUT;
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    use windows::Win32::System::Variant::{
        VariantChangeType, VariantClear, VAR_CHANGE_FLAGS, VARENUM, VARIANT, VT_BOOL, VT_BSTR,
        VT_DISPATCH, VT_ERROR, VT_I4, VT_R8,
    };

    /// One conversion at a time. Two at once would both see PowerPoint as
    /// not running, both start using the one instance, and the first to
    /// finish would quit it under the other.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    const POWERPOINT: &str = "PowerPoint.Application";
    const WORD: &str = "Word.Application";

    pub fn available() -> Vec<Converter> {
        let mut found = Vec::new();
        if registered(POWERPOINT) {
            found.push(Converter::PowerPoint);
        }
        if registered(WORD) {
            found.push(Converter::Word);
        }
        if soffice().is_some() {
            found.push(Converter::LibreOffice);
        }
        found
    }

    fn registered(prog_id: &str) -> bool {
        unsafe { CLSIDFromProgID(&HSTRING::from(prog_id)) }.is_ok()
    }

    pub fn convert(src: &Path, cache_root: &Path) -> Result<Converted, OfficeError> {
        let kind = Kind::of(src).ok_or_else(|| OfficeError::NotOffice(src.to_path_buf()))?;
        let bytes = std::fs::read(src)?;
        let hash = crate::update::sha256(&bytes).map_err(std::io::Error::other)?;
        let dir = cache_root.join(hex(&hash));
        if let Some(done) = cached(&dir) {
            return Ok(done);
        }
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
        // The call before ours may have converted the same bytes.
        if let Some(done) = cached(&dir) {
            return Ok(done);
        }

        let work = cache_root.join(format!("{}.part", hex(&hash)));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work)?;
        // Converting a copy of the hashed bytes keeps the cache true to its
        // name even if the file changes while Office is busy with it.
        let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("bin");
        let copy = work.join(format!("source.{ext}"));
        std::fs::write(&copy, &bytes)?;
        let copy = std::path::absolute(&copy)?;
        let out = std::path::absolute(&work)?;

        let result = run_converters(kind, &copy, &out, cache_root);
        let _ = std::fs::remove_file(&copy);
        let converted = match result {
            Ok(c) => c,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&work);
                return Err(e);
            }
        };
        std::fs::write(work.join(DONE), done_text(&converted))?;
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::rename(&work, &dir)?;
        cached(&dir).ok_or_else(|| {
            std::io::Error::other("the converted files went missing from the cache").into()
        })
    }

    /// Tries each installed converter in order and returns the first
    /// success, or the first failure when none succeeds.
    fn run_converters(
        kind: Kind,
        src: &Path,
        out: &Path,
        cache_root: &Path,
    ) -> Result<Converted, OfficeError> {
        let office = match kind {
            Kind::Deck => (Converter::PowerPoint, POWERPOINT),
            Kind::Document => (Converter::Word, WORD),
        };
        let mut first_error = None;
        if registered(office.1) {
            match through_office(office.0, src, out) {
                Ok(c) => return Ok(c),
                Err(e) => first_error = Some(e),
            }
        }
        if let Some(soffice) = soffice() {
            match libreoffice(&soffice, src, out, cache_root) {
                Ok(c) => return Ok(c),
                Err(e) => first_error = first_error.or(Some(e)),
            }
        }
        Err(first_error.unwrap_or(OfficeError::NoConverter {
            deck: kind == Kind::Deck,
        }))
    }

    fn through_office(by: Converter, src: &Path, out: &Path) -> Result<Converted, OfficeError> {
        let exe = match by {
            Converter::PowerPoint => "POWERPNT.EXE",
            _ => "WINWORD.EXE",
        };
        let before = pids(exe);
        let (tx, rx) = mpsc::channel();
        let (src, out) = (src.to_path_buf(), out.to_path_buf());
        let before_in = before.clone();
        std::thread::Builder::new()
            .name("forge-office-com".into())
            .spawn(move || {
                let r = match by {
                    Converter::PowerPoint => powerpoint(&src, &out, exe, &before_in),
                    _ => word(&src, &out, exe, &before_in),
                };
                let _ = tx.send(r);
            })?;
        match rx.recv_timeout(TIMEOUT) {
            Ok(r) => r,
            Err(_) => {
                // Only a process this call started is ours to kill; the
                // user's own PowerPoint is left alone even when stuck. Killing
                // ours also frees the worker thread from its COM call.
                for pid in pids(exe).into_iter().filter(|p| !before.contains(p)) {
                    kill(pid);
                }
                Err(OfficeError::TimedOut(by))
            }
        }
    }

    /// Initialises COM for this thread and undoes it when dropped, which
    /// must come after every COM object made on the thread is released.
    struct Apartment;

    impl Apartment {
        fn enter() -> Apartment {
            // An error means COM was already set up on this thread in some
            // mode; the calls below still work in either apartment.
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            Apartment
        }
    }

    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    /// Starts the Office app, or attaches to the one already running, and
    /// says whether it is ours: a new process appeared, so nobody else is
    /// using it and quitting it afterwards is ours to do.
    fn start(prog_id: &str, exe: &str, before: &[u32], by: Converter) -> Result<(Disp, bool), OfficeError> {
        let fail = |why: String| OfficeError::Failed { by, why };
        let clsid = unsafe { CLSIDFromProgID(&HSTRING::from(prog_id)) }.map_err(|e| fail(e.message()))?;
        let app: IDispatch = unsafe { CoCreateInstance(&clsid, None, CLSCTX_LOCAL_SERVER) }
            .map_err(|e| fail(e.message()))?;
        let ours = pids(exe).iter().any(|p| !before.contains(p));
        Ok((Disp { obj: app, by }, ours))
    }

    /// Quits an app this call started, unless the user opened something in
    /// it meanwhile (the hidden instance PowerPoint started for us is the
    /// one their next double click lands in).
    fn finish(app: &Disp, ours: bool, open_list: &str) {
        if !ours {
            return;
        }
        let still_open = app
            .get(open_list)
            .and_then(|l| l.disp(app.by))
            .and_then(|l| l.get("Count"))
            .and_then(|c| c.to_int(app.by))
            .unwrap_or(0);
        if still_open == 0 {
            let _ = app.call("Quit", vec![]);
        }
    }

    fn powerpoint(src: &Path, out: &Path, exe: &str, before: &[u32]) -> Result<Converted, OfficeError> {
        let _com = Apartment::enter();
        let by = Converter::PowerPoint;
        let (app, ours) = start(POWERPOINT, exe, before, by)?;
        if ours {
            // ppAlertsNone: a hidden PowerPoint must not stop on a dialog.
            let _ = app.put("DisplayAlerts", Var::int(1));
        }
        let result = (|| {
            const MSO_TRUE: i32 = -1;
            const MSO_FALSE: i32 = 0;
            let deck = app.get("Presentations")?.disp(by)?.call(
                "Open",
                vec![
                    Var::str(&src.to_string_lossy()),
                    Var::int(MSO_TRUE),  // ReadOnly
                    Var::int(MSO_TRUE),  // Untitled
                    Var::int(MSO_FALSE), // WithWindow
                ],
            )?;
            let deck = deck.disp(by)?;
            let slides = export_slides(&deck, out);
            let _ = deck.call("Close", vec![]);
            slides
        })();
        finish(&app, ours, "Presentations");
        result
    }

    fn export_slides(deck: &Disp, out: &Path) -> Result<Converted, OfficeError> {
        let by = deck.by;
        let setup = deck.get("PageSetup")?.disp(by)?;
        let w = setup.get("SlideWidth")?.to_float(by)?;
        let h = setup.get("SlideHeight")?.to_float(by)?;
        let height = if w > 0.0 {
            (SLIDE_WIDTH as f64 * h / w).round() as i32
        } else {
            (SLIDE_WIDTH * 9 / 16) as i32
        };
        let slides = deck.get("Slides")?.disp(by)?;
        let count = slides.get("Count")?.to_int(by)?;
        let mut files = Vec::new();
        for i in 1..=count {
            let file = slide_path(out, i as usize);
            slides.call("Item", vec![Var::int(i)])?.disp(by)?.call(
                "Export",
                vec![
                    Var::str(&file.to_string_lossy()),
                    Var::str("PNG"),
                    Var::int(SLIDE_WIDTH as i32),
                    Var::int(height),
                ],
            )?;
            files.push(file);
        }
        if files.is_empty() {
            return Err(OfficeError::Failed { by, why: "the deck has no slides".into() });
        }
        Ok(Converted::Slides(files))
    }

    fn word(src: &Path, out: &Path, exe: &str, before: &[u32]) -> Result<Converted, OfficeError> {
        let _com = Apartment::enter();
        let by = Converter::Word;
        let (app, ours) = start(WORD, exe, before, by)?;
        if ours {
            // wdAlertsNone, for the same reason as PowerPoint's.
            let _ = app.put("DisplayAlerts", Var::int(0));
        }
        let result = (|| {
            let doc = app.get("Documents")?.disp(by)?.call(
                "Open",
                vec![
                    Var::str(&src.to_string_lossy()),
                    Var::bool(false), // ConfirmConversions
                    Var::bool(true),  // ReadOnly
                    Var::bool(false), // AddToRecentFiles
                    Var::missing(),   // PasswordDocument
                    Var::missing(),   // PasswordTemplate
                    Var::missing(),   // Revert
                    Var::missing(),   // WritePasswordDocument
                    Var::missing(),   // WritePasswordTemplate
                    Var::missing(),   // Format
                    Var::missing(),   // Encoding
                    Var::bool(false), // Visible
                ],
            )?;
            let doc = doc.disp(by)?;
            let pdf = out.join(PDF_NAME);
            const WD_EXPORT_FORMAT_PDF: i32 = 17;
            let exported = doc.call(
                "ExportAsFixedFormat",
                vec![Var::str(&pdf.to_string_lossy()), Var::int(WD_EXPORT_FORMAT_PDF)],
            );
            const WD_DO_NOT_SAVE_CHANGES: i32 = 0;
            let _ = doc.call("Close", vec![Var::int(WD_DO_NOT_SAVE_CHANGES)]);
            exported.map(|_| Converted::Pdf(pdf))
        })();
        finish(&app, ours, "Documents");
        result
    }

    /// An `IDispatch` called by name, remembering which app it belongs to
    /// so its errors say so.
    struct Disp {
        obj: IDispatch,
        by: Converter,
    }

    /// en-US, since the member names are the English ones.
    const LCID: u32 = 0x0409;

    impl Disp {
        fn get(&self, name: &str) -> Result<Var, OfficeError> {
            self.invoke(name, DISPATCH_METHOD | DISPATCH_PROPERTYGET, vec![])
        }

        fn call(&self, name: &str, args: Vec<Var>) -> Result<Var, OfficeError> {
            self.invoke(name, DISPATCH_METHOD | DISPATCH_PROPERTYGET, args)
        }

        fn put(&self, name: &str, value: Var) -> Result<Var, OfficeError> {
            self.invoke(name, DISPATCH_PROPERTYPUT, vec![value])
        }

        fn invoke(
            &self,
            name: &str,
            flags: DISPATCH_FLAGS,
            mut args: Vec<Var>,
        ) -> Result<Var, OfficeError> {
            let fail = |why: String| OfficeError::Failed { by: self.by, why: format!("{name}: {why}") };
            let wide = HSTRING::from(name);
            let mut id = 0i32;
            unsafe {
                self.obj
                    .GetIDsOfNames(&GUID::zeroed(), &PCWSTR(wide.as_ptr()), 1, LCID, &mut id)
            }
            .map_err(|e| fail(e.message()))?;
            // IDispatch takes its arguments last first.
            args.reverse();
            let mut put_id = DISPID_PROPERTYPUT;
            let named = flags == DISPATCH_PROPERTYPUT;
            let params = DISPPARAMS {
                rgvarg: args.as_mut_ptr() as *mut VARIANT,
                rgdispidNamedArgs: if named { &mut put_id } else { std::ptr::null_mut() },
                cArgs: args.len() as u32,
                cNamedArgs: named as u32,
            };
            let mut result = Var(VARIANT::default());
            let mut excep = EXCEPINFO::default();
            let mut arg_err = 0u32;
            let called = unsafe {
                self.obj.Invoke(
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
        fn with(vt: VARENUM, fill: impl FnOnce(&mut windows::Win32::System::Variant::VARIANT_0_0_0)) -> Var {
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

        fn bool(b: bool) -> Var {
            Var::with(VT_BOOL, |u| u.boolVal = VARIANT_BOOL(if b { -1 } else { 0 }))
        }

        fn str(s: &str) -> Var {
            Var::with(VT_BSTR, |u| u.bstrVal = ManuallyDrop::new(BSTR::from(s)))
        }

        /// An optional argument left out, so later ones can be given.
        fn missing() -> Var {
            Var::with(VT_ERROR, |u| u.scode = DISP_E_PARAMNOTFOUND.0)
        }

        fn vt(&self) -> VARENUM {
            unsafe { self.0.Anonymous.Anonymous.vt }
        }

        fn disp(&self, by: Converter) -> Result<Disp, OfficeError> {
            let obj = if self.vt() == VT_DISPATCH {
                unsafe { (*self.0.Anonymous.Anonymous.Anonymous.pdispVal).clone() }
            } else {
                None
            };
            obj.map(|obj| Disp { obj, by }).ok_or_else(|| OfficeError::Failed {
                by,
                why: format!("expected an object, got variant type {}", self.vt().0),
            })
        }

        fn changed(&self, to: VARENUM, by: Converter) -> Result<Var, OfficeError> {
            let mut out = Var(VARIANT::default());
            unsafe { VariantChangeType(&mut out.0, &self.0, VAR_CHANGE_FLAGS(0), to) }
                .map_err(|e| OfficeError::Failed { by, why: e.message() })?;
            Ok(out)
        }

        fn to_int(&self, by: Converter) -> Result<i32, OfficeError> {
            let v = self.changed(VT_I4, by)?;
            Ok(unsafe { v.0.Anonymous.Anonymous.Anonymous.lVal })
        }

        fn to_float(&self, by: Converter) -> Result<f64, OfficeError> {
            let v = self.changed(VT_R8, by)?;
            Ok(unsafe { v.0.Anonymous.Anonymous.Anonymous.dblVal })
        }
    }

    /// The ids of every running process with this executable name.
    fn pids(exe: &str) -> Vec<u32> {
        let mut found = Vec::new();
        let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
            return found;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = unsafe { Process32FirstW(snap, &mut entry) }.is_ok();
        while more {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            if String::from_utf16_lossy(&entry.szExeFile[..len]).eq_ignore_ascii_case(exe) {
                found.push(entry.th32ProcessID);
            }
            more = unsafe { Process32NextW(snap, &mut entry) }.is_ok();
        }
        let _ = unsafe { CloseHandle(snap) };
        found
    }

    fn kill(pid: u32) {
        if let Ok(h) = unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) } {
            let _ = unsafe { TerminateProcess(h, 1) };
            let _ = unsafe { CloseHandle(h) };
        }
    }

    /// LibreOffice's `soffice.exe`, from its usual install folders or PATH.
    fn soffice() -> Option<PathBuf> {
        let mut places: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"]
            .iter()
            .filter_map(std::env::var_os)
            .map(|root| PathBuf::from(root).join(r"LibreOffice\program\soffice.exe"))
            .collect();
        if let Some(path) = std::env::var_os("PATH") {
            places.extend(std::env::split_paths(&path).map(|d| d.join("soffice.exe")));
        }
        places.into_iter().find(|p| p.is_file())
    }

    /// A file URL for LibreOffice's `-env:` option, which wants one.
    fn file_url(path: &Path) -> String {
        let mut url = String::from("file:///");
        for b in path.to_string_lossy().replace('\\', "/").bytes() {
            match b {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'/' | b':' | b'.' | b'-' | b'_' => {
                    url.push(b as char)
                }
                _ => url.push_str(&format!("%{b:02X}")),
            }
        }
        url
    }

    fn libreoffice(
        soffice: &Path,
        src: &Path,
        out: &Path,
        cache_root: &Path,
    ) -> Result<Converted, OfficeError> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let by = Converter::LibreOffice;
        // A profile of our own: with the user's profile, a LibreOffice they
        // have open swallows the request and nothing is converted.
        let profile = std::path::absolute(cache_root.join("libreoffice-profile"))?;
        let before = pids("soffice.bin");
        let mut child = Command::new(soffice)
            .arg(format!("-env:UserInstallation={}", file_url(&profile)))
            .args(["--headless", "--norestore", "--nologo", "--convert-to", "pdf", "--outdir"])
            .arg(out)
            .arg(src)
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() > TIMEOUT {
                let _ = child.kill();
                // soffice.exe is a launcher; the work happens in soffice.bin.
                for pid in pids("soffice.bin").into_iter().filter(|p| !before.contains(p)) {
                    kill(pid);
                }
                return Err(OfficeError::TimedOut(by));
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        let made = src.with_extension("pdf");
        let made = out.join(made.file_name().unwrap_or_default());
        if !made.is_file() {
            return Err(OfficeError::Failed {
                by,
                why: format!("soffice exited with {status} and wrote no PDF"),
            });
        }
        let pdf = out.join(PDF_NAME);
        std::fs::rename(&made, &pdf)?;
        Ok(Converted::Pdf(pdf))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn file_urls_escape_spaces() {
            assert_eq!(
                file_url(Path::new(r"C:\Users\a b\x")),
                "file:///C:/Users/a%20b/x"
            );
        }

        /// Converts real files with whatever is installed. Set
        /// `FORGE_OFFICE_SAMPLES` to a folder holding `test.pptx` and
        /// `test.docx`; the results land in its `cache` folder.
        #[test]
        #[ignore]
        fn converts_real_files() {
            let dir = PathBuf::from(std::env::var("FORGE_OFFICE_SAMPLES").expect("FORGE_OFFICE_SAMPLES"));
            let cache = dir.join("cache");
            eprintln!("available: {:?}", available());
            for name in ["test.pptx", "test.docx"] {
                let t = Instant::now();
                let first = convert(&dir.join(name), &cache);
                eprintln!("{name}: {first:?} in {:?}", t.elapsed());
                let first = first.expect("converted");
                let t = Instant::now();
                let again = convert(&dir.join(name), &cache).expect("cached");
                eprintln!("{name} again in {:?}", t.elapsed());
                assert_eq!(first, again);
                assert!(t.elapsed() < std::time::Duration::from_millis(500));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_extension() {
        assert_eq!(Kind::of(Path::new("a/Deck.PPTX")), Some(Kind::Deck));
        assert_eq!(Kind::of(Path::new("notes.docx")), Some(Kind::Document));
        assert_eq!(Kind::of(Path::new("notes.md")), None);
        assert_eq!(Kind::of(Path::new("pptx")), None);
    }

    #[test]
    fn a_cache_folder_is_only_used_when_complete() {
        let dir = std::env::temp_dir().join(format!("forge-office-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let slides = Converted::Slides(vec![slide_path(&dir, 1), slide_path(&dir, 2)]);
        std::fs::write(dir.join(DONE), done_text(&slides)).unwrap();
        std::fs::write(slide_path(&dir, 1), b"png").unwrap();
        assert_eq!(cached(&dir), None, "slide 2 is missing");
        std::fs::write(slide_path(&dir, 2), b"png").unwrap();
        assert_eq!(cached(&dir), Some(slides));
        std::fs::write(dir.join(DONE), "pdf").unwrap();
        assert_eq!(cached(&dir), None, "no doc.pdf yet");
        std::fs::write(dir.join(PDF_NAME), b"%PDF").unwrap();
        assert_eq!(cached(&dir), Some(Converted::Pdf(dir.join(PDF_NAME))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
