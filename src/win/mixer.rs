//! The Sonar mixer window, on the `tray` thread.
//!
//! `crate::mixer_ui` lays it out and paints its shapes, and this module adds the text and handles
//! the mouse. Nothing depends on it: if it fails, the volume wheel keeps working.

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem;
use std::panic;
use std::ptr;
use std::slice;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicPtr, Ordering};

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET,
    DIB_RGB_COLORS, DRAW_TEXT_FORMAT, DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX, DT_RIGHT,
    DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, EndPaint, FONT_WEIGHT, FW_NORMAL,
    FW_SEMIBOLD, GetMonitorInfoW, HDC, HFONT, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, OUT_TT_PRECIS, PAINTSTRUCT, SRCCOPY, ScreenToClient, SelectObject, SetBkMode,
    SetTextCharacterExtra, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_DROPSHADOW, CreateWindowExW, DI_NORMAL, DefWindowProcW, DestroyIcon, DrawIconEx,
    GetCursorPos, HICON, IDC_ARROW, IMAGE_ICON, LR_DEFAULTCOLOR, LoadCursorW, LoadImageW,
    RegisterClassW, SW_HIDE, SW_SHOW, SWP_NOZORDER, SetForegroundWindow, SetWindowPos, ShowWindow,
    WA_INACTIVE, WM_ACTIVATE, WM_APP, WM_CAPTURECHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_PAINT, WNDCLASSW, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{Error, PCWSTR, Result, w};

use super::tray::{ICON_RESOURCE_ID, post_to};
use crate::gg_protocol::{CHANNELS, ChannelLevel, ChatMix, Control, device_name};
use crate::mixer_ui::{
    Canvas, ChatScene, Frame, HOVERED_ICON, Hit, Layout, MUTED_ICON, ROWS, RowScene,
    SECONDARY_TEXT, Spot, TEXT, draw,
};

const WM_MIXER_REFRESH: u32 = WM_APP + 3;
const WM_MOUSELEAVE: u32 = 0x02A3;
const BASE_DPI: u32 = 96;
const CHAT_MIX_WHEEL_STEP: i32 = 5;

const VOLUME_ICON: &str = "\u{E767}";
const MUTED_VOLUME_ICON: &str = "\u{E74F}";

#[derive(Clone)]
pub struct MixerView {
    pub output_name: String,
    pub output: Option<ChannelLevel>,
    pub channels: [Option<ChannelLevel>; CHANNELS.len()],
    pub chat_mix: Option<ChatMix>,
    pub selected: Control,
    pub wheel_select: bool,
}

/// Connects the window to the rest of the program. It has no other access to its state.
pub struct MixerHandlers {
    pub view: fn() -> MixerView,
    pub step: fn() -> i32,
    pub on_change: fn(Control, i32),
    pub on_mute: fn(Control),
    pub on_visible: fn(bool),
}

#[derive(Default)]
struct State {
    view: Option<MixerView>,
    dragging: Option<Spot>,
    hovered_mute: Option<usize>,
    tracking_mouse: bool,
    dpi: u32,
}

static HANDLERS: OnceLock<MixerHandlers> = OnceLock::new();
static MIXER_WINDOW: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

thread_local! {
    // Windows calls this window on the tray thread only.
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn row_control(row: usize) -> Control {
    if row == 0 {
        Control::Output
    } else {
        Control::Channel(row - 1)
    }
}

fn spot_control(spot: Spot) -> Control {
    match spot {
        Spot::Row(row) => row_control(row),
        Spot::ChatMix => Control::ChatMix,
    }
}

fn row_level(view: &MixerView, row: usize) -> Option<ChannelLevel> {
    match row_control(row) {
        Control::Channel(index) => view.channels[index],
        _ => view.output,
    }
}

fn set_level(view: &mut MixerView, spot: Spot, value: i32) {
    let level = |current: Option<ChannelLevel>| {
        Some(ChannelLevel {
            percent: value,
            muted: current.is_some_and(|level| level.muted),
        })
    };
    match spot_control(spot) {
        Control::Output => view.output = level(view.output),
        Control::Channel(index) => view.channels[index] = level(view.channels[index]),
        Control::ChatMix => {
            if let Some(mix) = &mut view.chat_mix {
                mix.balance = value;
            }
        }
    }
}

fn spot_value(view: &MixerView, spot: Spot) -> Option<i32> {
    match spot {
        Spot::Row(row) => row_level(view, row).map(|level| level.percent),
        Spot::ChatMix => view.chat_mix.map(|mix| mix.balance),
    }
}

fn chat_mix_note(view: &MixerView) -> &'static str {
    view.chat_mix.map_or("", |mix| mix.state.note())
}

