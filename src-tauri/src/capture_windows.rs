//! Window capture on Windows: Windows Graphics Capture captures only the chosen window, even while
//! other windows cover it, so nothing else on screen is ever read.

use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetForegroundWindow, IsIconic, IsWindow};
use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
use windows_capture::frame::Frame;
use windows_capture::graphics_capture_api::InternalCaptureControl;
use windows_capture::settings::{
    ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings, MinimumUpdateIntervalSettings,
    SecondaryWindowSettings, Settings,
};
use windows_capture::monitor::Monitor;
use windows_capture::window::Window;

use super::{FrameData, Shared, WindowInfo, shrink};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A running capture. Prefer Windows Graphics Capture; use the visible-desktop GDI
/// capture loop on systems where Windows cannot create a GraphicsCaptureItem.
/// GDI is only a compatibility fallback: covered/minimised windows and some
/// exclusive-fullscreen/protected surfaces may not be readable with it.
pub struct WindowCapture(CaptureMethod);

enum CaptureMethod {
    Native(CaptureControl<Handler, BoxError>),
    Gdi { stop: Arc<AtomicBool>, thread: Option<JoinHandle<()>>, shared: Arc<Shared> },
}

impl WindowCapture {
    pub fn is_finished(&self) -> bool {
        match &self.0 {
            CaptureMethod::Native(c) => c.is_finished(),
            CaptureMethod::Gdi { thread, .. } => thread.as_ref().is_none_or(JoinHandle::is_finished),
        }
    }
    /// Read from the screen's pixels (the compatibility capture), not by Windows Graphics Capture.
    pub fn is_compat(&self) -> bool {
        matches!(self.0, CaptureMethod::Gdi { .. })
    }
    pub fn stop(self) {
        match self.0 {
            CaptureMethod::Native(c) => {
                let _ = c.stop();
            }
            CaptureMethod::Gdi { stop, thread, shared } => {
                stop.store(true, Ordering::Relaxed);
                if let Some(thread) = thread {
                    let _ = thread.join();
                }
                shared.compat_capture.store(false, Ordering::Relaxed);
            }
        }
    }
}

/// Compatibility capture for systems where Windows Graphics Capture can't start (a stripped-down
/// Windows without its runtime): the target's place on screen, copied with GDI.
///
/// What is copied is what the screen shows there, so while it runs:
/// - the app's own overlays keep out of screen captures (`Shared::compat_capture`, see overlay.rs),
///   or the app would read its own Arabic back;
/// - `rect` gives a place only while the target can be read (a window: only while it's the one in
///   front), so other apps' windows are never read. Meanwhile it waits; it ends when `alive` says the
///   target is gone (a window closed), and the capture manager starts again.
fn start_gdi(
    shared: Arc<Shared>,
    alive: impl Fn() -> bool + Send + 'static,
    rect: impl Fn() -> Option<(i32, i32, i32, i32)> + Send + 'static,
) -> anyhow::Result<WindowCapture> {
    if !alive() {
        anyhow::bail!("GDI fallback: the target is gone");
    }
    // the first picture at once when it can be read now, so a GDI failure shows right away
    if let Some((x, y, w, h)) = rect() {
        let first = grab_screen_region(x, y, w, h).ok_or_else(|| anyhow::anyhow!("GDI fallback: could not read pixels from the screen"))?;
        let (w, h) = (w as u32, h as u32);
        shared.put_preview(shrink(&first, w, h, w as usize * 4, false));
        shared.put_full(w, h, first);
    }
    shared.compat_capture.store(true, Ordering::Relaxed);
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let s = shared.clone();
    let thread = std::thread::Builder::new().name("vi-translator-gdi-capture".to_string()).spawn(move || {
        let mut last_full = Instant::now();
        let mut last_preview = Instant::now();
        while !stopped.load(Ordering::Relaxed) {
            // GDI copies the screen through the CPU: at most ~30 frames a second
            std::thread::sleep(Duration::from_millis(33));
            if !alive() {
                break;
            }
            let full = s.full_due(last_full);
            let preview = s.preview_due(last_preview);
            if !full && !preview {
                continue;
            }
            // (not readable now: minimised, or another window in front; waits)
            let Some((x, y, w, h)) = rect() else { continue };
            if let Some(rgba) = grab_screen_region(x, y, w, h) {
                let (w, h) = (w as u32, h as u32);
                if preview {
                    s.put_preview(shrink(&rgba, w, h, w as usize * 4, false));
                    last_preview = Instant::now();
                }
                if full {
                    s.put_full(w, h, rgba);
                    last_full = Instant::now();
                }
            }
        }
        s.compat_capture.store(false, Ordering::Relaxed);
    })?;
    Ok(WindowCapture(CaptureMethod::Gdi { stop, thread: Some(thread), shared }))
}

