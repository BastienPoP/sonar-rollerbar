//! `win` and its submodules are the only code allowed to contain `unsafe`: keyboard hook, COM
//! audio, tray and local HTTP. The crate root enforces this with `#![deny(unsafe_code)]`.

pub mod loopback;
pub mod mixer;
pub mod tray;

use std::ffi::{OsStr, c_void};
use std::panic;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Devices::FunctionDiscovery::{
    PKEY_Device_EnumeratorName, PKEY_Device_FriendlyName,
};
use windows::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, GetLastError, LPARAM, LRESULT, PROPERTYKEY, WPARAM,
};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eRender,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToBSTR};
use windows::Win32::System::Com::{
    CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, IPersistFile, STGM_READ,
};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{
    FOLDERID_Startup, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellExecuteW, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG,
    PostThreadMessageW, SW_SHOWNORMAL, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
    WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN,
};
use windows::core::{Error, HSTRING, Interface, PCWSTR, Result, w};

#[derive(Clone, Copy)]
pub enum VolumeKey {
    Up = 0,
    Down = 1,
    Mute = 2,
}

impl VolumeKey {
    pub const COUNT: usize = 3;

    fn from_code(code: u32) -> Option<Self> {
        match u16::try_from(code).ok()? {
            code if code == VK_VOLUME_UP.0 => Some(Self::Up),
            code if code == VK_VOLUME_DOWN.0 => Some(Self::Down),
            code if code == VK_VOLUME_MUTE.0 => Some(Self::Mute),
            _ => None,
        }
    }

    pub fn code(self) -> u16 {
        match self {
            Self::Up => VK_VOLUME_UP.0,
            Self::Down => VK_VOLUME_DOWN.0,
            Self::Mute => VK_VOLUME_MUTE.0,
        }
    }
}

#[derive(Clone, Copy)]
pub struct KeyEvent {
    pub key: VolumeKey,
    pub down: bool,
    pub injected: bool,
}

/// Only receives volume keys. Returns `true` to swallow the key.
pub type KeyHandler = fn(KeyEvent) -> bool;

static KEY_HANDLER: OnceLock<KeyHandler> = OnceLock::new();
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// The hook: every keystroke on the system goes through here.
///
/// Only `vkCode` and `flags` are read. Anything other than a volume key goes straight on to
/// `CallNextHookEx` without being copied. An error or a panic lets the key through.
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let swallow = panic::catch_unwind(|| should_swallow(code, wparam, lparam)).unwrap_or(false);
    if swallow {
        return LRESULT(1);
    }
    // SAFETY: the arguments are the ones received from Windows, forwarded unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

fn should_swallow(code: i32, wparam: WPARAM, lparam: LPARAM) -> bool {
    if code != HC_ACTION as i32 {
        return false;
    }
    // SAFETY: for WH_KEYBOARD_LL and HC_ACTION, lParam points to a KBDLLHOOKSTRUCT that is valid
    // during the call; both fields are copied before returning.
    let (vk_code, flags) = unsafe {
        let event = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        (event.vkCode, event.flags)
    };
    let Some(key) = VolumeKey::from_code(vk_code) else {
        return false;
    };
    let event = KeyEvent {
        key,
        down: matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN),
        injected: flags.contains(LLKHF_INJECTED),
    };
    KEY_HANDLER.get().is_some_and(|handler| handler(event))
}

/// Installs the hook and pumps the current thread's messages until the tray asks to quit. The
/// callback runs on this thread, which does not initialize COM.
pub fn run_keyboard_hook(handler: KeyHandler) -> Result<()> {
    let _ = KEY_HANDLER.set(handler);
    // SAFETY: the call takes no arguments and cannot fail.
    HOOK_THREAD_ID.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
    // SAFETY: a null module name requests the handle of the process, which is always valid.
    let instance = unsafe { GetModuleHandleW(None) }?;
    // SAFETY: keyboard_proc has the HOOKPROC signature and lives as long as the process.
    let hook = unsafe {
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(instance.into()), 0)
    }?;

    let mut message = MSG::default();
    let result = loop {
        // SAFETY: `message` is a local MSG, valid for writing.
        match unsafe { GetMessageW(&mut message, None, 0, 0) }.0 {
            0 => break Ok(()),
            -1 => break Err(Error::from_thread()),
            _ => {}
        }
    };

    // SAFETY: `hook` comes from SetWindowsHookExW and has not been removed yet.
    let _ = unsafe { UnhookWindowsHookEx(hook) };
    result
}

/// Makes `run_keyboard_hook` return. Callable from any thread.
fn request_hook_stop() {
    let hook_thread = HOOK_THREAD_ID.load(Ordering::Relaxed);
    // SAFETY: the call takes no pointer; it fails harmlessly if the thread is gone.
    let _ = unsafe { PostThreadMessageW(hook_thread, WM_QUIT, WPARAM(0), LPARAM(0)) };
}

/// Opens a folder or web page with its default application. Only called from the tray menu.
pub fn open_in_shell(target: &OsStr) -> Result<()> {
    let target = HSTRING::from(target);
    // SAFETY: the strings are valid for the call and the other arguments are optional.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &target,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    // The API documents values above 32 as success.
    if result.0 as isize > 32 {
        Ok(())
    } else {
        Err(Error::from_thread())
    }
}