fn layout_of(state: &State) -> Layout {
    let has_note = state.view.as_ref().is_some_and(|view| !chat_mix_note(view).is_empty());
    Layout::new(state.dpi.max(BASE_DPI), has_note)
}

pub fn register(instance: HINSTANCE, handlers: MixerHandlers) -> Result<()> {
    let _ = HANDLERS.set(handlers);
    // SAFETY: IDC_ARROW is a stock cursor, so no module is needed.
    let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }?;
    let class = WNDCLASSW {
        style: CS_DROPSHADOW,
        lpfnWndProc: Some(mixer_proc),
        hInstance: instance,
        hCursor: cursor,
        lpszClassName: w!("sonar-rollerbar-mixer"),
        ..Default::default()
    };
    // SAFETY: `class` is fully initialized and its name is a static string.
    if unsafe { RegisterClassW(&class) } == 0 {
        return Err(Error::from_thread());
    }
    Ok(())
}

/// Asks the window to show the latest levels. Callable from any thread.
pub fn refresh() {
    post_to(&MIXER_WINDOW, WM_MIXER_REFRESH);
}

fn create_window() -> Result<HWND> {
    // SAFETY: a null module name requests the handle of the process, which is always valid.
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }?.into();
    // SAFETY: the class is registered in `register`. The window starts hidden and is sized later.
    let window = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("sonar-rollerbar-mixer"),
            w!("Sonar mixer"),
            WS_POPUP,
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
    let corners = DWMWCP_ROUND;
    // Windows 10 doesn't know this attribute, and the window then keeps its square corners.
    // SAFETY: `window` is valid and the attribute is passed with its exact size.
    let _ = unsafe {
        DwmSetWindowAttribute(
            window,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            ptr::from_ref(&corners).cast(),
            mem::size_of_val(&corners) as u32,
        )
    };
    MIXER_WINDOW.store(window.0, Ordering::Relaxed);
    Ok(window)
}

/// Sits above the taskbar, under the cursor, on the screen the cursor is on.
fn place(window: HWND) -> Result<()> {
    let mut cursor = POINT::default();
    // SAFETY: `cursor` is a local POINT, valid for writing.
    unsafe { GetCursorPos(&mut cursor) }?;
    // SAFETY: the flag makes this return a valid monitor for any point.
    let monitor = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let (mut dpi, mut unused) = (BASE_DPI, BASE_DPI);
    // SAFETY: `monitor` is valid and the outputs are local, valid for writing.
    unsafe {
        let _ = GetMonitorInfoW(monitor, &mut info);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut unused);
    }
    let layout = STATE.with_borrow_mut(|state| {
        state.dpi = dpi;
        layout_of(state)
    });
    let area = info.rcWork;
    let (width, height) = layout.size();
    let x = (cursor.x - width / 2).clamp(area.left, (area.right - width).max(area.left));
    let y = (area.bottom - height).max(area.top);
    // SAFETY: `window` is valid and the flag keeps its order.
    unsafe { SetWindowPos(window, None, x, y, width, height, SWP_NOZORDER) }
}

fn load_view() {
    let Some(handlers) = HANDLERS.get() else {
        return;
    };
    let mut view = (handlers.view)();
    STATE.with_borrow_mut(|state| {
        // Leaves alone a slider that the user is dragging.
        if let (Some(spot), Some(shown)) = (state.dragging, &state.view)
            && let Some(value) = spot_value(shown, spot)
        {
            set_level(&mut view, spot, value);
        }
        state.view = Some(view);
    });
}

fn invalidate(window: HWND) {
    // SAFETY: `window` is valid. The whole window is repainted over itself.
    let _ = unsafe { InvalidateRect(Some(window), None, false) };
}

