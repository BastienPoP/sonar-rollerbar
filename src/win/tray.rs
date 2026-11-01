//! The tray icon and its menu, on the `tray` thread.
//!
//! Nothing else depends on this code: if it fails, the volume wheel keeps working.

use std::ffi::c_void;
use std::mem;
use std::panic;
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFY_ICON_MESSAGE,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, HICON, HMENU, IDI_APPLICATION,
    IMAGE_ICON, LR_DEFAULTCOLOR, LoadIconW, LoadImageW, MB_ICONINFORMATION, MB_OK, MF_CHECKED,
    MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG, MessageBoxW, PostMessageW, PostQuitMessage,
    RegisterClassW, RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON, SetForegroundWindow,
    TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_APP, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
};
use windows::core::{Error, HSTRING, PCWSTR, Result, w};

use super::mixer::{self, MixerHandlers};

pub struct TrayStatus {
    pub summary: String,
    pub paused: bool,
    pub run_at_startup: bool,
    pub step: i32,
    pub oled: bool,
    pub sonar_mixer: bool,
    pub follow_output: bool,
    pub wheel_select: bool,
    pub wheel_master: bool,
}

pub enum TrayCommand {
    TogglePause,
    ToggleStartup,
    CheckForUpdates,
    OpenLogs,
    SetStep(i32),
    ToggleOled,
    ToggleSonarMixer,
    ToggleFollowOutput,
    ToggleWheelSelect,
    ToggleWheelMaster,
}

/// Connects the tray to the rest of the program. It has no other access to its state.
pub struct TrayHandlers {
    pub status: fn() -> TrayStatus,
    pub on_command: fn(TrayCommand),
    pub about: fn() -> String,
}

static HANDLERS: OnceLock<TrayHandlers> = OnceLock::new();
static TRAY_WINDOW: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static TRAY_ICON: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

const TRAY_ICON_ID: u32 = 1;
pub(super) const ICON_RESOURCE_ID: usize = 1;
const WM_TRAY_ICON: u32 = WM_APP + 1;
const WM_TRAY_REFRESH: u32 = WM_APP + 2;

const MENU_PAUSE: usize = 1;
const MENU_STARTUP: usize = 2;
const MENU_UPDATES: usize = 3;
const MENU_LOGS: usize = 4;
const MENU_ABOUT: usize = 5;
const MENU_QUIT: usize = 6;
const MENU_MIXER: usize = 7;
const MENU_OLED: usize = 8;
const MENU_SONAR_MIXER: usize = 9;
const MENU_FOLLOW_OUTPUT: usize = 10;
const MENU_WHEEL_SELECT: usize = 11;
const MENU_WHEEL_MASTER: usize = 12;
const MENU_STEP_BASE: usize = 100;
const STEP_CHOICES: [i32; 5] = [1, 2, 3, 5, 10];

fn handlers() -> Option<&'static TrayHandlers> {
    HANDLERS.get()
}

/// Runs the tray on the current thread until the user quits.
pub fn run_tray(tray_handlers: TrayHandlers, mixer_handlers: MixerHandlers) -> Result<()> {
    let _ = HANDLERS.set(tray_handlers);
    // SAFETY: the reserved parameter is null. Shortcuts and ShellExecuteW need COM on this thread.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok()?;

    // SAFETY: a null module name requests the handle of the process, which is always valid.
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();
    // SAFETY: the name is a static NUL-terminated string.
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    TASKBAR_CREATED.store(taskbar_created, Ordering::Relaxed);
    TRAY_ICON.store(load_tray_icon(instance).0, Ordering::Relaxed);

    let tray_class = w!("sonar-rollerbar-tray");
    let class = WNDCLASSW {
        lpfnWndProc: Some(tray_proc),
        hInstance: instance,
        lpszClassName: tray_class,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialized and its name is a static string.
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err(Error::from_thread());
    }
    mixer::register(instance, mixer_handlers)?;

    // SAFETY: the class was just registered. The window stays hidden, but must be a top-level
    // window to receive the TaskbarCreated broadcast.
    let window = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            tray_class,
            w!("sonar-rollerbar"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )
    }?;
    TRAY_WINDOW.store(window.0, Ordering::Relaxed);
    // Fails if the taskbar isn't ready yet at sign-in: TaskbarCreated then adds the icon.
    update_tray_icon(window, NIM_ADD);

    let mut message = MSG::default();
    loop {
        // SAFETY: `message` is a local MSG, valid for writing.
        match unsafe { GetMessageW(&mut message, None, 0, 0) }.0 {
            0 => return Ok(()),
            -1 => return Err(Error::from_thread()),
            // SAFETY: `message` was just filled in by GetMessageW.
            _ => unsafe {
                DispatchMessageW(&message);
            },
        }
    }
}