/// Ask Windows once for capture without its yellow border round the captured window or screen:
/// `IsBorderRequired` = false (Windows 11) only takes the border away for an app that asked for
/// borderless capture (`GraphicsCaptureAccess`; a desktop app like this one is allowed without a
/// prompt). False where Windows hasn't that (Windows 10, whose border stays) or says no.
fn borderless_allowed() -> bool {
    static ALLOWED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ALLOWED.get_or_init(|| {
        use windows::Graphics::Capture::{GraphicsCaptureAccess, GraphicsCaptureAccessKind};
        use windows::Security::Authorization::AppCapabilityAccess::AppCapabilityAccessStatus;
        let (tx, rx) = std::sync::mpsc::channel();
        // (on a thread of its own: an answer that never comes can't hold up the capture)
        std::thread::spawn(move || {
            let r = GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless).and_then(|op| op.join());
            let _ = tx.send(r);
        });
        let answer = rx.recv_timeout(Duration::from_secs(3));
        if std::env::var_os("VI_TRANSLATOR_DEBUG").is_some() {
            eprintln!("[capture] borderless capture: {answer:?}");
        }
        matches!(answer, Ok(Ok(s)) if s == AppCapabilityAccessStatus::Allowed)
    })
}

/// The border settings to try, in order (once borderless capture was asked for): none, then the default.
fn border_settings() -> [DrawBorderSettings; 2] {
    let _ = borderless_allowed();
    [DrawBorderSettings::WithoutBorder, DrawBorderSettings::Default]
}

/// Start capturing the window titled `title`; returns the capture and the window's handle.
pub fn start_window(title: &str, shared: Arc<Shared>) -> anyhow::Result<(WindowCapture, isize)> {
    let win = find(title).ok_or_else(|| anyhow::anyhow!("Window \"{title}\" not found. Open it, or pick another window."))?;
    let hwnd = win.as_raw_hwnd() as isize;
    // on an HDR screen: its frames in HDR, for the game's colours (see `hdr_white`)
    let hdr = window_screen(hwnd).and_then(|d| hdr_white(&d));
    // Hide the yellow capture border where Windows allows it (`border_settings`); fall back if refused.
    let mut last_err = String::new();
    for border in border_settings() {
        let settings = Settings::new(
            win,
            CursorCaptureSettings::WithoutCursor,
            border,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            if hdr.is_some() { ColorFormat::Rgba16F } else { ColorFormat::Rgba8 },
            (shared.clone(), hdr),
        );
        match Handler::start_free_threaded(settings) {
            Ok(c) => return Ok((WindowCapture(CaptureMethod::Native(c)), hwnd)),
            Err(e) => last_err = format!("Capture failed: {e}"),
        }
    }
    // The native path failed for this window. Keep a working desktop-pixel
    // fallback rather than displaying ItemConvertFailed forever.
    eprintln!("VI Translator: native window capture failed for {title:?}: {last_err}; trying GDI");
    let alive = move || unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(Some(HWND(hwnd as *mut _))).as_bool() };
    // (only while it's the window in front: what covers it is never read)
    match start_gdi(shared, alive, move || focused_client_rect(hwnd)) {
        Ok(c) => Ok((c, hwnd)),
        Err(e) => Err(anyhow::anyhow!(
            "{}; GDI compatibility capture also failed: {e:#}",
            why_not(win, title).unwrap_or(last_err)
        )),
    }
}

