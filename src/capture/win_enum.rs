//! Permissive top-level window enumeration (Windows) for apps whose windows the strict
//! OBS enumeration excludes — PDOOM-1274's Siemens NX case: in-app tool/modal windows
//! carry WS_EX_TOOLWINDOW (or are owned windows with a hidden owner), which
//! libobs-window-helper's validators drop, so the app reads as "window-less" even while
//! the user is actively working in it. The bind-zoo spike (spike/permissive-bind-zoo)
//! proved WGC captures both shapes fine once bound, so those filters are OBS-enumeration
//! artifacts, not capture limitations.
//!
//! One filter is NOT an artifact: the spike also proved that binding a window whose title
//! is EMPTY (an obs_id with an empty first segment) crashes OBS natively — an access
//! violation in win-capture.dll!wc_tick on the graphics thread, killing the process
//! mid-recording, 3/3 reproductions. Every selection path in this module therefore keeps
//! a hard `title_len > 0` gate. Do not relax it.

use std::ffi::c_void;

/// Win32 RECT. Declared identically to `window_geometry::Rect` — the `GetWindowRect` extern
/// must agree across modules or rustc emits `clashing_extern_declarations`.
#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(
        callback: unsafe extern "system" fn(*mut c_void, isize) -> i32,
        data: isize,
    ) -> i32;
    fn GetWindowThreadProcessId(hwnd: *mut c_void, pid: *mut u32) -> u32;
    fn GetWindowTextW(hwnd: *mut c_void, buf: *mut u16, max: i32) -> i32;
    fn GetClassNameW(hwnd: *mut c_void, buf: *mut u16, max: i32) -> i32;
    fn GetWindowLongPtrW(hwnd: *mut c_void, index: i32) -> isize;
    fn IsWindowVisible(hwnd: *mut c_void) -> i32;
    fn IsIconic(hwnd: *mut c_void) -> i32;
    fn GetWindowRect(hwnd: *mut c_void, rect: *mut Rect) -> i32;
    fn GetWindow(hwnd: *mut c_void, cmd: u32) -> *mut c_void;
}
#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn QueryFullProcessImageNameW(
        process: *mut c_void,
        flags: u32,
        name: *mut u16,
        size: *mut u32,
    ) -> i32;
}

const GWL_STYLE: i32 = -16;
const GWL_EXSTYLE: i32 = -20;
const GW_OWNER: u32 = 4;
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

/// Smallest window the permissive path will bind. Tool palettes and modal dialogs are
/// comfortably larger; tooltips, IME candidates, and shell popups are smaller. Mirrors the
/// #132 principle (window identity via geometry) without its source-size dependency.
pub(crate) const MIN_PERMISSIVE_WIDTH: i32 = 160;
pub(crate) const MIN_PERMISSIVE_HEIGHT: i32 = 120;

/// One top-level window, unfiltered: everything the permissive selection (and the
/// window-less telemetry) needs to decide or explain, in plain data.
#[derive(Debug, Clone)]
pub(crate) struct RawWindow {
    pub hwnd: isize,
    pub pid: u32,
    pub title_len: usize,
    pub title: String,
    pub class: String,
    /// Exe file NAME with extension ("ugraf.exe") — what obs_id embeds.
    pub exe_name: String,
    /// Exe file STEM ("ugraf") — what app identity matches on.
    pub exe_stem: String,
    pub style: isize,
    pub ex_style: isize,
    pub visible: bool,
    pub iconic: bool,
    pub owner_hwnd: isize,
    pub width: i32,
    pub height: i32,
}

