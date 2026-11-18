//! Live DWM previews for the dock's app window list.
//!
//! Each preview needs a top-level destination window: `DwmRegisterThumbnail`
//! rejects child windows. The hosts are transparent to the mouse so clicks
//! reach the popup beneath, and are owned by that popup so they die with it.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE, DWM_THUMBNAIL_PROPERTIES,
    DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowRect, HTTRANSPARENT, HWND_TOPMOST, IsWindow, KillTimer,
    SetTimer, SetWindowPos, SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, WM_NCHITTEST, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{BOOL, w};

use super::logic;

/// Timer ID. Kept here so `Dock::owns_message` can name the last timer ID.
/// 110 is taken by `DODGE_TIMER` on main, so previews use the next free slot.
pub const TIMER: usize = 111;
/// The taskbar's previews arrive with a quick, restrained fade and almost no
/// travel: just enough that the whole surface settles into place.
const SHOW_MS: u32 = 110;
const HIDE_MS: u32 = 90;
const RISE: f32 = 4.0;
/// A hovering icon can wobble between neighbouring slots while the dock moves.
pub const DEBOUNCE: Duration = Duration::from_millis(180);

struct Preview {
    id: isize,
    host: HWND,
    /// Title row it belongs to, which need not match its index if a source
    /// window could not be previewed.
    row: usize,
}

#[derive(Clone, Copy)]
struct Anim {
    started: Instant,
    from: f32,
    to: f32,
}

/// The previews, their animation, and the popup geometry they follow.
pub struct Thumbnails {
    previews: Vec<Preview>,
    anim: Option<Anim>,
    /// Cached source handles, so an unchanged hover does not rebuild hosts.
    targets: Vec<isize>,
    base: RECT,
    last_show: Option<Instant>,
}

impl Default for Thumbnails {
    fn default() -> Self {
        Self::new()
    }
}

