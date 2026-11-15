//! Windows tray icon: a notification-area icon (Shell_NotifyIcon) owned by a
//! hidden window on its own thread, whose message loop receives the icon's
//! clicks and shows its menu.

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::channel::mpsc::UnboundedSender;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, HICON, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW,
    PostQuitMessage, RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_CONTEXTMENU,
    WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
};
use windows::core::{PCWSTR, w};

use super::{Command, Status, menu_labels};

/// Sent by the icon on clicks.
const WM_TRAY: u32 = WM_APP + 1;
/// Sent by `set_status`: show the new status.
const WM_STATUS: u32 = WM_APP + 2;
const ICON_ID: u32 = 1;
const ICON_PX: i32 = 32;

pub struct Tray {
    /// The hidden window (an HWND, which isn't `Send`).
    hwnd: Arc<AtomicIsize>,
    status: Arc<Mutex<Status>>,
}

/// What the window procedure needs, kept on the tray's thread.
struct State {
    commands: UnboundedSender<Command>,
    status: Arc<Mutex<Status>>,
    icon: HICON,
    /// Sent to all windows when Explorer (and with it the taskbar) restarts.
    taskbar_created: u32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

impl Tray {
    pub fn start(commands: UnboundedSender<Command>) -> Option<Self> {
        let hwnd = Arc::new(AtomicIsize::new(0));
        let status = Arc::new(Mutex::new(Status::default()));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (h, s) = (hwnd.clone(), status.clone());
        std::thread::Builder::new()
            .name("tray".into())
            .spawn(move || {
                // SAFETY: Win32 calls on this thread's own window and icon;
                // pointers passed are to live locals.
                let ok = unsafe { create(commands, s) };
                match ok {
                    Some(window) => {
                        h.store(window.0 as isize, Ordering::SeqCst);
                        let _ = ready_tx.send(true);
                        // SAFETY: a standard message loop for this thread.
                        unsafe {
                            let mut msg = MSG::default();
                            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                                let _ = TranslateMessage(&msg);
                                DispatchMessageW(&msg);
                            }
                        }
                    }
                    None => {
                        let _ = ready_tx.send(false);
                    }
                }
            })
            .ok()?;
        ready_rx.recv().ok().filter(|ok| *ok).map(|_| Self { hwnd, status })
    }

    pub fn set_status(&self, status: &Status) {
        *self.status.lock().unwrap() = status.clone();
        self.post(WM_STATUS);
    }

    fn post(&self, msg: u32) {
        let hwnd = HWND(self.hwnd.load(Ordering::SeqCst) as *mut _);
        // SAFETY: posting to a window of ours; fails harmlessly once it's gone.
        let _ = unsafe { PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0)) };
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // The window removes the icon and ends its thread.
        self.post(WM_CLOSE);
    }
}

/// Create the hidden window and add the icon.
unsafe fn create(commands: UnboundedSender<Command>, status: Arc<Mutex<Status>>) -> Option<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let class = w!("LocalPlaylistTray");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return None;
        }
        // A plain hidden window rather than a message-only one: those don't
        // get the "taskbar created" broadcast.
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            w!("Local Playlist tray"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .ok()?;
        let icon = make_icon()?;
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        STATE.with(|s| *s.borrow_mut() = Some(State { commands, status, icon, taskbar_created }));
        if !Shell_NotifyIconW(NIM_ADD, &icon_data(hwnd, icon, "Local Playlist")).as_bool() {
            let _ = DestroyWindow(hwnd);
            return None;
        }
        Some(hwnd)
    }
}

/// The app icon as an HICON (32-bit colour with alpha; the AND mask is only
/// a fallback for displays without alpha).
unsafe fn make_icon() -> Option<HICON> {
    let rgba = super::icon_rgba(ICON_PX as u32);
    let mut bgra = Vec::with_capacity(rgba.len());
    let mut and_mask = Vec::with_capacity(rgba.len() / 4);
    for p in rgba.chunks_exact(4) {
        bgra.extend([p[2], p[1], p[0], p[3]]);
        and_mask.push(if p[3] == 0 { 0xff } else { 0 });
    }
    // SAFETY: the buffers are ICON_PX² pixels (the mask has more than the
    // 1 bit per pixel it needs).
    unsafe { CreateIcon(None, ICON_PX, ICON_PX, 1, 32, and_mask.as_ptr(), bgra.as_ptr()).ok() }
}

fn icon_data(hwnd: HWND, icon: HICON, tip: &str) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon,
        ..Default::default()
    };
    // At most 127 UTF-16 units and the terminating zero.
    for (dst, src) in data.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
        *dst = src;
    }
    data
}

fn send(c: Command) {
    STATE.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            let _ = s.commands.unbounded_send(c);
        }
    });
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY (whole function): Win32 calls on our own window, menu and icon.
    unsafe {
        let taskbar_created = STATE.with(|s| s.borrow().as_ref().map(|s| s.taskbar_created));
        match msg {
            WM_TRAY => match lparam.0 as u32 {
                WM_LBUTTONUP => send(Command::ToggleWindow),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            },
            WM_STATUS => {
                let tip = STATE.with(|s| {
                    let s = s.borrow();
                    let s = s.as_ref()?;
                    let status = s.status.lock().unwrap();
                    let tip = match &status.track {
                        Some(t) => format!("Local Playlist — {t}"),
                        None => "Local Playlist".into(),
                    };
                    Some((s.icon, tip))
                });
                if let Some((icon, tip)) = tip {
                    let _ = Shell_NotifyIconW(NIM_MODIFY, &icon_data(hwnd, icon, &tip));
                }
            }
            m if Some(m) == taskbar_created => {
                // Explorer restarted: the icon is gone, add it again.
                if let Some(icon) = STATE.with(|s| s.borrow().as_ref().map(|s| s.icon)) {
                    let _ = Shell_NotifyIconW(NIM_ADD, &icon_data(hwnd, icon, "Local Playlist"));
                }
            }
            WM_CLOSE => {
                if let Some(icon) = STATE.with(|s| s.borrow().as_ref().map(|s| s.icon)) {
                    let _ = Shell_NotifyIconW(NIM_DELETE, &icon_data(hwnd, icon, ""));
                    let _ = DestroyIcon(icon);
                }
                let _ = DestroyWindow(hwnd);
            }
            WM_DESTROY => {
                STATE.with(|s| s.borrow_mut().take());
                PostQuitMessage(0);
            }
            _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
        }
        LRESULT(0)
    }
}

/// The right-click menu, built from the current status.
unsafe fn show_menu(hwnd: HWND) {
    let Some(status) = STATE.with(|s| s.borrow().as_ref().map(|s| s.status.lock().unwrap().clone())) else {
        return;
    };
    // SAFETY: our own menu, destroyed before returning.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let now = wide(status.track.as_deref().unwrap_or("Nothing playing"));
        let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(now.as_ptr()));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let labels = menu_labels(&status);
        let texts: Vec<Vec<u16>> = labels.iter().map(|(_, l)| wide(l)).collect();
        for (i, text) in texts.iter().enumerate() {
            if i == 3 {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            }
            // Item ids start at 1: 0 is "nothing chosen".
            let _ = AppendMenuW(menu, MF_STRING, i + 1, PCWSTR(text.as_ptr()));
        }
        let mut at = POINT::default();
        let _ = GetCursorPos(&mut at);
        // Without this the menu doesn't close when clicking elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let chosen =
            TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, at.x, at.y, None, hwnd, None).0;
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        if chosen > 0
            && let Some((command, _)) = labels.get(chosen as usize - 1)
        {
            send(*command);
        }
    }
}