/// Why Windows won't capture this window, in words the player can act on (the capture library only
/// says it "failed to convert" it).
fn why_not(win: Window, title: &str) -> Option<String> {
    if elevated(process_of(win.as_raw_hwnd() as isize)) && !elevated(unsafe { windows::Win32::System::Threading::GetCurrentProcessId() }) {
        return Some(format!(
            "Windows won't let VI Translator read \"{title}\": that app runs as administrator. Run VI Translator as administrator too (right-click it, Run as administrator), or start the other app normally."
        ));
    }
    let item: Result<windows_capture::settings::GraphicsCaptureItemType, windows::core::Error> = win.try_into();
    let e = item.err()?;
    Some(format!("Windows can't capture \"{title}\" ({}). If it's minimised, restore it; if it runs as administrator, run VI Translator as administrator too.", e.message()))
}

fn process_of(hwnd: isize) -> u32 {
    let mut pid = 0u32;
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(HWND(hwnd as *mut _), Some(&mut pid));
    }
    pid
}

/// The process runs as administrator (elevated); false when that can't be told.
fn elevated(pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
    use windows::Win32::System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};
    if pid == 0 {
        return false;
    }
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return false };
        let mut token = windows::Win32::Foundation::HANDLE::default();
        let mut out = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = OpenProcessToken(process, TOKEN_QUERY, &mut token).is_ok()
            && GetTokenInformation(token, TokenElevation, Some(&mut out as *mut _ as *mut _), std::mem::size_of::<TOKEN_ELEVATION>() as u32, &mut len).is_ok();
        if !token.is_invalid() {
            let _ = CloseHandle(token);
        }
        let _ = CloseHandle(process);
        ok && out.TokenIsElevated != 0
    }
}

struct Handler {
    shared: Arc<Shared>,
    /// An HDR screen's SDR white (its frames come as FP16 scRGB, turned into SDR here)
    hdr: Option<f32>,
    last: Instant,
    last_preview: Instant,
    scratch: Vec<u8>,
}