pub(super) fn post_to(window: &AtomicPtr<c_void>, message: u32) {
    let window = window.load(Ordering::Relaxed);
    if !window.is_null() {
        // SAFETY: the handle comes from CreateWindowExW; a stale one only makes the call fail.
        let _ = unsafe { PostMessageW(Some(HWND(window)), message, WPARAM(0), LPARAM(0)) };
    }
}

/// Asks the tray to show the latest status. Callable from any thread.
pub fn refresh_tray() {
    post_to(&TRAY_WINDOW, WM_TRAY_REFRESH);
}

fn load_tray_icon(instance: HINSTANCE) -> HICON {
    // SAFETY: the metric indexes are valid constants.
    let (width, height) =
        unsafe { (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) };
    // SAFETY: an integer resource ID is the documented form of this name argument.
    let icon = unsafe {
        LoadImageW(
            Some(instance),
            PCWSTR(ICON_RESOURCE_ID as *const u16),
            IMAGE_ICON,
            width,
            height,
            LR_DEFAULTCOLOR,
        )
    };
    match icon {
        Ok(icon) => HICON(icon.0),
        // SAFETY: IDI_APPLICATION is a stock icon, so no module is needed.
        Err(_) => unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default(),
    }
}

fn update_tray_icon(window: HWND, action: NOTIFY_ICON_MESSAGE) {
    let summary = handlers().map_or_else(String::new, |handlers| (handlers.status)().summary);
    let tooltip = format!("sonar-rollerbar\n{summary}");

    let mut data = NOTIFYICONDATAW {
        cbSize: mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: TRAY_ICON_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: WM_TRAY_ICON,
        hIcon: HICON(TRAY_ICON.load(Ordering::Relaxed)),
        ..Default::default()
    };
    // The last unit stays zero to terminate the string.
    let capacity = data.szTip.len() - 1;
    for (unit, value) in data.szTip[..capacity].iter_mut().zip(tooltip.encode_utf16()) {
        *unit = value;
    }
    // SAFETY: `data` is fully initialized, with its size in cbSize.
    let _ = unsafe { Shell_NotifyIconW(action, &data) };
}

fn fill_step_menu(menu: HMENU, status: &TrayStatus) -> Result<()> {
    for (index, step) in STEP_CHOICES.iter().enumerate() {
        let flags = if *step == status.step {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        };
        let label = HSTRING::from(format!("{step}%"));
        // SAFETY: `menu` is valid and the string outlives the call, which copies it.
        unsafe { AppendMenuW(menu, flags, MENU_STEP_BASE + index, &label) }?;
    }
    Ok(())
}

fn fill_options_menu(menu: HMENU, status: &TrayStatus) -> Result<()> {
    let flags = |checked: bool| {
        if checked {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        }
    };
    // Only means something with the double click on, so it stays visible but can't be changed.
    let master_flags = if status.wheel_select {
        flags(status.wheel_master)
    } else {
        flags(status.wheel_master) | MF_GRAYED
    };
    // SAFETY: `menu` is valid and the strings are static.
    unsafe {
        AppendMenuW(
            menu,
            flags(status.oled),
            MENU_OLED,
            w!("Show volume on keyboard OLED"),
        )?;
        AppendMenuW(
            menu,
            flags(status.sonar_mixer),
            MENU_SONAR_MIXER,
            w!("Sonar mixer"),
        )?;
        AppendMenuW(
            menu,
            flags(status.follow_output),
            MENU_FOLLOW_OUTPUT,
            w!("Follow Sonar's output"),
        )?;
        AppendMenuW(
            menu,
            flags(status.wheel_select),
            MENU_WHEEL_SELECT,
            w!("Double-click the wheel to choose what it controls"),
        )?;
        AppendMenuW(
            menu,
            master_flags,
            MENU_WHEEL_MASTER,
            w!("Include Master in the double click"),
        )?;
    }
    Ok(())
}

fn fill_tray_menu(menu: HMENU, status: &TrayStatus) -> Result<()> {
    let summary = HSTRING::from(status.summary.as_str());
    let checked_if = |checked: bool| {
        if checked {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING
        }
    };
    // SAFETY: the call takes no arguments. The submenus are destroyed with `menu`.
    let (step_menu, options_menu) = unsafe { (CreatePopupMenu()?, CreatePopupMenu()?) };
    fill_step_menu(step_menu, status)?;
    fill_options_menu(options_menu, status)?;
    // SAFETY: the menus are valid and the strings outlive these calls, which copy them.
    unsafe {
        AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, &summary)?;
        AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?;
        if status.sonar_mixer {
            AppendMenuW(menu, MF_STRING, MENU_MIXER, w!("Open Sonar mixer"))?;
        }
        AppendMenuW(menu, checked_if(status.paused), MENU_PAUSE, w!("Pause"))?;
        AppendMenuW(menu, MF_POPUP, step_menu.0 as usize, w!("Wheel step for this output"))?;
        AppendMenuW(menu, MF_POPUP, options_menu.0 as usize, w!("Options"))?;
        AppendMenuW(
            menu,
            checked_if(status.run_at_startup),
            MENU_STARTUP,
            w!("Run at startup"),
        )?;
        AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?;
        AppendMenuW(menu, MF_STRING, MENU_UPDATES, w!("Check for updates"))?;
        AppendMenuW(menu, MF_STRING, MENU_LOGS, w!("Open logs"))?;
        AppendMenuW(menu, MF_STRING, MENU_ABOUT, w!("About"))?;
        AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?;
        AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit"))?;
    }
    Ok(())
}

