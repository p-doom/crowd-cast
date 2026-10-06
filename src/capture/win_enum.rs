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

#[link(name = "dwmapi")]
extern "system" {
    fn DwmGetWindowAttribute(hwnd: *mut c_void, attr: u32, value: *mut c_void, size: u32) -> i32;
}

const DWMWA_CLOAKED: u32 = 14;

/// The owner of `hwnd` (`GetWindow(GW_OWNER)`), 0 for an unowned top-level window. An owned
/// window is a dialog/palette of a main window (#137: the dead-dialog evidence, and the
/// main-window-first order at scene creation).
pub(crate) fn window_owner(hwnd: isize) -> isize {
    unsafe { GetWindow(hwnd as *mut c_void, GW_OWNER) as isize }
}

/// One-line description of a single (bound) window for the dead-with-window diagnostic
/// (#137): class, title, size, owner, cloaked, minimized, visible, foreground. Enough to find
/// the WGC-level reason a titled, visible window never delivers frames from shipped logs.
pub(crate) fn describe_bound_window(hwnd: isize) -> String {
    unsafe {
        let h = hwnd as *mut c_void;
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
        let mut cloaked: u32 = 0;
        let cloaked_hr = DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED,
            &mut cloaked as *mut u32 as *mut c_void,
            std::mem::size_of::<u32>() as u32,
        );
        let cloaked = if cloaked_hr == 0 {
            format!("{cloaked:#x}")
        } else {
            "unknown".to_string()
        };
        let owner = GetWindow(h, GW_OWNER) as isize;
        format!(
            "hwnd={:#x} class={:?} title={:?} size={}x{} owned={} owner={:#x} cloaked={} \
             minimized={} visible={} foreground={} style={:#x} ex_style={:#x}",
            hwnd,
            String::from_utf16_lossy(&class_buf[..clen.max(0) as usize]),
            String::from_utf16_lossy(&title_buf[..tlen.max(0) as usize]),
            rect.right - rect.left,
            rect.bottom - rect.top,
            owner != 0,
            owner,
            cloaked,
            IsIconic(h) != 0,
            IsWindowVisible(h) != 0,
            super::window_geometry::foreground_hwnd() == hwnd,
            GetWindowLongPtrW(h, GWL_STYLE),
            GetWindowLongPtrW(h, GWL_EXSTYLE),
        )
    }
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

    // ================================================================================
    // Additional adversarial tests (independent review pass on PR #141/#142) — realistic
    // Siemens NX window shapes, encoding hazards, and known selection-heuristic gaps. Run and
    // passing (verified against a standalone copy of these pure functions, native macOS,
    // since this module only compiles under `#[cfg(target_os = "windows")]`; see the review
    // notes for how they were checked against the actual Windows target).
    // ================================================================================

    /// Full-featured constructor for adversarial scenarios: unlike `win`, exposes ex_style and
    /// owner_hwnd independently so a test can shape a window exactly like a real
    /// NX/tool-window/tooltip artifact (owned, WS_EX_TOOLWINDOW-styled, etc).
    fn win_ex(
        hwnd: isize,
        stem: &str,
        title: &str,
        w: i32,
        h: i32,
        visible: bool,
        iconic: bool,
        ex_style: isize,
        owner_hwnd: isize,
    ) -> RawWindow {
        RawWindow {
            hwnd,
            pid: 42,
            title_len: title.chars().count(),
            title: title.to_string(),
            class: "c".into(),
            exe_name: format!("{stem}.exe"),
            exe_stem: stem.into(),
            style: 0,
            ex_style,
            visible,
            iconic,
            owner_hwnd,
            width: w,
            height: h,
        }
    }

    const WS_EX_TOOLWINDOW: isize = 0x0000_0080;

    /// The motivating shape (PDOOM-1274): NX's main frame is untitled in this scenario, an
    /// untitled owned helper window sits alongside it, and a titled WS_EX_TOOLWINDOW tool
    /// palette (owned, hidden owner) is what the user is actually working in. The permissive
    /// resolver must land on the titled tool window, never the untitled ones — style bits
    /// (WS_EX_TOOLWINDOW, ownership) are NOT among `permissive_bindable`'s gates, only
    /// title/visibility/size are, so this also proves style is irrelevant to the outcome.
    #[test]
    fn nx_realistic_shape_picks_the_titled_tool_window() {
        let main_frame_untitled = win_ex(1, "ugraf", "", 1920, 1040, true, false, 0, 0);
        let untitled_owned_helper = win_ex(2, "ugraf", "", 300, 200, true, false, 0, 1);
        let tool_window = win_ex(3, "ugraf", "Extrude", 640, 480, true, false, WS_EX_TOOLWINDOW, 1);
        let cands = [main_frame_untitled, untitled_owned_helper, tool_window];
        let got = select_permissive_candidate(&cands, "ugraf", None).unwrap();
        assert_eq!(got.hwnd, 3, "only the titled tool window may be selected");
    }

    /// Tooltips and IME candidate windows are visible, titled, and can be owned — only their
    /// small size should disqualify them. Spread of realistic tiny sizes, including one pixel
    /// short of each individual threshold.
    #[test]
    fn tooltip_and_ime_popup_sizes_all_excluded() {
        let shapes = [(40, 20), (300, 40), (159, 200), (200, 119)];
        for (w, h) in shapes {
            let popup = win_ex(10, "ugraf", "Tooltip", w, h, true, false, 0, 0);
            assert!(!permissive_bindable(&popup), "{}x{} should not qualify", w, h);
        }
    }

    /// The documented minimum is inclusive (>=): exactly 160x120 must qualify, one pixel
    /// under on either axis must not.
    #[test]
    fn minimum_size_boundary_is_inclusive() {
        let exact = win_ex(1, "ugraf", "T", 160, 120, true, false, 0, 0);
        assert!(permissive_bindable(&exact));
        let short_w = win_ex(2, "ugraf", "T", 159, 120, true, false, 0, 0);
        assert!(!permissive_bindable(&short_w));
        let short_h = win_ex(3, "ugraf", "T", 160, 119, true, false, 0, 0);
        assert!(!permissive_bindable(&short_h));
    }

    /// A same-exe splash/about/print-preview overlay that happens to be LARGER than the
    /// window the user is actually working in. `select_permissive_candidate` has no concept
    /// of "main content" beyond area, so — documented limitation, not a crash — the overlay
    /// wins when nothing is preferred. Pins the behavior so a change to the heuristic is a
    /// deliberate, reviewed decision rather than a silent drift.
    #[test]
    fn oversized_same_exe_overlay_can_win_over_the_real_work_window() {
        let real_work_window = win_ex(1, "ugraf", "Assembly1.prt", 1280, 900, true, false, 0, 0);
        let splash_overlay = win_ex(2, "ugraf", "About Siemens NX", 1920, 1080, true, false, 0, 0);
        let cands = [real_work_window, splash_overlay];
        let got = select_permissive_candidate(&cands, "ugraf", None).unwrap();
        assert_eq!(got.hwnd, 2, "largest-by-area picked the overlay, not the work window — known limitation");
    }

    /// `select_permissive_candidate` has no window-placement awareness at all — no monitor,
    /// no virtual desktop. A large window physically on a DIFFERENT monitor than the one the
    /// user is looking at competes on equal footing with the window under the user's cursor.
    #[test]
    fn largest_by_area_has_no_monitor_locality_awareness() {
        let window_user_is_looking_at = win_ex(1, "ugraf", "Assembly1.prt", 1280, 900, true, false, 0, 0);
        let window_on_the_other_monitor = win_ex(2, "ugraf", "Reference.prt", 2560, 1440, true, false, 0, 0);
        let cands = [window_user_is_looking_at, window_on_the_other_monitor];
        let got = select_permissive_candidate(&cands, "ugraf", None).unwrap();
        assert_eq!(got.hwnd, 2, "bigger-but-elsewhere wins with no monitor awareness");
    }

    /// `RawWindow` carries no DWM-cloaked / virtual-desktop flag, so a window parked on a
    /// different virtual desktop — which `IsWindowVisible` still reports as visible; only DWM
    /// cloaking hides it — is indistinguishable here from a genuinely on-screen window. The
    /// strict validator explicitly excludes cloaked windows (`is_window_cloaked` in
    /// libobs-window-helper); this permissive path has no equivalent check. This test does not
    /// prove a runtime bug (nothing here can construct a "cloaked" RawWindow); it documents
    /// that the data model cannot express the distinction, which is itself the gap.
    #[test]
    fn visible_flag_alone_cannot_express_dwm_cloaking() {
        let plausibly_cloaked = win_ex(1, "ugraf", "Extrude", 640, 480, true, false, 0, 0);
        assert!(
            permissive_bindable(&plausibly_cloaked),
            "current data model has no way to exclude a cloaked-but-visible window"
        );
    }

    /// Zero and negative rects (e.g. `GetWindowRect` failing on a window destroyed between
    /// enumeration and this check — TOCTOU) must degrade to "not bindable", never panic or
    /// flow into the area comparison as a negative/garbage value.
    #[test]
    fn zero_and_negative_rects_never_qualify() {
        let zero = win_ex(1, "ugraf", "T", 0, 0, true, false, 0, 0);
        let negative = win_ex(2, "ugraf", "T", -100, -100, true, false, 0, 0);
        let zero_width_only = win_ex(3, "ugraf", "T", 0, 900, true, false, 0, 0);
        for w in [&zero, &negative, &zero_width_only] {
            assert!(!permissive_bindable(w));
        }
        assert!(select_permissive_candidate(&[zero, negative, zero_width_only], "ugraf", None).is_none());
    }

    /// Every candidate for the app is present but under-sized: must return None, not panic or
    /// pick the "least small" one.
    #[test]
    fn app_with_only_sub_minimum_windows_yields_none() {
        let cands = [
            win_ex(1, "ugraf", "a", 50, 50, true, false, 0, 0),
            win_ex(2, "ugraf", "b", 100, 90, true, false, 0, 0),
            win_ex(3, "ugraf", "c", 159, 119, true, false, 0, 0),
        ];
        assert!(select_permissive_candidate(&cands, "ugraf", None).is_none());
        assert!(select_permissive_candidate(&cands, "ugraf", Some(3)).is_none());
    }

    /// A title that already contains the literal text "#22"/"#3A" must still round-trip:
    /// encode replaces '#' before ':', so every '#' — including ones that happen to start a
    /// "#22"/"#3A"-looking sequence — is escaped first, and the decoder (undo "#3A" -> ':'
    /// then "#22" -> '#', OBS's own order) recovers the exact original.
    #[test]
    fn build_obs_id_title_already_containing_encoded_sequences() {
        let title = "Weird#22Name#3Awith#hashes:and:colons";
        let id = build_obs_id(title, "c", "e.exe");
        let first_segment = id.split(':').next().unwrap_or("");
        assert!(!first_segment.contains(':'), "no raw ':' may survive encoding");
        let decoded = first_segment.replace("#3A", ":").replace("#22", "#");
        assert_eq!(decoded, title);
    }

    /// Unicode/emoji titles (common in localized NX UIs and drawing names) contain no '#' or
    /// ':' so they must pass through `enc` byte-for-byte, with no panic from multi-byte UTF-8.
    #[test]
    fn build_obs_id_unicode_and_emoji_titles_round_trip() {
        let title = "図面1 🛠 Extrude";
        let id = build_obs_id(title, "NXToolWnd", "ugraf.exe");
        assert!(id.starts_with(title));
        assert_eq!(id, format!("{}:{}:{}", title, "NXToolWnd", "ugraf.exe"));
    }

    /// A title that is nothing but the two hazard characters: after encoding, the only raw
    /// ':' characters left in the whole id are the two structural delimiters `build_obs_id`
    /// itself inserts between title/class/exe.
    #[test]
    fn build_obs_id_all_hash_and_colon_title_does_not_collide_with_delimiters() {
        let title = "#:#:#:#:";
        let id = build_obs_id(title, "", "x.exe");
        assert_eq!(id.matches(':').count(), 2, "only the two structural delimiters remain");
    }

    #[test]
    fn build_obs_id_empty_class_and_exe_still_produces_a_parseable_three_part_id() {
        let id = build_obs_id("Extrude", "", "");
        assert_eq!(id, "Extrude::");
        assert_eq!(id.split(':').count(), 3);
    }

    /// Smoke test against pathological field values (max/min hwnd, negative size, non-UTF-8-
    /// hazard unicode title): must format, never panic.
    #[test]
    fn describe_window_never_panics_on_adversarial_fields() {
        let w = win_ex(isize::MAX, "ugraf", "図面#:🛠", i32::MIN, i32::MAX, false, true, -1, isize::MIN);
        let s = describe_window(&w);
        assert!(s.contains("ugraf"));
        assert!(s.contains("hwnd="));
    }
}
