//! Windows setup wizard — a native Win32 app picker built with native-windows-gui.
//!
//! Mirrors the macOS wizard contract: list capturable apps, let the user select
//! targets + toggle capture-all / start-on-login, then (on Save) update and save
//! the config so the re-exec in `main` sees `setup_completed = true`.
//!
//! The app list is a ListView with real checkboxes (LVS_EX_CHECKBOXES). nwg
//! doesn't expose checkbox state, so it's driven via raw Win32 SendMessage on
//! the control's HWND (see the `lv` module).

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use native_windows_gui as nwg;
use tracing::info;

use super::autostart::{disable_autostart, enable_autostart, AutostartConfig};
use super::wizard_gui::WizardResult;
use crate::config::Config;

/// Raw Win32 helpers for ListView checkbox state (nwg has no checkbox API).
mod lv {
    use winapi::shared::minwindef::{LPARAM, WPARAM};
    use winapi::shared::windef::HWND;
    use winapi::um::commctrl::{
        LVITEMW, LVIS_STATEIMAGEMASK, LVM_GETITEMSTATE, LVM_SETEXTENDEDLISTVIEWSTYLE,
        LVM_SETITEMSTATE, LVS_EX_CHECKBOXES, LVS_EX_FULLROWSELECT,
    };
    use winapi::um::winuser::SendMessageW;

    /// State-image index encodes the checkbox: 1 = unchecked, 2 = checked,
    /// stored in bits 12-15 of the item state.
    fn state_image(checked: bool) -> u32 {
        if checked {
            2 << 12
        } else {
            1 << 12
        }
    }

    pub unsafe fn enable_checkboxes(hwnd: HWND) {
        let style = (LVS_EX_CHECKBOXES | LVS_EX_FULLROWSELECT) as WPARAM;
        SendMessageW(hwnd, LVM_SETEXTENDEDLISTVIEWSTYLE, style, style as LPARAM);
    }

    pub unsafe fn set_check(hwnd: HWND, row: usize, checked: bool) {
        let mut item: LVITEMW = std::mem::zeroed();
        item.stateMask = LVIS_STATEIMAGEMASK;
        item.state = state_image(checked);
        SendMessageW(
            hwnd,
            LVM_SETITEMSTATE,
            row as WPARAM,
            &item as *const LVITEMW as LPARAM,
        );
    }

    pub unsafe fn is_checked(hwnd: HWND, row: usize) -> bool {
        let st = SendMessageW(
            hwnd,
            LVM_GETITEMSTATE,
            row as WPARAM,
            LVIS_STATEIMAGEMASK as LPARAM,
        ) as u32;
        ((st & LVIS_STATEIMAGEMASK) >> 12) == 2
    }
}

/// Windows shell / system UI processes that are never useful capture targets,
/// so we hide them from the picker to reduce clutter.
const SYSTEM_EXES: &[&str] = &[
    "applicationframehost",
    "searchhost",
    "searchapp",
    "shellexperiencehost",
    "startmenuexperiencehost",
    "systemsettings",
    "textinputhost",
    "lockapp",
    "sihost",
    "ctfmon",
    "dwm",
    "runtimebroker",
    "useroobebroker",
    "widgets",
    "widgetservice",
    "explorer",
    "taskmgr",
];

/// A clean, human-friendly label for an app: the product name when available
/// (e.g. "Mozilla Firefox"), with the executable appended only when it adds
/// information. Avoids the noisy raw window titles.
fn friendly_label(product_name: &str, exe: &str) -> String {
    let product = product_name.trim();
    if product.is_empty() {
        return exe.to_string();
    }
    if product.eq_ignore_ascii_case(exe) {
        product.to_string()
    } else {
        format!("{} ({})", product, exe)
    }
}