/// Every top-level window on the desktop, no filtering. One `EnumWindows` pass; exe paths
/// resolved once per unique pid. Fields that fail to resolve default to empty/zero rather
/// than dropping the window — the telemetry path needs to SEE unresolvable windows.
pub(crate) fn raw_toplevel_windows() -> Vec<RawWindow> {
    unsafe extern "system" fn collect(hwnd: *mut c_void, data: isize) -> i32 {
        let out = &mut *(data as *mut Vec<*mut c_void>);
        out.push(hwnd);
        1
    }
    let mut handles: Vec<*mut c_void> = Vec::new();
    unsafe {
        EnumWindows(collect, &mut handles as *mut _ as isize);
    }

    let mut exe_cache: std::collections::HashMap<u32, (String, String)> =
        std::collections::HashMap::new();
    let mut out = Vec::with_capacity(handles.len());
    for h in handles {
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(h, &mut pid);
            let (exe_name, exe_stem) = exe_cache
                .entry(pid)
                .or_insert_with(|| exe_of_pid(pid).unwrap_or_default())
                .clone();
            let mut title_buf = [0u16; 512];
            let tlen = GetWindowTextW(h, title_buf.as_mut_ptr(), title_buf.len() as i32);
            let mut class_buf = [0u16; 256];
            let clen = GetClassNameW(h, class_buf.as_mut_ptr(), class_buf.len() as i32);
            let mut rect = Rect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            GetWindowRect(h, &mut rect);
            // A title that FILLS the buffer was truncated (possibly mid-surrogate-pair): a
            // constructed obs_id from it could never exact-match OBS's own full-title read,
            // so a bind would sit not-ready forever and ride the restart ladder. Report it as
            // untitled — unbindable — which degrades to the pause (pre-#141 behavior).
            let truncated = tlen.max(0) as usize >= title_buf.len() - 1;
            let title = String::from_utf16_lossy(&title_buf[..tlen.max(0) as usize]);
            out.push(RawWindow {
                hwnd: h as isize,
                pid,
                title_len: if truncated { 0 } else { title.chars().count() },
                title,
                class: String::from_utf16_lossy(&class_buf[..clen.max(0) as usize]),
                exe_name,
                exe_stem,
                style: GetWindowLongPtrW(h, GWL_STYLE),
                ex_style: GetWindowLongPtrW(h, GWL_EXSTYLE),
                visible: IsWindowVisible(h) != 0,
                iconic: IsIconic(h) != 0,
                owner_hwnd: GetWindow(h, GW_OWNER) as isize,
                width: rect.right - rect.left,
                height: rect.bottom - rect.top,
            });
        }
    }
    out
}

/// `(file name with extension, file stem)` for a pid, matching how libobs-window-helper
/// derives the exe it embeds in obs_id (`full_exe.file_name()`).
fn exe_of_pid(pid: u32) -> Option<(String, String)> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(process);
        if ok == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..size as usize]);
        let name = full.rsplit(['\\', '/']).next()?.to_string();
        let stem = name
            .rsplit_once('.')
            .map(|(s, _)| s.to_string())
            .unwrap_or_else(|| name.clone());
        Some((name, stem))
    }
}

/// Whether the permissive path may bind this window at all. The gates, in order of what
/// they protect:
/// - `title_len > 0`: HARD crash gate — an empty obs_id title segment is a native OBS crash
///   (see module docs). Never relax.
/// - visible + not minimized: WGC needs on-screen content; the strict enumeration agrees.
/// - minimum size: don't bind tooltips/IME/shell popups that briefly take these styles.
pub(crate) fn permissive_bindable(w: &RawWindow) -> bool {
    w.title_len > 0
        && w.visible
        && !w.iconic
        && w.width >= MIN_PERMISSIVE_WIDTH
        && w.height >= MIN_PERMISSIVE_HEIGHT
}

/// Pure selection: the window of `stem` the permissive fallback should bind, from an
/// unfiltered candidate list. `preferred` wins when it belongs to the app and qualifies —
/// callers pass the FOCUSED window (follow-focus: the user is in it, the NX tool-window
/// case) or the CURRENTLY BOUND one (watchdog refresh: keeps the two writers agreeing on
/// one target, the #133 alignment rule). Otherwise the largest qualifying window (the most
/// plausible "main content" heuristic without a z-order read). Returns `None` when nothing
/// qualifies — the caller then pauses (PDOOM-1274 behavior) and emits the window-less
/// telemetry.
pub(crate) fn select_permissive_candidate<'a>(
    candidates: &'a [RawWindow],
    stem: &str,
    preferred: Option<isize>,
) -> Option<&'a RawWindow> {
    let qualifying = || {
        candidates
            .iter()
            .filter(|w| w.exe_stem.eq_ignore_ascii_case(stem) && permissive_bindable(w))
    };
    if let Some(pref) = preferred {
        if let Some(w) = qualifying().find(|w| w.hwnd == pref) {
            return Some(w);
        }
    }
    qualifying().max_by_key(|w| (w.width as i64) * (w.height as i64))
}

/// obs_id construction replicated from libobs-window-helper: `title:class:exe` with each
/// part encoded `#`→`#22` then `:`→`#3A`, in that order. Pinned by unit test; callers
/// prefer the strict enumeration's own obs_id when the hwnd appears there (authoritative),
/// constructing only for windows the strict list excludes.
pub(crate) fn build_obs_id(title: &str, class: &str, exe_name: &str) -> String {
    fn enc(s: &str) -> String {
        s.replace('#', "#22").replace(':', "#3A")
    }
    format!("{}:{}:{}", enc(title), enc(class), enc(exe_name))
}