impl Thumbnails {
    pub fn new() -> Self {
        Self {
            previews: Vec::new(),
            anim: None,
            targets: Vec::new(),
            base: RECT::default(),
            last_show: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.previews.is_empty()
    }

    /// How far the previews are currently faded in.
    pub fn amount(&self) -> f32 {
        let Some(anim) = self.anim else { return 1.0 };
        let showing = anim.to == 1.0;
        let duration = if showing { SHOW_MS } else { HIDE_MS };
        let t = (anim.started.elapsed().as_secs_f32() / (duration as f32 / 1000.0)).clamp(0.0, 1.0);
        // A quick ease-out on arrival, a plain gentle fade on the way out.
        let eased = if showing { crate::ease::out_cubic(t) } else { crate::ease::smoothstep(t) };
        anim.from + (anim.to - anim.from) * eased
    }

    pub fn animating_out(&self) -> bool {
        self.anim.is_some_and(|anim| anim.to == 0.0)
    }

    pub fn last_show(&self) -> Option<Instant> {
        self.last_show
    }

    pub fn note_show(&mut self) {
        self.last_show = Some(Instant::now());
    }

    pub fn set_base(&mut self, base: RECT) {
        self.base = base;
    }

    /// Reports DWM failures when probing with `WISP_DEBUG_THUMBS=1`.
    fn fail(what: &str, error: &windows::core::Error) {
        if std::env::var_os("WISP_DEBUG_THUMBS").is_some() {
            eprintln!("Dock thumbnails: {what}: {error}");
        }
    }

    /// Drops every live preview.
    pub fn clear(&mut self) {
        for preview in self.previews.drain(..) {
            unsafe {
                let _ = DwmUnregisterThumbnail(preview.id);
                let _ = DestroyWindow(preview.host);
            }
        }
        self.targets.clear();
        self.anim = None;
        self.last_show = None;
    }

    /// Rebuilds previews only when the set of source windows changed.
    pub fn sync(&mut self, popup: HWND, targets: &[isize], dpi: u32) {
        if self.targets == targets {
            return;
        }
        self.clear();
        self.targets = targets.to_vec();
        let Ok(instance) = (unsafe { GetModuleHandleW(None) }) else { return };
        for (row, &target) in targets.iter().enumerate() {
            let source = HWND(target as _);
            // DWM can preview minimized windows too, as the taskbar does.
            if !unsafe { IsWindow(Some(source)) }.as_bool() {
                continue;
            }
            // DWM rejects a child as the destination: the host must be top-level.
            let Ok(host) = (unsafe {
                CreateWindowExW(
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                    w!("Wisp.DockThumb"),
                    w!(""),
                    WS_POPUP,
                    0,
                    0,
                    1,
                    1,
                    Some(popup),
                    None,
                    Some(instance.into()),
                    None,
                )
            }) else {
                continue;
            };
            match unsafe { DwmRegisterThumbnail(host, source) } {
                Ok(id) => self.previews.push(Preview { id, host, row }),
                Err(error) => {
                    Self::fail("register", &error);
                    unsafe {
                        let _ = DestroyWindow(host);
                    }
                }
            }
        }
        self.place(popup, dpi, 1.0);
    }

    /// Moves the previews to follow the popup and applies the current fade.
    pub fn place(&mut self, popup: HWND, dpi: u32, amount: f32) {
        let scale = dpi as f32 / 96.0;
        let pad = (logic::THUMB_PAD * scale).round() as i32;
        let width = (logic::THUMB_W * scale).round() as i32;
        let height = (logic::THUMB_H * scale).round() as i32;
        let row = (logic::THUMB_ROW * scale).round() as i32;
        let opacity = (255.0 * amount).round().clamp(0.0, 255.0) as u8;
        let mut rect = RECT::default();
        if let Err(error) = unsafe { GetWindowRect(popup, &mut rect) } {
            Self::fail("popup rect", &error);
            return;
        }
        for preview in &self.previews {
            let top = rect.top + pad + preview.row as i32 * row;
            unsafe {
                let _ = SetWindowPos(
                    preview.host,
                    Some(HWND_TOPMOST),
                    rect.left + pad,
                    top,
                    width,
                    height,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
            let properties = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                },
                rcSource: RECT::default(),
                opacity,
                fVisible: BOOL(1),
                fSourceClientAreaOnly: BOOL(0),
            };
            if let Err(error) = unsafe { DwmUpdateThumbnailProperties(preview.id, &properties) } {
                Self::fail("update", &error);
            }
        }
    }

    /// Starts the fade in when the previews first appear.
    pub fn begin_show(&mut self, owner: HWND, popup: HWND, dpi: u32) {
        if self.previews.is_empty() {
            return;
        }
        self.anim = Some(Anim {
            started: Instant::now(),
            from: 0.0,
            to: 1.0,
        });
        self.place(popup, dpi, 0.0);
        unsafe {
            let _ = SetTimer(Some(owner), TIMER, 16, None);
        }
    }

    /// Starts the fade out from wherever the fade in currently is. Returns
    /// false when there is nothing to animate and the caller should close now.
    pub fn begin_hide(&mut self, owner: HWND) -> bool {
        if self.is_empty() {
            return false;
        }
        if self.animating_out() {
            return true;
        }
        self.anim = Some(Anim {
            started: Instant::now(),
            from: self.amount(),
            to: 0.0,
        });
        unsafe {
            let _ = SetTimer(Some(owner), TIMER, 16, None);
        }
        true
    }

    /// Reverses a fade out if the pointer came back before it finished.
    pub fn cancel_hide(&mut self) {
        if self.animating_out() {
            self.anim = Some(Anim {
                started: Instant::now(),
                from: self.amount(),
                to: 1.0,
            });
        }
    }

    /// Advances the fade. Returns true when a hide finished and the popup
    /// should be closed by the caller.
    pub fn step(&mut self, owner: HWND, popup: HWND, dpi: u32) -> bool {
        let Some(anim) = self.anim else { return false };
        let duration = if anim.to == 1.0 { SHOW_MS } else { HIDE_MS };
        let t = anim.started.elapsed().as_secs_f32() / (duration as f32 / 1000.0);
        let amount = self.amount();
        // The entire popup, labels included, rises into place with the fade.
        let rise = ((1.0 - amount) * RISE * (dpi as f32 / 96.0)).round() as i32;
        unsafe {
            let _ = SetWindowPos(
                popup,
                None,
                self.base.left,
                self.base.top + rise,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.place(popup, dpi, amount);
        if t < 1.0 {
            return false;
        }
        unsafe {
            let _ = KillTimer(Some(owner), TIMER);
        }
        self.anim = None;
        anim.to == 0.0
    }
}

/// A thumbnail host: transparent to the mouse, with no painting of its own.
pub unsafe extern "system" fn thumb_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        // WS_EX_TRANSPARENT alone does not pass every click through to the
        // popup below; sending hit tests through makes the whole tile live.
        if msg == WM_NCHITTEST {
            return LRESULT(HTTRANSPARENT as isize);
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fade_is_monotone_in_each_direction() {
        let mut thumbs = Thumbnails::new();
        // With no previews registered, amount is the fully shown state.
        assert_eq!(thumbs.amount(), 1.0);
        assert!(!thumbs.animating_out());
        assert!(thumbs.is_empty());
    }

    #[test]
    fn debounce_is_shorter_than_the_steady_hover_delay() {
        assert!(DEBOUNCE < Duration::from_millis(450));
    }
}