pub fn open() -> Result<()> {
    let existing = MIXER_WINDOW.load(Ordering::Relaxed);
    let window = if existing.is_null() {
        create_window()?
    } else {
        HWND(existing)
    };
    if let Some(handlers) = HANDLERS.get() {
        (handlers.on_visible)(true);
    }
    load_view();
    place(window)?;
    invalidate(window);
    // SAFETY: `window` is valid. It takes the foreground so that clicking elsewhere closes it.
    unsafe {
        let _ = ShowWindow(window, SW_SHOW);
        let _ = SetForegroundWindow(window);
    }
    Ok(())
}

fn hide(window: HWND) {
    // SAFETY: `window` is valid.
    let _ = unsafe { ShowWindow(window, SW_HIDE) };
    if let Some(handlers) = HANDLERS.get() {
        (handlers.on_visible)(false);
    }
}

struct Font(HFONT);

impl Font {
    fn new(height: i32, weight: FONT_WEIGHT, face: PCWSTR) -> Self {
        // SAFETY: the name is a static string and the other arguments are plain values.
        Self(unsafe {
            CreateFontW(
                -height,
                0,
                0,
                0,
                weight.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_TT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                face,
            )
        })
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: the font is ours, and `draw_text` never leaves it selected in a device context.
        let _ = unsafe { DeleteObject(self.0.into()) };
    }
}

fn draw_text(dc: HDC, font: &Font, color: u32, frame: Frame, text: &str, align: DRAW_TEXT_FORMAT) {
    let mut rect = RECT {
        left: frame.x,
        top: frame.y,
        right: frame.x + frame.width,
        bottom: frame.y + frame.height,
    };
    let mut units: Vec<u16> = text.encode_utf16().collect();
    // Colours are 0xRRGGBB here and 0xBBGGRR in GDI.
    let color = COLORREF((color & 0xFF) << 16 | (color & 0xFF00) | (color >> 16) & 0xFF);
    let format = align | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS;
    // SAFETY: `dc`, the font and the local rectangle are valid, and the previous font is restored.
    unsafe {
        let previous = SelectObject(dc, font.0.into());
        SetTextColor(dc, color);
        DrawTextW(dc, &mut units, &mut rect, format);
        SelectObject(dc, previous);
    }
}

fn draw_header(dc: HDC, layout: &Layout, view: &MixerView) {
    let icon = layout.icon();
    // SAFETY: an integer resource ID is the documented form of this name argument, the icon is
    // ours, and it is destroyed once drawn.
    unsafe {
        let module = GetModuleHandleW(None).map(HINSTANCE::from).ok();
        let name = PCWSTR(ICON_RESOURCE_ID as *const u16);
        let image = LoadImageW(module, name, IMAGE_ICON, icon.width, icon.height, LR_DEFAULTCOLOR);
        if let Ok(image) = image {
            let handle = HICON(image.0);
            let _ = DrawIconEx(dc, icon.x, icon.y, handle, 0, 0, 0, None, DI_NORMAL);
            let _ = DestroyIcon(handle);
        }
    }
    let title = Font::new(layout.scaled(14.0), FW_SEMIBOLD, w!("Segoe UI"));
    let small = Font::new(layout.scaled(12.0), FW_NORMAL, w!("Segoe UI"));
    draw_text(dc, &title, TEXT, layout.title(), "sonar-rollerbar", DRAW_TEXT_FORMAT(0));
    let name = device_name(&view.output_name);
    draw_text(dc, &small, SECONDARY_TEXT, layout.output_name(), name, DT_RIGHT);
}

