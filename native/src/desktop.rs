use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::ComponentHandle;
use std::{
    io,
    mem::size_of,
    os::windows::ffi::OsStrExt,
    ptr::{null, null_mut},
};
use windows_sys::{
    Win32::{
        Foundation::*,
        System::{Registry::*, Threading::*},
        UI::{Shell::*, WindowsAndMessaging::*},
    },
    core::w,
};

const TRAY_MESSAGE: u32 = WM_APP + 1;

/// Whether closing the window hides Sonic to the tray (true) or quits it.
static CLOSE_TO_TRAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Hands memory Sonic isn't using right now back to Windows (while hidden).
pub fn trim_memory() {
    unsafe {
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

pub fn set_close_to_tray(enabled: bool) {
    CLOSE_TO_TRAY.store(enabled, std::sync::atomic::Ordering::Relaxed);
}
const RUN: *const u16 = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const APP: *const u16 = w!("SonicNative");

pub struct Instance(HANDLE);

impl Instance {
    pub fn acquire() -> io::Result<Option<Self>> {
        // An auto-reset event handles both instance ownership and requests to show the window.
        unsafe {
            let event = CreateEventW(null(), 0, 0, w!("Local\\SonicNative-ShowWindow-v1"));
            if event.is_null() {
                return Err(io::Error::last_os_error());
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let success = SetEvent(event);
                let error = io::Error::last_os_error();
                CloseHandle(event);
                return if success == 0 { Err(error) } else { Ok(None) };
            }
            Ok(Some(Self(event)))
        }
    }

    pub fn activate_if_requested(&self, tray: &Tray) {
        unsafe {
            if WaitForSingleObject(self.0, 0) == WAIT_OBJECT_0 {
                show(tray.data.hWnd);
            }
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub struct Tray {
    data: NOTIFYICONDATAW,
    custom_icon: bool,
}

impl Tray {
    pub fn attach(ui: &crate::Sonic) -> Option<Self> {
        let handle = ui.window().window_handle();
        let RawWindowHandle::Win32(handle) = handle.window_handle().ok()?.as_raw() else {
            return None;
        };
        let hwnd = handle.hwnd.get() as HWND;
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: TRAY_MESSAGE,
            ..Default::default()
        };
        for (to, from) in data.szTip.iter_mut().zip("Sonic".encode_utf16()) {
            *to = from;
        }
        unsafe {
            let custom = app_icon(32);
            let custom_icon = !custom.is_null();
            data.hIcon = if custom_icon {
                custom
            } else {
                LoadIconW(null_mut(), IDI_APPLICATION)
            };
            if Shell_NotifyIconW(NIM_ADD, &data) == 0 {
                if custom_icon {
                    DestroyIcon(data.hIcon);
                }
                return None;
            }
            if SetWindowSubclass(hwnd, Some(window_proc), 1, 0) == 0 {
                Shell_NotifyIconW(NIM_DELETE, &data);
                if custom_icon {
                    DestroyIcon(data.hIcon);
                }
                return None;
            }
            Some(Self { data, custom_icon })
        }
    }
}

/// Whether Sonic's window is on screen (not hidden in the tray or minimized).
pub fn window_shown(ui: &crate::Sonic) -> bool {
    let handle = ui.window().window_handle();
    let Some(RawWindowHandle::Win32(handle)) = handle.window_handle().ok().map(|h| h.as_raw())
    else {
        return true;
    };
    let hwnd = handle.hwnd.get() as HWND;
    unsafe { IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 }
}

/// Sets the window's big and small icons. The taskbar and Alt+Tab use the big one;
/// without it Windows shows its cached copy of the .exe icon, which can be stale after
/// an update. Returns false until the native window exists, so the caller can retry.
pub fn set_window_icons(ui: &crate::Sonic) -> bool {
    let handle = ui.window().window_handle();
    let Some(RawWindowHandle::Win32(handle)) = handle.window_handle().ok().map(|h| h.as_raw())
    else {
        return false;
    };
    let hwnd = handle.hwnd.get() as HWND;
    unsafe {
        for (kind, metric) in [(ICON_BIG, SM_CXICON), (ICON_SMALL, SM_CXSMICON)] {
            let icon = app_icon(GetSystemMetrics(metric));
            if !icon.is_null() {
                // The window keeps these for its lifetime; Windows frees them on exit.
                SendMessageW(hwnd, WM_SETICON, kind as usize, icon as isize);
            }
        }
    }
    true
}

/// Sonic's icon at `size` pixels, from the embedded .ico (null if it can't be made).
unsafe fn app_icon(size: i32) -> HICON {
    let icon = include_bytes!("../assets/icon.ico");
    ico_image(icon, size)
        .map(|bytes| unsafe {
            CreateIconFromResourceEx(
                bytes.as_ptr(),
                bytes.len() as u32,
                1,
                0x0003_0000,
                size,
                size,
                LR_DEFAULTCOLOR,
            )
        })
        .unwrap_or(null_mut())
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            RemoveWindowSubclass(self.data.hWnd, Some(window_proc), 1);
            Shell_NotifyIconW(NIM_DELETE, &self.data);
            if self.custom_icon {
                DestroyIcon(self.data.hIcon);
            }
        }
    }
}

unsafe fn show(hwnd: HWND) {
    unsafe {
        ShowWindow(
            hwnd,
            if IsIconic(hwnd) != 0 {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        );
        SetForegroundWindow(hwnd);
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _: usize,
    _: usize,
) -> LRESULT {
    unsafe {
        if message == WM_CLOSE {
            if CLOSE_TO_TRAY.load(std::sync::atomic::Ordering::Relaxed) {
                ShowWindow(hwnd, SW_HIDE);
            } else {
                let _ = slint::quit_event_loop();
            }
            return 0;
        }
        if message == TRAY_MESSAGE {
            match lparam as u32 {
                WM_LBUTTONUP => show(hwnd),
                WM_RBUTTONUP => {
                    let menu = CreatePopupMenu();
                    if menu.is_null() {
                        return 0;
                    }
                    let startup = startup_enabled();
                    AppendMenuW(menu, MF_STRING, 1, w!("Open Sonic"));
                    AppendMenuW(
                        menu,
                        MF_STRING | if startup { MF_CHECKED } else { MF_UNCHECKED },
                        2,
                        w!("Run at startup"),
                    );
                    AppendMenuW(menu, MF_SEPARATOR, 0, null());
                    AppendMenuW(menu, MF_STRING, 3, w!("Quit"));
                    let mut cursor = POINT::default();
                    GetCursorPos(&mut cursor);
                    SetForegroundWindow(hwnd);
                    let command = TrackPopupMenu(
                        menu,
                        TPM_RETURNCMD | TPM_RIGHTBUTTON,
                        cursor.x,
                        cursor.y,
                        0,
                        hwnd,
                        null(),
                    );
                    DestroyMenu(menu);
                    PostMessageW(hwnd, WM_NULL, 0, 0);
                    match command {
                        1 => show(hwnd),
                        2 => {
                            if let Err(error) = set_startup(!startup) {
                                let message: Vec<u16> =
                                    format!("Could not update startup: {error}")
                                        .encode_utf16()
                                        .chain([0])
                                        .collect();
                                MessageBoxW(hwnd, message.as_ptr(), w!("Sonic"), MB_ICONERROR);
                            }
                        }
                        3 => {
                            let _ = slint::quit_event_loop();
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            return 0;
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

unsafe fn startup_enabled() -> bool {
    let mut size = 0;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN,
            APP,
            RRF_RT_REG_SZ,
            null_mut(),
            null_mut(),
            &mut size,
        ) == ERROR_SUCCESS
    }
}

unsafe fn set_startup(enabled: bool) -> io::Result<()> {
    let result = if enabled {
        let exe = std::env::current_exe()?;
        let command: Vec<u16> = [b'"' as u16]
            .into_iter()
            .chain(exe.as_os_str().encode_wide())
            .chain([b'"' as u16, 0])
            .collect();
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN,
                APP,
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * 2) as u32,
            )
        }
    } else {
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, APP) }
    };
    if result == ERROR_SUCCESS || (!enabled && result == ERROR_FILE_NOT_FOUND) {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(result as i32))
    }
}

fn ico_image(file: &[u8], target: i32) -> Option<&[u8]> {
    let count = u16::from_le_bytes(file.get(4..6)?.try_into().ok()?) as usize;
    let directory = file.get(6..6 + count * 16)?;
    let entry = directory.chunks_exact(16).min_by_key(|entry| {
        let width = if entry[0] == 0 {
            256
        } else {
            i32::from(entry[0])
        };
        // Prefer the closest size, and a larger one over a smaller one on ties.
        ((width - target).abs(), width < target)
    })?;
    let size = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
    let offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
    file.get(offset..offset.checked_add(size)?)
}

/// Opens the Windows folder picker over Sonic.
pub fn pick_folder(ui: &crate::Sonic) -> Option<std::path::PathBuf> {
    use windows_sys::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{
            BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW, SHBrowseForFolderW,
            SHGetPathFromIDListW,
        },
    };
    let owner = ui
        .window()
        .window_handle()
        .window_handle()
        .ok()
        .and_then(|handle| match handle.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
            _ => None,
        })
        .unwrap_or(null_mut());
    let mut name = [0u16; 260];
    let title: Vec<u16> = "Choose a folder with music".encode_utf16().chain([0]).collect();
    let info = BROWSEINFOW {
        hwndOwner: owner,
        pszDisplayName: name.as_mut_ptr(),
        lpszTitle: title.as_ptr(),
        ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
        ..Default::default()
    };
    let list = unsafe { SHBrowseForFolderW(&info) };
    if list.is_null() {
        return None;
    }
    let mut path = [0u16; 1024];
    let ok = unsafe { SHGetPathFromIDListW(list, path.as_mut_ptr()) };
    unsafe { CoTaskMemFree(list as *const _) };
    if ok == 0 {
        return None;
    }
    let len = path.iter().position(|&c| c == 0).unwrap_or(path.len());
    Some(std::path::PathBuf::from(String::from_utf16_lossy(&path[..len])))
}

/// Opens the Windows color dialog over Sonic, like the original's color input.
pub fn pick_color(ui: &crate::Sonic, initial: [u8; 3]) -> Option<[u8; 3]> {
    use windows_sys::Win32::UI::Controls::Dialogs::{
        CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW, ChooseColorW,
    };
    let owner = ui
        .window()
        .window_handle()
        .window_handle()
        .ok()
        .and_then(|handle| match handle.as_raw() {
            RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
            _ => None,
        })
        .unwrap_or(null_mut());
    let [r, g, b] = initial;
    let mut custom = [0u32; 16];
    let mut dialog = CHOOSECOLORW {
        lStructSize: size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        rgbResult: u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_RGBINIT | CC_FULLOPEN,
        ..Default::default()
    };
    if unsafe { ChooseColorW(&mut dialog) } == 0 {
        return None;
    }
    let color = dialog.rgbResult;
    Some([color as u8, (color >> 8) as u8, (color >> 16) as u8])
}