pub fn startup_folder() -> Result<PathBuf> {
    // SAFETY: the folder ID is valid, and the returned string is copied, then freed once.
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Startup, KF_FLAG_DEFAULT, None)?;
        let folder = path.to_hstring().to_os_string();
        CoTaskMemFree(Some(path.0 as *const c_void));
        Ok(PathBuf::from(folder))
    }
}

/// Must be called on a thread with COM initialized.
pub fn create_shortcut(shortcut: &Path, target: &Path, arguments: &str) -> Result<()> {
    let shortcut = HSTRING::from(shortcut.as_os_str());
    let target = HSTRING::from(target.as_os_str());
    let arguments = HSTRING::from(arguments);
    // SAFETY: COM is initialized and the strings outlive these calls, which copy them.
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&target)?;
        link.SetArguments(&arguments)?;
        link.cast::<IPersistFile>()?.Save(&shortcut, true)
    }
}

pub fn local_timestamp() -> String {
    // SAFETY: the call takes no arguments.
    let time = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

/// Must be called once on each thread that uses audio.
pub fn init_com() -> Result<()> {
    // SAFETY: the reserved parameter is null, as the API requires.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.ok()
}

pub struct RenderDevice {
    device: IMMDevice,
    /// Windows' stable ID for this endpoint, which is also how Sonar names its outputs.
    pub id: String,
    pub name: String,
    /// Bus the device is attached to, such as `USB` or `ROOT`. Empty if Windows does not say.
    pub bus: String,
}

impl RenderDevice {
    pub fn open(self) -> Result<Endpoint> {
        // SAFETY: `device` is a valid COM interface and `None` omits the optional parameters.
        let volume = unsafe { self.device.Activate(CLSCTX_ALL, None) }?;
        Ok(Endpoint {
            device: self.device,
            volume,
        })
    }
}

fn string_property(store: &IPropertyStore, key: &PROPERTYKEY) -> Result<String> {
    // SAFETY: `store` and `key` are valid; the PROPVARIANT is cleared exactly once, after its
    // conversion to a BSTR that owns its memory.
    unsafe {
        let mut value = store.GetValue(key)?;
        let text = PropVariantToBSTR(&value).map(|text| text.to_string());
        let _ = PropVariantClear(&mut value);
        text
    }
}

fn endpoint_id(device: &IMMDevice) -> Result<String> {
    // SAFETY: `device` is a valid COM interface, and the returned string is copied, then freed
    // once.
    unsafe {
        let id = device.GetId()?;
        let text = id.to_hstring().to_string();
        CoTaskMemFree(Some(id.0 as *const c_void));
        Ok(text)
    }
}

/// Active render devices, in Windows enumeration order.
pub fn active_render_devices() -> Result<Vec<RenderDevice>> {
    // SAFETY: COM is initialized on this thread and `windows` manages the returned interfaces.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let collection = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        let mut devices = Vec::new();
        for index in 0..collection.GetCount()? {
            let device = collection.Item(index)?;
            let id = endpoint_id(&device)?;
            let store = device.OpenPropertyStore(STGM_READ)?;
            let name = string_property(&store, &PKEY_Device_FriendlyName)?;
            let bus = string_property(&store, &PKEY_Device_EnumeratorName).unwrap_or_default();
            devices.push(RenderDevice {
                device,
                id,
                name,
                bus,
            });
        }
        Ok(devices)
    }
}

pub struct Endpoint {
    device: IMMDevice,
    volume: IAudioEndpointVolume,
}

impl Endpoint {
    pub fn is_active(&self) -> bool {
        // SAFETY: `device` is a valid COM interface owned by `self`.
        let state = unsafe { self.device.GetState() };
        state.is_ok_and(|state| state == DEVICE_STATE_ACTIVE)
    }

    pub fn volume_percent(&self) -> Result<i32> {
        // SAFETY: `volume` is a valid COM interface owned by `self`.
        let level = unsafe { self.volume.GetMasterVolumeLevelScalar() }?;
        Ok((level * 100.0).round() as i32)
    }

    pub fn set_volume_percent(&self, percent: i32) -> Result<()> {
        let level = percent as f32 / 100.0;
        // SAFETY: `volume` is valid and the optional event context is null.
        unsafe { self.volume.SetMasterVolumeLevelScalar(level, ptr::null()) }
    }

    pub fn is_muted(&self) -> Result<bool> {
        // SAFETY: `volume` is a valid COM interface owned by `self`.
        Ok(unsafe { self.volume.GetMute() }?.as_bool())
    }

    pub fn set_muted(&self, muted: bool) -> Result<()> {
        // SAFETY: `volume` is valid and the optional event context is null.
        unsafe { self.volume.SetMute(muted, ptr::null()) }
    }
}

/// Returns `false` if another instance is already running. The handle is never closed: it marks
/// the instance until the process ends.
pub fn acquire_single_instance() -> Result<bool> {
    // SAFETY: the name is a static NUL-terminated string and the attributes are the defaults.
    unsafe { CreateMutexW(None, false, w!("Local\\sonar-rollerbar")) }?;
    // SAFETY: reads the thread's error code, set by the call just above.
    Ok(unsafe { GetLastError() } != ERROR_ALREADY_EXISTS)
}

/// Release builds have no console of their own. Keep this to one-off output: once attached, the
/// process would die with the terminal that started it.
pub fn attach_parent_console() {
    // SAFETY: the call takes no pointer; failing only means there is no console to attach to.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}