fn choose_tray_command(window: HWND, status: &TrayStatus) -> Result<usize> {
    // SAFETY: the call takes no arguments.
    let menu = unsafe { CreatePopupMenu() }?;
    let filled = fill_tray_menu(menu, status);
    let mut cursor = POINT::default();
    // SAFETY: `cursor` is a local POINT, valid for writing.
    let positioned = unsafe { GetCursorPos(&mut cursor) };
    let command = filled.and(positioned).map(|()| {
        // SAFETY: `menu` and `window` are valid. Windows requires a foreground window for the
        // menu to close on an outside click, and the posted WM_NULL for it to close cleanly.
        unsafe {
            let _ = SetForegroundWindow(window);
            let flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON;
            let command = TrackPopupMenu(menu, flags, cursor.x, cursor.y, None, window, None);
            let _ = PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0));
            command.0 as usize
        }
    });
    // SAFETY: `menu` is ours and is no longer displayed.
    let _ = unsafe { DestroyMenu(menu) };
    command
}

fn show_about(window: HWND, handlers: &TrayHandlers) {
    let text = HSTRING::from((handlers.about)());
    // SAFETY: `window` and the strings are valid. This only blocks the tray thread.
    unsafe {
        MessageBoxW(
            Some(window),
            &text,
            w!("About sonar-rollerbar"),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

fn show_tray_menu(window: HWND) {
    let Some(handlers) = handlers() else {
        return;
    };
    let command = match choose_tray_command(window, &(handlers.status)()) {
        Ok(MENU_PAUSE) => TrayCommand::TogglePause,
        Ok(MENU_STARTUP) => TrayCommand::ToggleStartup,
        Ok(MENU_UPDATES) => TrayCommand::CheckForUpdates,
        Ok(MENU_LOGS) => TrayCommand::OpenLogs,
        Ok(MENU_OLED) => TrayCommand::ToggleOled,
        Ok(MENU_SONAR_MIXER) => TrayCommand::ToggleSonarMixer,
        Ok(MENU_FOLLOW_OUTPUT) => TrayCommand::ToggleFollowOutput,
        Ok(MENU_WHEEL_SELECT) => TrayCommand::ToggleWheelSelect,
        Ok(MENU_WHEEL_MASTER) => TrayCommand::ToggleWheelMaster,
        Ok(MENU_MIXER) => {
            let _ = mixer::open();
            return;
        }
        Ok(MENU_ABOUT) => {
            show_about(window, handlers);
            return;
        }
        Ok(MENU_QUIT) => {
            // SAFETY: `window` belongs to this thread.
            let _ = unsafe { DestroyWindow(window) };
            return;
        }
        Ok(id) if id >= MENU_STEP_BASE => match STEP_CHOICES.get(id - MENU_STEP_BASE) {
            Some(step) => TrayCommand::SetStep(*step),
            None => return,
        },
        _ => return,
    };
    (handlers.on_command)(command);
    update_tray_icon(window, NIM_MODIFY);
}

fn on_tray_icon_click(window: HWND, button: u32) {
    let mixer_enabled = handlers().is_some_and(|handlers| (handlers.status)().sonar_mixer);
    if button == WM_LBUTTONUP && mixer_enabled {
        let _ = mixer::open();
    } else {
        show_tray_menu(window);
    }
}

fn on_tray_destroyed(window: HWND) {
    update_tray_icon(window, NIM_DELETE);
    // SAFETY: the call only queues a quit message for the tray thread.
    unsafe { PostQuitMessage(0) };
    super::request_hook_stop();
}

fn on_tray_message(window: HWND, message: u32, lparam: LPARAM) -> bool {
    match message {
        WM_TRAY_ICON => {
            let event = lparam.0 as u32;
            if matches!(event, WM_LBUTTONUP | WM_RBUTTONUP) {
                on_tray_icon_click(window, event);
            }
            true
        }
        WM_TRAY_REFRESH => {
            update_tray_icon(window, NIM_MODIFY);
            true
        }
        WM_DESTROY => {
            on_tray_destroyed(window);
            true
        }
        message if message == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            update_tray_icon(window, NIM_ADD);
            true
        }
        _ => false,
    }
}

unsafe extern "system" fn tray_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let handled = panic::catch_unwind(|| on_tray_message(window, message, lparam)).unwrap_or(false);
    if handled {
        return LRESULT(0);
    }
    // SAFETY: the arguments are the ones received from Windows, forwarded unchanged.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