fn list_windowed_apps() -> Vec<(String, String)> {
    use libobs_simple::sources::windows::{WindowCaptureSourceBuilder, WindowSearchMode};
    use std::collections::BTreeMap;

    // exe stem (lowercase) -> display label
    let mut by_exe: BTreeMap<String, String> = BTreeMap::new();

    if let Ok(windows) = WindowCaptureSourceBuilder::get_windows(WindowSearchMode::ExcludeMinimized) {
        for w in windows {
            let exe = std::path::Path::new(&w.0.full_exe)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if exe.is_empty() {
                continue;
            }
            let exe_l = exe.to_ascii_lowercase();
            if SYSTEM_EXES.contains(&exe_l.as_str()) {
                continue;
            }
            let product = w.0.product_name.clone().unwrap_or_default();
            by_exe
                .entry(exe_l)
                .or_insert_with(|| friendly_label(&product, &exe));
        }
    }

    if by_exe.is_empty() {
        // Fallback: running processes (noisier, but better than an empty list).
        for app in crate::capture::list_capturable_apps() {
            let exe_l = app.bundle_id.to_ascii_lowercase();
            if SYSTEM_EXES.contains(&exe_l.as_str()) {
                continue;
            }
            by_exe.entry(exe_l).or_insert(app.name);
        }
    }

    let mut apps: Vec<(String, String)> = by_exe
        .into_iter()
        .map(|(exe, label)| (label, exe))
        .collect();
    // Sort by the human-friendly label, case-insensitively.
    apps.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    apps
}

