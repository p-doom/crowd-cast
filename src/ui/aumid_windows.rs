//! Windows AppUserModelID (AUMID) registration for branded toast notifications.
//!
//! Toasts raised by a desktop (non-packaged) app inherit the identity of the
//! process' AUMID, and Windows resolves that AUMID to a Start Menu shortcut to
//! get the name + icon shown on the toast. Without our own identity we fall
//! back to PowerShell's AUMID, so toasts read "Windows PowerShell".
//!
//! This module makes toasts read "crowd-cast" by:
//!   1. ensuring a Start Menu shortcut exists whose `System.AppUserModel.ID`
//!      property equals [`APP_AUMID`] (the `.lnk` file name becomes the toast's
//!      display name, and its icon becomes the toast icon), and
//!   2. setting the process' explicit AUMID at startup.
//!
//! The toast code ([`super::notifications`]) then raises toasts with the same
//! [`APP_AUMID`].

use std::path::{Path, PathBuf};

use tracing::{debug, warn};

/// Stable application id. Must match the toast's app id and the shortcut's
/// `System.AppUserModel.ID` property.
pub const APP_AUMID: &str = "dev.crowd-cast.agent";

/// Display name shown on toasts — Windows derives it from the shortcut's file
/// name, so this is the `.lnk` stem.
const SHORTCUT_NAME: &str = "crowd-cast";

/// Register our notification identity and apply it to the current process.
/// Best-effort: logs and continues on failure (toasts still work, just branded
/// as PowerShell).
pub fn register() {
    let icon = ensure_icon();
    if let Err(e) = ensure_shortcut(icon.as_deref()) {
        warn!("Could not create Start Menu shortcut for notifications: {e:#}");
    }
    set_process_aumid();
}

/// Apply [`APP_AUMID`] to the current process so its toasts use that identity.
fn set_process_aumid() {
    use windows::core::PCWSTR;
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    let wide: Vec<u16> = APP_AUMID.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a valid NUL-terminated UTF-16 buffer that outlives the call.
    unsafe {
        if let Err(e) = SetCurrentProcessExplicitAppUserModelID(PCWSTR(wide.as_ptr())) {
            debug!("SetCurrentProcessExplicitAppUserModelID failed: {e}");
        }
    }
}

fn start_menu_dir() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    Some(PathBuf::from(appdata).join(r"Microsoft\Windows\Start Menu\Programs"))
}

/// Create the Start Menu shortcut (if absent) carrying our AUMID property.
fn ensure_shortcut(icon: Option<&Path>) -> anyhow::Result<()> {
    use anyhow::Context;

    let lnk = start_menu_dir()
        .context("APPDATA not set")?
        .join(format!("{SHORTCUT_NAME}.lnk"));
    if lnk.exists() {
        return Ok(());
    }
    let exe = std::env::current_exe().context("current_exe failed")?;
    create_shortcut(&exe, &lnk, icon)?;
    debug!("Created notification shortcut at {}", lnk.display());
    Ok(())
}

/// Build the `.lnk` via COM (IShellLink + IPropertyStore + IPersistFile) on a
/// dedicated STA thread so we don't disturb the main thread's COM state.
fn create_shortcut(exe: &Path, lnk: &Path, icon: Option<&Path>) -> anyhow::Result<()> {
    use windows::core::{Interface, GUID, HSTRING};
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
        COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, PROPERTYKEY};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};

    // PKEY_AppUserModel_ID (propkey.h): fmtid {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3}, pid 5.
    const PKEY_APPUSERMODEL_ID: PROPERTYKEY = PROPERTYKEY {
        fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
        pid: 5,
    };

    let exe = HSTRING::from(exe.to_string_lossy().as_ref());
    let lnk = HSTRING::from(lnk.to_string_lossy().as_ref());
    let icon = icon.map(|p| HSTRING::from(p.to_string_lossy().as_ref()));

    // COM apartment state is per-thread; isolate it on its own thread.
    std::thread::spawn(move || -> anyhow::Result<()> {
        // SAFETY: all interfaces are released before CoUninitialize; the wide
        // strings outlive the calls that borrow them.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let result = (|| -> anyhow::Result<()> {
                let link: IShellLinkW =
                    CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
                link.SetPath(&exe)?;
                if let Some(icon) = &icon {
                    let _ = link.SetIconLocation(icon, 0);
                }

                let store: IPropertyStore = link.cast()?;
                let value = windows::core::PROPVARIANT::from(APP_AUMID);
                store.SetValue(&PKEY_APPUSERMODEL_ID, &value)?;
                store.Commit()?;

                let file: IPersistFile = link.cast()?;
                file.Save(&lnk, BOOL::from(true))?;
                Ok(())
            })();
            CoUninitialize();
            result
        }
    })
    .join()
    .map_err(|_| anyhow::anyhow!("shortcut creation thread panicked"))?
}

/// Write the embedded logo to a stable `.ico` (once) for the shortcut/toast
/// icon. Best-effort — returns None on any failure.
fn ensure_icon() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA")?)
        .join("crowd-cast")
        .join("agent");
    let path = dir.join("crowd-cast.ico");
    if path.exists() {
        return Some(path);
    }

    let png: &[u8] = include_bytes!("../../assets/logo.png");
    let img = image::load_from_memory(png).ok()?;
    let icon = img.resize_exact(64, 64, image::imageops::FilterType::Lanczos3);

    std::fs::create_dir_all(&dir).ok()?;
    let mut file = std::fs::File::create(&path).ok()?;
    icon.write_to(&mut file, image::ImageFormat::Ico).ok()?;
    Some(path)
}