/// One-line description of a window for the window-less telemetry: enough to classify the
/// NX shape from a participant's shipped logs without a diagnostic session (PDOOM-1274).
pub(crate) fn describe_window(w: &RawWindow) -> String {
    format!(
        "hwnd={:#x} exe={} pid={} title_len={} class={:?} style={:#x} ex_style={:#x} \
         visible={} iconic={} owner={:#x} size={}x{}",
        w.hwnd,
        w.exe_name,
        w.pid,
        w.title_len,
        w.class,
        w.style,
        w.ex_style,
        w.visible,
        w.iconic,
        w.owner_hwnd,
        w.width,
        w.height
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(hwnd: isize, stem: &str, title_len: usize, w: i32, h: i32) -> RawWindow {
        RawWindow {
            hwnd,
            pid: 42,
            title_len,
            title: "T".repeat(title_len),
            class: "c".into(),
            exe_name: format!("{stem}.exe"),
            exe_stem: stem.into(),
            style: 0,
            ex_style: 0,
            visible: true,
            iconic: false,
            owner_hwnd: 0,
            width: w,
            height: h,
        }
    }

    /// Pins the encoding to libobs-window-helper's encode_string exactly: `#` first, then
    /// `:`, per part, joined unencoded. A drift here binds the wrong window or nothing.
    #[test]
    fn obs_id_encoding_matches_helper() {
        assert_eq!(build_obs_id("a:b", "c#d", "e.exe"), "a#3Ab:c#22d:e.exe");
        // '#' is encoded BEFORE ':' — "#:" becomes "#22#3A", never "#22:" re-encoded.
        assert_eq!(build_obs_id("#:", "", "x.exe"), "#22#3A::x.exe");
        assert_eq!(build_obs_id("Extrude", "NXToolWnd", "ugraf.exe"), "Extrude:NXToolWnd:ugraf.exe");
    }

    /// The crash gate: untitled windows are never bindable, whatever else they look like.
    #[test]
    fn untitled_windows_never_qualify() {
        let w = win(1, "ugraf", 0, 1920, 1080);
        assert!(!permissive_bindable(&w));
        assert!(select_permissive_candidate(&[w], "ugraf", Some(1)).is_none());
    }

    #[test]
    fn tiny_hidden_or_iconic_windows_never_qualify() {
        let tiny = win(1, "ugraf", 5, 100, 80);
        let mut hidden = win(2, "ugraf", 5, 800, 600);
        hidden.visible = false;
        let mut iconic = win(3, "ugraf", 5, 800, 600);
        iconic.iconic = true;
        for w in [&tiny, &hidden, &iconic] {
            assert!(!permissive_bindable(w));
        }
        assert!(select_permissive_candidate(&[tiny, hidden, iconic], "ugraf", None).is_none());
    }

    #[test]
    fn foreground_window_wins_over_larger_sibling() {
        let cands = [win(1, "ugraf", 5, 1920, 1080), win(2, "ugraf", 7, 800, 600)];
        let got = select_permissive_candidate(&cands, "ugraf", Some(2)).unwrap();
        assert_eq!(got.hwnd, 2);
    }

    /// The watchdog refresh passes the currently BOUND hwnd as `preferred`: while that
    /// window stays alive and qualifying, a refresh must keep it — re-resolving to the
    /// largest window instead would fight follow-focus over the target, ~2 black frames per
    /// flip (the two-writer churn #133's watchdog alignment eliminated on the strict path).
    #[test]
    fn bound_window_stays_preferred_on_watchdog_refresh() {
        let cands = [win(1, "ugraf", 5, 1920, 1080), win(2, "ugraf", 7, 800, 600)];
        let got = select_permissive_candidate(&cands, "ugraf", Some(2)).unwrap();
        assert_eq!(got.hwnd, 2, "live bound window must be kept");
        // Bound window gone (closed): falls back to largest qualifying.
        let got = select_permissive_candidate(&cands, "ugraf", Some(99)).unwrap();
        assert_eq!(got.hwnd, 1);
    }

    #[test]
    fn without_foreground_largest_qualifying_wins() {
        let cands = [
            win(1, "ugraf", 5, 400, 300),
            win(2, "ugraf", 5, 1200, 900),
            win(3, "firefox", 5, 1920, 1080),
        ];
        let got = select_permissive_candidate(&cands, "ugraf", None).unwrap();
        assert_eq!(got.hwnd, 2);
    }

    /// A foreground window of ANOTHER app must not hijack the selection.
    #[test]
    fn foreign_foreground_is_ignored() {
        let cands = [win(1, "ugraf", 5, 800, 600), win(2, "firefox", 5, 1920, 1080)];
        let got = select_permissive_candidate(&cands, "ugraf", Some(2)).unwrap();
        assert_eq!(got.hwnd, 1);
    }
}