fn draw_rows(dc: HDC, layout: &Layout, view: &MixerView, hovered_mute: Option<usize>) {
    let body = Font::new(layout.scaled(13.0), FW_NORMAL, w!("Segoe UI"));
    let strong = Font::new(layout.scaled(13.0), FW_SEMIBOLD, w!("Segoe UI"));
    let section = Font::new(layout.scaled(11.0), FW_SEMIBOLD, w!("Segoe UI"));
    let icons = Font::new(layout.scaled(15.0), FW_NORMAL, w!("Segoe MDL2 Assets"));

    // SAFETY: `dc` is valid and the call takes plain integers.
    let previous = unsafe { SetTextCharacterExtra(dc, layout.scaled(1.0)) };
    draw_text(dc, &section, SECONDARY_TEXT, layout.section(), "SONAR", DRAW_TEXT_FORMAT(0));
    // SAFETY: as above.
    unsafe { SetTextCharacterExtra(dc, previous) };

    for row in 0..ROWS {
        let control = row_control(row);
        let level = row_level(view, row);
        let muted = level.is_some_and(|level| level.muted);
        let color = if muted { SECONDARY_TEXT } else { TEXT };
        let font = if control == view.selected { &strong } else { &body };
        draw_text(dc, font, color, layout.row_label(row), control.label(), DRAW_TEXT_FORMAT(0));
        let value = match level {
            None => "-".to_owned(),
            Some(ChannelLevel { muted: true, .. }) => "Off".to_owned(),
            Some(level) => format!("{}%", level.percent),
        };
        draw_text(dc, &body, color, layout.row_value(row), &value, DT_RIGHT);
        let (icon, color) = if muted {
            (MUTED_VOLUME_ICON, MUTED_ICON)
        } else if hovered_mute == Some(row) {
            (VOLUME_ICON, HOVERED_ICON)
        } else {
            (VOLUME_ICON, SECONDARY_TEXT)
        };
        draw_text(dc, &icons, color, layout.row_mute(row), icon, DT_CENTER);
    }
}

fn chat_mix_value(view: &MixerView) -> String {
    match view.chat_mix {
        None => "-".to_owned(),
        Some(mix) if !mix.is_available() => "Unavailable".to_owned(),
        Some(mix) if mix.balance == 0 => "Balanced".to_owned(),
        Some(mix) => {
            let side = if mix.balance < 0 { "Game" } else { "Chat" };
            format!("{}% {side}", mix.balance.abs())
        }
    }
}

fn footer_text(view: &MixerView) -> String {
    match view.selected {
        Control::Output if view.wheel_select => {
            "Double-click the wheel to change what it controls".to_owned()
        }
        Control::Output => "The wheel controls Output".to_owned(),
        target => format!("Wheel controls {}, back to Output after 10 s", target.label()),
    }
}

fn draw_chat_mix(dc: HDC, layout: &Layout, view: &MixerView) {
    let body = Font::new(layout.scaled(13.0), FW_NORMAL, w!("Segoe UI"));
    let strong = Font::new(layout.scaled(13.0), FW_SEMIBOLD, w!("Segoe UI"));
    let small = Font::new(layout.scaled(12.0), FW_NORMAL, w!("Segoe UI"));
    let title = if view.selected == Control::ChatMix { &strong } else { &body };
    draw_text(dc, title, TEXT, layout.chat_title(), "ChatMix", DRAW_TEXT_FORMAT(0));
    let value = chat_mix_value(view);
    draw_text(dc, &small, SECONDARY_TEXT, layout.chat_value(), &value, DT_RIGHT);
    draw_text(dc, &small, SECONDARY_TEXT, layout.chat_game(), "Game", DRAW_TEXT_FORMAT(0));
    draw_text(dc, &small, SECONDARY_TEXT, layout.chat_chat(), "Chat", DT_RIGHT);
    let note = chat_mix_note(view);
    if !note.is_empty() {
        draw_text(dc, &small, SECONDARY_TEXT, layout.chat_note(), note, DRAW_TEXT_FORMAT(0));
    }
    draw_text(dc, &small, SECONDARY_TEXT, layout.footer(), &footer_text(view), DRAW_TEXT_FORMAT(0));
}

fn scene(view: &MixerView) -> ([RowScene; ROWS], ChatScene) {
    let rows = std::array::from_fn(|row| {
        let level = row_level(view, row);
        RowScene {
            percent: level.map(|level| level.percent),
            muted: level.is_some_and(|level| level.muted),
            selected: row_control(row) == view.selected,
        }
    });
    let chat = ChatScene {
        balance: view.chat_mix.map(|mix| mix.balance),
        enabled: view.chat_mix.is_some_and(ChatMix::is_available),
        selected: view.selected == Control::ChatMix,
    };
    (rows, chat)
}

