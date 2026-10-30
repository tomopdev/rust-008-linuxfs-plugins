//! Okno se zprávou pro Total Commander (WCX nemá vlastní API pro dialogy, proto přímo Windows). Mimo Windows (testy)
//! se text jen zapamatuje.

pub const TITLE: &str = "LinuxFS";

#[cfg(windows)]
pub fn warning(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, MB_ICONWARNING, MB_OK, MessageBoxW};
    let mut t: Vec<u16> = text.encode_utf16().collect();
    t.push(0);
    let mut c: Vec<u16> = TITLE.encode_utf16().collect();
    c.push(0);
    // SAFETY: NUL-terminated buffers; the foreground window (Total Commander's) or NULL is a valid owner.
    unsafe {
        MessageBoxW(GetForegroundWindow(), t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONWARNING);
    }
}

#[cfg(not(windows))]
thread_local! {
    static LOG: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(not(windows))]
pub fn warning(text: &str) {
    LOG.with(|l| l.borrow_mut().push(text.to_string()));
}

/// Test hook: texts of the windows shown on this thread (and clears them).
#[cfg(not(windows))]
pub fn take_messages() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}
