//! Windows setup wizard — a native Win32 app picker built with native-windows-gui.
//!
//! Mirrors the macOS wizard contract: list capturable apps, let the user select
//! targets + toggle capture-all / start-on-login, then (on Save) update and save
//! the config so the re-exec in `main` sees `setup_completed = true`.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use native_windows_gui as nwg;
use tracing::info;

use super::autostart::{disable_autostart, enable_autostart, AutostartConfig};
use super::wizard_gui::WizardResult;
use crate::config::Config;

/// Enumerate currently-visible application windows as (display label, exe stem)
/// pairs, de-duplicated by executable. Falls back to the running-process list if
/// no windows can be enumerated.
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
            let title = w.0.title.clone().unwrap_or_default();
            let label = if title.trim().is_empty() {
                exe.clone()
            } else {
                format!("{}  —  {}", title.trim(), exe)
            };
            by_exe.entry(exe.to_ascii_lowercase()).or_insert(label);
        }
    }

    if by_exe.is_empty() {
        // Fallback: running processes (noisier, but better than an empty list).
        for app in crate::capture::list_capturable_apps() {
            by_exe
                .entry(app.bundle_id.to_ascii_lowercase())
                .or_insert(app.name);
        }
    }

    by_exe.into_iter().map(|(exe, label)| (label, exe)).collect()
}

/// Show the native Windows setup wizard. Blocks until the user clicks Save or
/// Cancel (or closes the window).
pub fn run_wizard_windows(config: &mut Config) -> Result<WizardResult> {
    nwg::init().map_err(|e| anyhow::anyhow!("Failed to initialize GUI: {:?}", e))?;
    let _ = nwg::Font::set_global_family("Segoe UI");

    let apps = list_windowed_apps();
    let labels: Vec<String> = apps.iter().map(|(label, _)| label.clone()).collect();
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
        .text("Select the applications to capture (Ctrl/Shift-click for multiple):")
        .parent(&window)
        .build(&mut intro)
        .map_err(|e| anyhow::anyhow!("label: {:?}", e))?;

    nwg::ListBox::builder()
        .collection(labels)
        .multi_selection(preselect)
        .flags(nwg::ListBoxFlags::VISIBLE | nwg::ListBoxFlags::MULTI_SELECT)
        .parent(&window)
        .build(&mut list)
        .map_err(|e| anyhow::anyhow!("listbox: {:?}", e))?;

    nwg::CheckBox::builder()
        .text("Capture all applications (ignore the selection above)")
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
                        let selected: Vec<String> = list
                            .multi_selection()
                            .into_iter()
                            .filter_map(|i| apps.get(i).map(|(_, exe)| exe.clone()))
                            .collect();
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