impl GraphicsCaptureApiHandler for Handler {
    type Flags = (Arc<Shared>, Option<f32>);
    type Error = BoxError;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        let ago = Instant::now() - Duration::from_secs(1);
        let (shared, hdr) = ctx.flags;
        if std::env::var_os("VI_TRANSLATOR_DEBUG").is_some() {
            eprintln!("[capture] HDR screen: {hdr:?}");
        }
        Ok(Self { shared, hdr, last: ago, last_preview: ago, scratch: Vec::new() })
    }

    fn on_frame_arrived(&mut self, frame: &mut Frame, _control: InternalCaptureControl) -> Result<(), Self::Error> {
        vi_translator_core::fork::linelog::note_arrived(); // fork: measurement log (every frame the source sends)
        // Full frames (a 4K frame is ~33 MB) only at the capture rate — plenty for reading text;
        // small preview frames at the (higher) preview rate, while the preview is watched.
        let full = self.shared.full_due(self.last);
        let preview = self.shared.preview_due(self.last_preview);
        if !full && !preview {
            return Ok(());
        }
        let fork_t0 = Instant::now(); // fork: measurement log (what taking a frame costs)
        let mut buf = frame.buffer_without_title_bar()?;
        let (w, h) = (buf.width(), buf.height());
        if let Some(white) = self.hdr {
            // HDR: one SDR picture for both (the preview shrinks it)
            let pitch = buf.row_pitch() as usize;
            let rgba = super::scrgb_to_rgba(buf.as_raw_buffer(), w, h, pitch, white);
            if preview {
                self.last_preview = Instant::now();
                self.shared.put_preview(shrink(&rgba, w, h, w as usize * 4, false));
                vi_translator_core::fork::linelog::note_preview_cost(self.last_preview.elapsed()); // fork: measurement log
            }
            if full {
                self.last = Instant::now();
                self.shared.put_full(w, h, rgba);
                vi_translator_core::fork::linelog::note_copy_cost(fork_t0.elapsed()); // fork: measurement log (with the HDR picture brought to SDR)
            }
            return Ok(());
        }
        if preview {
            self.last_preview = Instant::now();
            let pitch = buf.row_pitch() as usize;
            self.shared.put_preview(shrink(buf.as_raw_buffer(), w, h, pitch, false));
            vi_translator_core::fork::linelog::note_preview_cost(self.last_preview.elapsed()); // fork: measurement log
        }
        if full {
            self.last = Instant::now();
            let rgba = buf.as_nopadding_buffer(&mut self.scratch).to_vec();
            self.shared.put_full(w, h, rgba);
            vi_translator_core::fork::linelog::note_copy_cost(fork_t0.elapsed()); // fork: measurement log
        }
        Ok(())
    }

    fn on_closed(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Windows HDR on the screen `device` ("\\.\DISPLAY1"): its SDR white in scRGB units (1.0 = 80 nits;
/// Windows' "SDR content brightness", 200 nits = 2.5), or None when it shows SDR (Windows' automatic
/// colour management on an SDR screen isn't HDR). On an HDR screen Windows' 8-bit capture clips what
/// is brighter than SDR white channel by channel, and a game's coloured highlights came out white:
/// there its frames are taken in FP16 and brought to SDR here (`capture::scrgb_to_rgba`). Looked at
/// when the capture starts (HDR switched on or off meanwhile: from the next start).
pub fn hdr_white(device: &str) -> Option<f32> {
    use windows::Win32::Devices::Display::*;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); np as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), None) != ERROR_SUCCESS {
            return None;
        }
        let header = |kind, size: usize, adapter, id| DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size as u32, adapterId: adapter, id };
        for p in &paths[..np as usize] {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(), p.sourceInfo.adapterId, p.sourceInfo.id),
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0 {
                continue;
            }
            let n = source.viewGdiDeviceName.iter().position(|&c| c == 0).unwrap_or(source.viewGdiDeviceName.len());
            if String::from_utf16_lossy(&source.viewGdiDeviceName[..n]) != device {
                continue;
            }
            let mut color = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
                header: header(DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO, std::mem::size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(), p.targetInfo.adapterId, p.targetInfo.id),
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut color.header) != 0 {
                return None;
            }
            // bits: supported, enabled, wide colour enforced (colour management on an SDR screen), force-disabled
            let bits = color.Anonymous.value;
            if bits & 0b0010 == 0 || bits & 0b1100 != 0 {
                return None;
            }
            let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
                header: header(DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL, std::mem::size_of::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(), p.targetInfo.adapterId, p.targetInfo.id),
                ..Default::default()
            };
            // (1000 = 80 nits; when Windows doesn't say: its default, 200 nits)
            let level = if DisplayConfigGetDeviceInfo(&mut white.header) == 0 && white.SDRWhiteLevel > 0 { white.SDRWhiteLevel as f32 / 1000.0 } else { 2.5 };
            return Some(level.clamp(1.0, 6.25));
        }
        None
    }
}

/// The screen a window is (mostly) on: its device name, as `hdr_white` takes it.
fn window_screen(hwnd: isize) -> Option<String> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MONITORINFOEXW, MonitorFromWindow};
    unsafe {
        let m = MonitorFromWindow(HWND(hwnd as *mut _), MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFOEXW { monitorInfo: MONITORINFO { cbSize: std::mem::size_of::<MONITORINFOEXW>() as u32, ..Default::default() }, ..Default::default() };
        if !GetMonitorInfoW(m, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO).as_bool() {
            return None;
        }
        let n = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
        Some(String::from_utf16_lossy(&info.szDevice[..n]))
    }
}