fn paint_buffered(screen: HDC, state: &State, view: &MixerView) -> Result<()> {
    let layout = layout_of(state);
    let (width, height) = layout.size();
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = ptr::null_mut();
    // SAFETY: `screen` is valid and the bitmap's memory stays valid until DeleteObject.
    let (buffer, bitmap) = unsafe {
        let buffer = CreateCompatibleDC(Some(screen));
        match CreateDIBSection(Some(buffer), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bitmap) => (buffer, bitmap),
            Err(error) => {
                let _ = DeleteDC(buffer);
                return Err(error);
            }
        }
    };
    let (rows, chat) = scene(view);
    // SAFETY: the bitmap is `width` by `height` at 32 bits, and this slice ends before GDI draws
    // in the same memory below.
    let pixels =
        unsafe { slice::from_raw_parts_mut(bits.cast::<u32>(), (width * height) as usize) };
    draw(&mut Canvas { pixels, width, height }, &layout, &rows, &chat);

    // SAFETY: both device contexts are valid. The bitmap is unselected, then freed once.
    unsafe {
        let previous = SelectObject(buffer, bitmap.into());
        SetBkMode(buffer, TRANSPARENT);
        draw_header(buffer, &layout, view);
        draw_rows(buffer, &layout, view, state.hovered_mute);
        draw_chat_mix(buffer, &layout, view);
        let copied = BitBlt(screen, 0, 0, width, height, Some(buffer), 0, 0, SRCCOPY);
        SelectObject(buffer, previous);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(buffer);
        copied
    }
}

fn paint(window: HWND) {
    let mut paint = PAINTSTRUCT::default();
    // SAFETY: `window` is valid and `paint` is a local structure, valid for writing.
    let screen = unsafe { BeginPaint(window, &mut paint) };
    STATE.with_borrow(|state| {
        if let Some(view) = &state.view {
            let _ = paint_buffered(screen, state, view);
        }
    });
    // SAFETY: this ends the paint started above.
    let _ = unsafe { EndPaint(window, &paint) };
}

fn cursor_position(lparam: LPARAM) -> (i32, i32) {
    let low = (lparam.0 & 0xFFFF) as i16;
    let high = ((lparam.0 >> 16) & 0xFFFF) as i16;
    (i32::from(low), i32::from(high))
}

fn change_value(window: HWND, spot: Spot, value: i32) {
    let Some(handlers) = HANDLERS.get() else {
        return;
    };
    STATE.with_borrow_mut(|state| {
        if let Some(view) = &mut state.view {
            set_level(view, spot, value);
        }
    });
    (handlers.on_change)(spot_control(spot), value);
    invalidate(window);
}

fn chat_mix_is_usable() -> bool {
    STATE.with_borrow(|state| {
        let mix = state.view.as_ref().and_then(|view| view.chat_mix);
        mix.is_some_and(ChatMix::is_available)
    })
}

fn drag_to(window: HWND, spot: Spot, x: i32) {
    let value = STATE.with_borrow(|state| layout_of(state).value_at(spot, x));
    change_value(window, spot, value);
}

fn on_mouse_down(window: HWND, x: i32, y: i32) {
    match STATE.with_borrow(|state| layout_of(state).hit(x, y)) {
        Some(Hit::Slider(Spot::ChatMix)) if !chat_mix_is_usable() => {}
        Some(Hit::Slider(spot)) => {
            STATE.with_borrow_mut(|state| state.dragging = Some(spot));
            // SAFETY: `window` is valid. The drag continues outside of it.
            unsafe { SetCapture(window) };
            drag_to(window, spot, x);
        }
        Some(Hit::Mute(row)) => {
            if let Some(handlers) = HANDLERS.get() {
                (handlers.on_mute)(row_control(row));
            }
        }
        None => {}
    }
}