/// Show the native Windows setup wizard. Blocks until the user clicks Save or
/// Cancel (or closes the window).
pub fn run_wizard_windows(config: &mut Config) -> Result<WizardResult> {
    nwg::init().map_err(|e| anyhow::anyhow!("Failed to initialize GUI: {:?}", e))?;
    let _ = nwg::Font::set_global_family("Segoe UI");

    let apps = list_windowed_apps();
    let preselect: Vec<usize> = apps
        .iter()
        .enumerate()
        .filter(|(_, (_, exe))| {
            config
                .capture
                .target_apps
                .iter()
                .any(|t| t.eq_ignore_ascii_case(exe))
        })
        .map(|(i, _)| i)
        .collect();

    let mut window = Default::default();
    let mut intro = Default::default();
    let mut list = Default::default();
    let mut capture_all = Default::default();
    let mut autostart = Default::default();
    let mut save_btn = Default::default();
    let mut cancel_btn = Default::default();
    let layout = Default::default();

    nwg::Window::builder()
        .size((460, 520))
        .center(true)
        .title("crowd-cast setup")
        .build(&mut window)
        .map_err(|e| anyhow::anyhow!("window: {:?}", e))?;

    nwg::Label::builder()
        .text("Check the applications you want to capture:")
        .parent(&window)
        .build(&mut intro)
        .map_err(|e| anyhow::anyhow!("label: {:?}", e))?;

    nwg::ListView::builder()
        .list_style(nwg::ListViewStyle::Detailed)
        .parent(&window)
        .build(&mut list)
        .map_err(|e| anyhow::anyhow!("listview: {:?}", e))?;

    // Single, header-less column that holds the app label + its checkbox.
    list.set_headers_enabled(false);
    list.insert_column(nwg::InsertListViewColumn {
        index: Some(0),
        fmt: None,
        width: Some(430),
        text: Some("Application".to_string()),
    });
    for (label, _) in &apps {
        list.insert_item(label.as_str());
    }

    // nwg has no checkbox API, so enable LVS_EX_CHECKBOXES and pre-check the
    // already-configured apps via raw Win32 on the control's HWND.
    if let Some(hwnd) = list.handle.hwnd() {
        unsafe {
            lv::enable_checkboxes(hwnd);
            for &i in &preselect {
                lv::set_check(hwnd, i, true);
            }
        }
    }

    nwg::CheckBox::builder()
        .text("Capture all applications (ignore the checks above)")
        .check_state(check_state(config.capture.capture_all))
        .parent(&window)
        .build(&mut capture_all)
        .map_err(|e| anyhow::anyhow!("checkbox: {:?}", e))?;

    nwg::CheckBox::builder()
        .text("Start crowd-cast automatically at login")
        .check_state(check_state(config.capture.start_on_login))
        .parent(&window)
        .build(&mut autostart)
        .map_err(|e| anyhow::anyhow!("checkbox: {:?}", e))?;

    nwg::Button::builder()
        .text("Save")
        .parent(&window)
        .build(&mut save_btn)
        .map_err(|e| anyhow::anyhow!("button: {:?}", e))?;

    nwg::Button::builder()
        .text("Cancel")
        .parent(&window)
        .build(&mut cancel_btn)
        .map_err(|e| anyhow::anyhow!("button: {:?}", e))?;

    nwg::GridLayout::builder()
        .parent(&window)
        .spacing(2)
        .child_item(nwg::GridLayoutItem::new(&intro, 0, 0, 2, 1))
        .child_item(nwg::GridLayoutItem::new(&list, 0, 1, 2, 8))
        .child_item(nwg::GridLayoutItem::new(&capture_all, 0, 9, 2, 1))
        .child_item(nwg::GridLayoutItem::new(&autostart, 0, 10, 2, 1))
        .child(0, 11, &save_btn)
        .child(1, 11, &cancel_btn)
        .build(&layout)
        .map_err(|e| anyhow::anyhow!("layout: {:?}", e))?;

    let result = Rc::new(RefCell::new(WizardResult::default()));
    let apps = Rc::new(apps);
    let window = Rc::new(window);

    let handler = {
        let res = result.clone();
        let apps = apps.clone();
        nwg::full_bind_event_handler(&window.handle, move |evt, _data, handle| {
            use nwg::Event as E;
            match evt {
                // Closing the (only) window ends the wizard with completed = false.
                E::OnWindowClose => nwg::stop_thread_dispatch(),
                E::OnButtonClick => {
                    if &handle == &save_btn {
                        let mut selected: Vec<String> = Vec::new();
                        if let Some(hwnd) = list.handle.hwnd() {
                            for (i, (_, exe)) in apps.iter().enumerate() {
                                if unsafe { lv::is_checked(hwnd, i) } {
                                    selected.push(exe.clone());
                                }
                            }
                        }
                        let mut r = res.borrow_mut();
                        r.completed = true;
                        r.selected_apps = selected;
                        r.capture_all = is_checked(&capture_all);
                        r.autostart_enabled = is_checked(&autostart);
                        nwg::stop_thread_dispatch();
                    } else if &handle == &cancel_btn {
                        nwg::stop_thread_dispatch();
                    }
                }
                _ => {}
            }
        })
    };

    nwg::dispatch_thread_events();
    nwg::unbind_event_handler(&handler);

    let result = result.borrow().clone();

    if result.completed {
        info!(
            "Windows wizard completed: {} app(s), capture_all={}, autostart={}",
            result.selected_apps.len(),
            result.capture_all,
            result.autostart_enabled
        );
        config.capture.capture_all = result.capture_all;
        config.capture.target_apps = result.selected_apps.clone();
        config.capture.setup_completed = true;
        config.capture.start_on_login = result.autostart_enabled;

        if result.autostart_enabled {
            if let Err(e) = enable_autostart(&AutostartConfig::default()) {
                info!("Failed to enable autostart: {}", e);
            }
        } else if let Err(e) = disable_autostart() {
            info!("Failed to disable autostart: {}", e);
        }

        config.save()?;
        info!("Configuration saved");
    } else {
        info!("Windows wizard cancelled");
    }

    Ok(result)
}

fn check_state(checked: bool) -> nwg::CheckBoxState {
    if checked {
        nwg::CheckBoxState::Checked
    } else {
        nwg::CheckBoxState::Unchecked
    }
}

fn is_checked(cb: &nwg::CheckBox) -> bool {
    matches!(cb.check_state(), nwg::CheckBoxState::Checked)
}