/// The screens (monitors): their id (Windows' device name, "\\.\DISPLAY1") and a name to show.
pub fn list_screens() -> Vec<super::ScreenInfo> {
    let mut out: Vec<super::ScreenInfo> = Monitor::enumerate()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| {
            let id = m.device_name().ok()?;
            let n = m.index().unwrap_or(0);
            let (w, h) = (m.width().unwrap_or(0), m.height().unwrap_or(0));
            let name = m.name().ok().filter(|s| !s.trim().is_empty());
            Some(super::ScreenInfo { id, number: n, name: name.unwrap_or_default(), w, h })
        })
        .collect();
    out.sort_by_key(|s| s.number);
    out
}

fn monitor(id: &str) -> Option<Monitor> {
    Monitor::enumerate().ok()?.into_iter().find(|m| m.device_name().is_ok_and(|d| d == id))
}

/// Where a screen is on the desktop, in pixels: (x, y, width, height).
pub fn screen_rect(id: &str) -> Option<(i32, i32, i32, i32)> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
    let m = monitor(id)?;
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    unsafe { GetMonitorInfoW(HMONITOR(m.as_raw_hmonitor()), &mut info) }.as_bool().then(|| {
        let r = info.rcMonitor;
        (r.left, r.top, r.right - r.left, r.bottom - r.top)
    })
}

/// Start capturing a whole screen.
pub fn start_screen(id: &str, shared: Arc<Shared>) -> anyhow::Result<WindowCapture> {
    let m = monitor(id).ok_or_else(|| anyhow::anyhow!("Screen \"{id}\" not found (unplugged?). Pick another source."))?;
    let hdr = hdr_white(id);
    let mut last_err = String::new();
    for border in border_settings() {
        let settings = Settings::new(
            m,
            CursorCaptureSettings::WithoutCursor,
            border,
            SecondaryWindowSettings::Default,
            MinimumUpdateIntervalSettings::Default,
            DirtyRegionSettings::Default,
            if hdr.is_some() { ColorFormat::Rgba16F } else { ColorFormat::Rgba8 },
            (shared.clone(), hdr),
        );
        match Handler::start_free_threaded(settings) {
            Ok(c) => return Ok(WindowCapture(CaptureMethod::Native(c))),
            Err(e) => last_err = format!("Capture failed: {e}"),
        }
    }
    // On stripped-down Windows installations, WinRT's GraphicsCaptureItem
    // conversion can fail for *every* display. BitBlt needs no WinRT runtime.
    eprintln!("VI Translator: native screen capture failed for {id:?}: {last_err}; trying GDI");
    let id_owned = id.to_string();
    let id_alive = id_owned.clone();
    start_gdi(shared, move || screen_rect(&id_alive).is_some(), move || screen_rect(&id_owned)).map_err(|e| {
        anyhow::anyhow!("{last_err}; GDI compatibility capture also failed: {e:#}")
    })
}