fn on_mouse_move(window: HWND, x: i32, y: i32) {
    let (dragging, changed, tracking) = STATE.with_borrow_mut(|state| {
        let hovered = match layout_of(state).hit(x, y) {
            Some(Hit::Mute(row)) => Some(row),
            _ => None,
        };
        let changed = state.hovered_mute != hovered;
        state.hovered_mute = hovered;
        let tracking = mem::replace(&mut state.tracking_mouse, true);
        (state.dragging, changed, tracking)
    });
    if let Some(spot) = dragging {
        drag_to(window, spot, x);
    } else if changed {
        invalidate(window);
    }
    if !tracking {
        let mut track = TRACKMOUSEEVENT {
            cbSize: mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: window,
            dwHoverTime: 0,
        };
        // SAFETY: `track` is fully initialized, with its size in cbSize.
        let _ = unsafe { TrackMouseEvent(&mut track) };
    }
}

fn on_mouse_up(window: HWND, x: i32) {
    if let Some(spot) = STATE.with_borrow(|state| state.dragging) {
        // Repeats the last value: a full queue may have dropped it during the drag.
        drag_to(window, spot, x);
        // SAFETY: the call takes no arguments.
        let _ = unsafe { ReleaseCapture() };
    }
}

fn on_mouse_wheel(window: HWND, wparam: WPARAM, lparam: LPARAM) {
    let Some(handlers) = HANDLERS.get() else {
        return;
    };
    let notch = ((wparam.0 >> 16) & 0xFFFF) as i16;
    let (x, y) = cursor_position(lparam);
    let mut point = POINT { x, y };
    // SAFETY: `window` is valid and `point` is a local POINT, valid for writing.
    let _ = unsafe { ScreenToClient(window, &mut point) };
    let target = STATE.with_borrow(|state| {
        let spot = match layout_of(state).hit(point.x, point.y) {
            Some(Hit::Slider(spot)) => spot,
            Some(Hit::Mute(row)) => Spot::Row(row),
            None => return None,
        };
        let value = spot_value(state.view.as_ref()?, spot)?;
        Some((spot, value))
    });
    let Some((spot, value)) = target else {
        return;
    };
    if spot == Spot::ChatMix && !chat_mix_is_usable() {
        return;
    }
    let (step, low) = match spot {
        Spot::Row(_) => ((handlers.step)(), 0),
        Spot::ChatMix => (CHAT_MIX_WHEEL_STEP, -100),
    };
    let moved = value + i32::from(notch.signum()) * step;
    change_value(window, spot, moved.clamp(low, 100));
}

fn on_refresh(window: HWND) {
    let height_before = STATE.with_borrow(|state| layout_of(state).size().1);
    load_view();
    let height = STATE.with_borrow(|state| layout_of(state).size().1);
    if height != height_before {
        let _ = place(window);
    }
    invalidate(window);
}

fn on_message(window: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> Option<isize> {
    match message {
        WM_PAINT => {
            paint(window);
            Some(0)
        }
        // Painting covers the whole window, so erasing would only make it flicker.
        WM_ERASEBKGND => Some(1),
        WM_MIXER_REFRESH => {
            on_refresh(window);
            Some(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = cursor_position(lparam);
            on_mouse_down(window, x, y);
            Some(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = cursor_position(lparam);
            on_mouse_move(window, x, y);
            Some(0)
        }
        WM_LBUTTONUP => {
            on_mouse_up(window, cursor_position(lparam).0);
            Some(0)
        }
        WM_CAPTURECHANGED => {
            STATE.with_borrow_mut(|state| state.dragging = None);
            Some(0)
        }
        WM_MOUSELEAVE => {
            STATE.with_borrow_mut(|state| {
                state.hovered_mute = None;
                state.tracking_mouse = false;
            });
            invalidate(window);
            Some(0)
        }
        WM_MOUSEWHEEL => {
            on_mouse_wheel(window, wparam, lparam);
            Some(0)
        }
        WM_KEYDOWN if wparam.0 == usize::from(VK_ESCAPE.0) => {
            hide(window);
            Some(0)
        }
        WM_ACTIVATE => {
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                hide(window);
            }
            None
        }
        _ => None,
    }
}

unsafe extern "system" fn mixer_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let handled =
        panic::catch_unwind(|| on_message(window, message, wparam, lparam)).unwrap_or(None);
    if let Some(result) = handled {
        return LRESULT(result);
    }
    // SAFETY: the arguments are the ones received from Windows, forwarded unchanged.
    unsafe { DefWindowProcW(window, message, wparam, lparam) }
}