/// Capture a single frame of a window, or of a screen ("screen:<id>"), for `--grab` and the
/// picker of the game's part.
pub fn grab_once(title: &str) -> anyhow::Result<Arc<FrameData>> {
    if let Some(id) = title.strip_prefix(super::SCREEN_PREFIX) {
        let shared = Arc::new(Shared::default());
        let control = start_screen(id, shared.clone())?;
        let start = Instant::now();
        let frame = loop {
            if let Some(f) = shared.latest() {
                break Ok(f);
            }
            if start.elapsed() > Duration::from_secs(5) {
                break Err(anyhow::anyhow!("no picture from the screen within 5 s"));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        control.stop();
        return frame;
    }
    let shared = Arc::new(Shared::default());
    // Reuse start_window, including its compatibility path, for screenshots
    // and the crop picker. Before this change, grab_once bypassed the fallback.
    let (control, _hwnd) = start_window(title, shared.clone())?;
    let start = Instant::now();
    let frame = loop {
        if let Some(f) = shared.latest() {
            break Ok(f);
        }
        if start.elapsed() > Duration::from_secs(5) {
            break Err(anyhow::anyhow!("no frame within 5 s (is the window minimised?)"));
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    control.stop();
    frame
}

/// Visible top-level windows that can be captured (excluding this app's own windows).
pub fn list_windows(own_title: &str) -> Vec<WindowInfo> {
    let mut out: Vec<WindowInfo> = Window::enumerate()
        .unwrap_or_default()
        .into_iter()
        .filter(|w| w.is_valid())
        .filter_map(|w| {
            let title = w.title().ok()?.trim().to_string();
            if title.is_empty() || title == own_title {
                return None;
            }
            // invisible system windows that only confuse the list
            let process = w.process_name().unwrap_or_default();
            if matches!(process.to_lowercase().as_str(), "textinputhost.exe" | "shellexperiencehost.exe" | "searchhost.exe" | "startmenuexperiencehost.exe")
                || title == "Program Manager"
            {
                return None;
            }
            Some(WindowInfo { title, process: w.process_name().unwrap_or_default() })
        })
        .collect();
    out.sort_by_key(|w| w.title.to_lowercase());
    out
}

/// Client area of a window in screen pixels: (x, y, width, height).
pub fn client_rect(hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    if hwnd == 0 {
        return None;
    }
    let h = HWND(hwnd as *mut _);
    unsafe {
        if !IsWindow(Some(h)).as_bool() || IsIconic(h).as_bool() {
            return None;
        }
        let mut rc = RECT::default();
        GetClientRect(h, &mut rc).ok()?;
        let mut pt = POINT { x: 0, y: 0 };
        let _ = ClientToScreen(h, &mut pt);
        let (w, hh) = (rc.right - rc.left, rc.bottom - rc.top);
        (w > 50 && hh > 50).then_some((pt.x, pt.y, w, hh))
    }
}

pub fn foreground_window() -> isize {
    unsafe { GetForegroundWindow().0 as isize }
}

/// The window's client rect, only while it's the one in front.
pub fn focused_client_rect(hwnd: isize) -> Option<(i32, i32, i32, i32)> {
    if hwnd == 0 || foreground_window() != hwnd {
        return None;
    }
    client_rect(hwnd)
}


/// Copy a region of the screen (physical pixels) as RGBA — what the user actually sees there,
/// including our Arabic overlay windows on top of the game.
pub fn grab_screen_region(x: i32, y: i32, w: i32, h: i32) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap, CreateCompatibleDC,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, ReleaseDC, SRCCOPY, SelectObject,
    };
    if w <= 0 || h <= 0 {
        return None;
    }
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, w, h);
        let old = SelectObject(mem, bmp.into());
        let ok = BitBlt(mem, 0, 0, w, h, Some(screen), x, y, SRCCOPY | CAPTUREBLT).is_ok();
        let mut info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let lines = GetDIBits(mem, bmp, 0, h as u32, Some(buf.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS);
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if !ok || lines == 0 {
            return None;
        }
        for px in buf.chunks_exact_mut(4) {
            px.swap(0, 2); // BGRA -> RGBA
            px[3] = 255;
        }
        Some(buf)
    }
}


/// The handle of the window titled `title`.
pub fn find_window(title: &str) -> Option<isize> {
    find(title).map(|w| w.as_raw_hwnd() as isize)
}

/// The window titled `title`; else one whose title contains it; else the program it names. A source
/// saved as "PS Remote Play" is its window under any title (Windows set to another language
/// translates it: "اللعب عن بُعد").
fn find(title: &str) -> Option<Window> {
    // a visible window with that title first: an app may also have hidden windows of the same name
    // (which can't be captured), and FindWindow may return one of those
    let all: Vec<Window> = Window::enumerate().unwrap_or_default().into_iter().filter(|w| w.is_valid()).collect();
    if let Some(w) = all.iter().find(|w| w.title().is_ok_and(|t| t.trim() == title.trim())) {
        return Some(*w);
    }
    if let Ok(w) = Window::from_name(title) {
        return Some(w);
    }
    if let Some(w) = all.iter().find(|w| w.title().is_ok_and(|t| !t.trim().is_empty() && t.contains(title))) {
        return Some(*w);
    }
    let program = super::program_of(title)?;
    all.into_iter().find(|w| w.process_name().is_ok_and(|p| p.eq_ignore_ascii_case(program)) && w.title().is_ok_and(|t| !t.trim().is_empty()))
}
