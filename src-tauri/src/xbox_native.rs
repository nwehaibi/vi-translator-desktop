//! Xbox Remote Play with the app's own player (the `vi-translator-xbox` crate), as the phone apps
//! have it: the player signs in with a code typed at microsoft.com/link (no password in the app),
//! chooses the console once, and from then on the Xbox is a picture source ("xbox:"): its picture
//! goes straight to the reading and the preview (Play shows it full screen), its sound to the
//! speakers, and this computer's controllers to it. Microsoft's web player (xbox.rs) stays as the
//! other way.
//!
//! What the app keeps (xbox.json in its data folder): Microsoft's refresh token (no password), the
//! gamertag, the chosen console and the settings. Tokens are never logged.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use vi_translator_ps as ps;
use vi_translator_xbox as xb;
use xb::play::Status;

use crate::capture::{Shared, YuvColor, YuvFrame};

/// The source value for the app's own Xbox player.
pub const PREFIX: &str = "xbox:";

/// The chosen console.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Console {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: String,
}

/// xbox.json
#[derive(Serialize, Deserialize, Clone, Default, PartialEq)]
pub struct Saved {
    /// Microsoft's refresh token (empty: signed out)
    #[serde(default)]
    pub refresh: String,
    #[serde(default)]
    pub gamertag: String,
    #[serde(default)]
    pub console: Option<Console>,
    /// the stream's height: 1080 or 720 (0: 1080)
    #[serde(default)]
    pub resolution: u32,
    #[serde(default)]
    pub sound_off: bool,
    #[serde(default)]
    pub volume: Option<u32>,
    #[serde(default)]
    pub controllers_off: bool,
}

impl std::fmt::Debug for Saved {
    // (the refresh token stays out of any log)
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Saved {{ signed_in: {}, console: {:?}, resolution: {} }}", !self.refresh.is_empty(), self.console, self.resolution)
    }
}

/// A code being typed: what to show, and when it runs out.
#[derive(Clone)]
struct Code {
    user_code: String,
    uri: String,
    expires_at: Instant,
}

struct Global {
    app: AppHandle,
    file: PathBuf,
    saved: Mutex<Saved>,
    account: Mutex<Option<Arc<xb::auth::Account>>>,
    /// the code shown while signing in; a new sign-in (or Cancel) bumps `sign_in_run`
    code: Mutex<Option<Code>>,
    sign_in_run: AtomicU64,
    /// what the last sign-in or stream went wrong with (English; the page translates it)
    error: Mutex<String>,
    /// the stream starts again (its quality changed, or the console or account did)
    restart: AtomicBool,
    settings: Arc<xb::play::Settings>,
}

static G: OnceLock<Global> = OnceLock::new();

fn g() -> &'static Global {
    G.get().expect("xbox_native::init")
}

/// At startup: the app and its data folder.
pub fn init(app: AppHandle, data_dir: &Path) {
    let file = data_dir.join("xbox.json");
    let saved: Saved = std::fs::read_to_string(&file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let _ = G.set(Global {
        app,
        file,
        account: Mutex::new(None),
        code: Mutex::new(None),
        sign_in_run: AtomicU64::new(0),
        error: Mutex::new(String::new()),
        restart: AtomicBool::new(false),
        settings: Arc::new(xb::play::Settings::new(!saved.sound_off, saved.volume.unwrap_or(100), !saved.controllers_off)),
        saved: Mutex::new(saved),
    });
}

fn write(s: &Saved) {
    if let Ok(t) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(&g().file, t);
    }
}

fn save(s: &Saved) {
    *g().saved.lock().unwrap() = s.clone();
    write(s);
    emit();
}

fn emit() {
    let _ = g().app.emit_to("main", "xbox", status());
}

pub fn saved() -> Saved {
    g().saved.lock().unwrap().clone()
}

fn set_error(e: impl Into<String>) {
    *g().error.lock().unwrap() = e.into();
    emit();
}

/// The Source tab's Xbox panel: signed in? the code to type? which console? the settings.
pub fn status() -> serde_json::Value {
    let g = g();
    let s = g.saved.lock().unwrap().clone();
    let code = g.code.lock().unwrap().clone().filter(|c| c.expires_at > Instant::now());
    serde_json::json!({
        "signed_in": !s.refresh.is_empty(),
        "gamertag": s.gamertag,
        "code": code.map(|c| serde_json::json!({ "code": c.user_code, "uri": c.uri, "seconds": c.expires_at.saturating_duration_since(Instant::now()).as_secs() })),
        "console": s.console,
        "resolution": if s.resolution == 720 { 720 } else { 1080 },
        "sound": !s.sound_off,
        "volume": s.volume.unwrap_or(100),
        "controllers": !s.controllers_off,
        "error": g.error.lock().unwrap().clone(),
    })
}

/// The signed-in account (made from the saved refresh token once; new refresh tokens are kept).
fn account() -> Option<Arc<xb::auth::Account>> {
    let g = g();
    let mut a = g.account.lock().unwrap();
    if a.is_none() {
        let refresh = saved().refresh;
        if refresh.is_empty() {
            return None;
        }
        *a = Some(Arc::new(xb::auth::Account::new(&refresh, keep_refresh)));
    }
    a.clone()
}

fn keep_refresh(r: &str) {
    let mut s = saved();
    s.refresh = r.to_string();
    *g().saved.lock().unwrap() = s.clone();
    write(&s);
}

/// Microsoft no longer takes the sign-in: forgotten (the console stays chosen for the next one).
fn signed_out() {
    *g().account.lock().unwrap() = None;
    let mut s = saved();
    s.refresh.clear();
    save(&s);
}

// ---------------------------------------------------------------------------------------- signing in

/// A new code to type at microsoft.com/link; waits for it in the background (the page hears "xbox"
/// when it's done or fails).
pub async fn sign_in() -> Result<serde_json::Value, String> {
    let g = g();
    let run = g.sign_in_run.fetch_add(1, Ordering::Relaxed) + 1;
    *g.error.lock().unwrap() = String::new();
    let dc = xb::auth::request_code().await.map_err(|e| e.to_string())?;
    *g.code.lock().unwrap() = Some(Code { user_code: dc.user_code.clone(), uri: dc.verification_uri.clone(), expires_at: dc.expires_at });
    emit();
    tauri::async_runtime::spawn(async move {
        let mut interval = dc.interval;
        loop {
            tokio::time::sleep(Duration::from_secs(interval)).await;
            if g.sign_in_run.load(Ordering::Relaxed) != run {
                return;
            }
            match xb::auth::poll_code(&dc).await {
                Ok(xb::auth::Poll::Pending) => {}
                Ok(xb::auth::Poll::SlowDown) => interval += 5,
                Ok(xb::auth::Poll::Done(tokens)) => {
                    let refresh = tokens.refresh.clone();
                    let a = Arc::new(xb::auth::Account::with_tokens(tokens, keep_refresh));
                    let gamertag = a.gamertag().await.unwrap_or_default();
                    *g.account.lock().unwrap() = Some(a);
                    *g.code.lock().unwrap() = None;
                    let mut s = saved();
                    s.refresh = refresh;
                    s.gamertag = gamertag;
                    *g.error.lock().unwrap() = String::new();
                    save(&s);
                    g.restart.store(true, Ordering::Relaxed);
                    return;
                }
                Ok(xb::auth::Poll::Failed(why)) => {
                    *g.code.lock().unwrap() = None;
                    return set_error(if why == "expired_token" {
                        "The code expired. Get a new one.".to_string()
                    } else if why == "authorization_declined" {
                        "The sign-in was declined.".to_string()
                    } else {
                        format!("Microsoft refused the sign-in ({why}).")
                    });
                }
                // (no network for a moment: keep waiting)
                Err(_) => {}
            }
        }
    });
    Ok(status())
}

pub fn cancel_sign_in() {
    g().sign_in_run.fetch_add(1, Ordering::Relaxed);
    *g().code.lock().unwrap() = None;
    emit();
}

/// Opens the page the code is typed on (only Microsoft's own).
pub fn open_link() -> Result<(), String> {
    let uri = g().code.lock().unwrap().as_ref().map(|c| c.uri.clone()).unwrap_or_else(|| "https://www.microsoft.com/link".into());
    let ok = uri.parse::<tauri::Url>().ok().is_some_and(|u| {
        u.scheme() == "https" && u.host_str().is_some_and(|h| h == "microsoft.com" || h.ends_with(".microsoft.com") || h.ends_with(".live.com"))
    });
    crate::open_url(if ok { &uri } else { "https://www.microsoft.com/link" })
}

pub fn sign_out() {
    cancel_sign_in();
    *g().account.lock().unwrap() = None;
    let mut s = saved();
    s.refresh.clear();
    s.gamertag.clear();
    s.console = None;
    *g().error.lock().unwrap() = String::new();
    save(&s);
    g().restart.store(true, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------------------- consoles

/// A console on the account, as listed.
#[derive(Serialize)]
pub struct Listed {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub power: String,
}

/// The account's consoles.
pub async fn consoles() -> Result<Vec<Listed>, String> {
    let a = account().ok_or("Sign in first.")?;
    let r = async {
        let login = a.streaming().await?;
        xb::home::Home::new(login, 1080)?.consoles().await
    }
    .await;
    match r {
        Ok(l) => Ok(l.into_iter().map(|c| Listed { id: c.id, name: c.name, kind: c.kind, power: c.power }).collect()),
        Err(xb::Error::SignedOut) => {
            signed_out();
            Err(xb::play::SIGNED_OUT.into())
        }
        Err(e) => Err(e.to_string()),
    }
}

pub fn choose(console: Console) {
    let mut s = saved();
    s.console = Some(console);
    *g().error.lock().unwrap() = String::new();
    save(&s);
    g().restart.store(true, Ordering::Relaxed);
}

pub fn forget_console() {
    let mut s = saved();
    s.console = None;
    save(&s);
    g().restart.store(true, Ordering::Relaxed);
}

// ---------------------------------------------------------------------------------------- settings

pub fn set_resolution(r: u32) {
    let mut s = saved();
    s.resolution = if r == 720 { 720 } else { 1080 };
    save(&s);
    g().restart.store(true, Ordering::Relaxed);
}

/// The Xbox's sound on this computer: on or off, and its volume (0–100); heard at once.
pub fn set_sound(on: bool, volume: u32) {
    let g = g();
    g.settings.sound.store(on, Ordering::Relaxed);
    g.settings.volume.store(volume.min(100), Ordering::Relaxed);
    let mut s = saved();
    s.sound_off = !on;
    s.volume = Some(volume.min(100));
    *g.saved.lock().unwrap() = s.clone();
    write(&s);
}

/// This computer's controllers play on the Xbox (or not); at once.
pub fn set_controllers(on: bool) {
    g().settings.controllers.store(on, Ordering::Relaxed);
    let mut s = saved();
    s.controllers_off = !on;
    *g().saved.lock().unwrap() = s.clone();
    write(&s);
}

// ---------------------------------------------------------------------------------------- playing

/// The Xbox's picture going to `Shared` while it lives (as the PS5's does).
pub struct XboxCapture {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

impl XboxCapture {
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
    }
}

/// The picture into the app: every frame as YUY2 for the preview (at the preview rate) and as RGBA
/// for the reading (at the capture rate). BT.709, studio range.
struct ToShared {
    shared: Arc<Shared>,
    times: Mutex<(Instant, Instant)>,
}

const COLOR: YuvColor = YuvColor { bt709: true, full_range: false };

impl xb::play::Screen for ToShared {
    fn frame(&self, f: xb::play::I420) {
        // fork: measurement log: every frame the console's stream gave, decoded; and this thread's CPU (the
        // decoder's: OpenH264, on the CPU, and the conversions below)
        vi_translator_core::fork::linelog::note_arrived();
        vi_translator_core::fork::linelog::note_source_cpu();
        let (full, preview) = {
            let t = self.times.lock().unwrap();
            (self.shared.full_due(t.0), self.shared.preview_due(t.1))
        };
        if !full && !preview {
            return;
        }
        let fork_t0 = Instant::now(); // fork: measurement log (what taking a frame costs: its conversions)
        let f = ps::play::I420 { w: f.w, h: f.h, y: f.y, u: f.u, v: f.v, y_stride: f.y_stride, uv_stride: f.uv_stride };
        let yuy2 = ps::play::i420_to_yuy2(&f, false);
        let w = f.w & !1;
        let fork_yuy2 = fork_t0.elapsed(); // fork: measurement log
        let mut t = self.times.lock().unwrap();
        if full {
            t.0 = Instant::now();
            self.shared.put_full(w, f.h, crate::capture::yuy2_to_rgba(&yuy2, w, f.h, COLOR));
            vi_translator_core::fork::linelog::note_copy_cost(fork_t0.elapsed()); // fork: measurement log (I420 to YUY2 to RGBA)
        }
        if preview {
            t.1 = Instant::now();
            self.shared.put_preview_yuv(YuvFrame { w, h: f.h, color: COLOR, data: yuy2 });
            // fork: measurement log (its YUY2, counted for the preview when no full frame was taken with it)
            vi_translator_core::fork::linelog::note_preview_cost(t.1.elapsed() + if full { Duration::ZERO } else { fork_yuy2 });
        }
    }

    fn log(&self, level: i32, text: &str) {
        if level <= 2 || std::env::var_os("VI_TRANSLATOR_DEBUG").is_some() {
            eprintln!("[xbox] {text}");
        }
    }
}

/// Minutes east of UTC here, as the session takes them.
fn tz_minutes() -> i32 {
    #[cfg(windows)]
    {
        use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
        let mut tz = TIME_ZONE_INFORMATION::default();
        let r = unsafe { GetTimeZoneInformation(&mut tz) };
        // (2: daylight saving time now)
        let bias = tz.Bias + if r == 2 { tz.DaylightBias } else { tz.StandardBias };
        -bias
    }
    #[cfg(not(windows))]
    {
        chrono::Local::now().offset().local_minus_utc() / 60
    }
}

/// The status line for where the stream is (English; the page translates it).
fn status_line(s: &Status, name: &str, pads: u64) -> String {
    match s {
        Status::Starting => format!("Starting the stream from {name} …"),
        Status::Waking => format!("Waking {name} …"),
        Status::Waiting => format!("Waiting for {name} to get ready …"),
        Status::Connecting => format!("Connecting to {name} …"),
        Status::Playing => format!("Playing on {name} ({pads} controller(s))"),
        Status::Ended(why) | Status::Failed(why) => why.clone(),
    }
}

/// Streams from the chosen console until stopped, the stream ends or it fails (the manager starts
/// it again; after a failure, not before 15 s, so a console that refuses isn't asked every second).
pub fn start(shared: Arc<Shared>) -> XboxCapture {
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let handle = std::thread::spawn(move || {
        let set = |s: String| shared.set_status(s);
        let wait = |secs: u64| {
            let t = Instant::now();
            while t.elapsed() < Duration::from_secs(secs) && !s2.load(Ordering::Relaxed) && !g().restart.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(200));
            }
        };
        g().restart.store(false, Ordering::Relaxed);
        let Some(account) = account() else {
            set("Xbox: sign in with a code in Source first.".into());
            return wait(30);
        };
        let Some(c) = saved().console else {
            set("Xbox: choose your console in Source first.".into());
            return wait(30);
        };
        let screen = Arc::new(ToShared { shared: shared.clone(), times: Mutex::new((Instant::now() - Duration::from_secs(1), Instant::now() - Duration::from_secs(1))) });
        let res = if saved().resolution == 720 { 720 } else { 1080 };
        let player = xb::play::Player::start(account, &c.id, res, tz_minutes(), g().settings.clone(), screen);
        let mut last = None;
        let mut last_line = Instant::now() - Duration::from_secs(5);
        while !s2.load(Ordering::Relaxed) && !g().restart.load(Ordering::Relaxed) {
            let st = player.status();
            let pads = player.stats.controllers.load(Ordering::Relaxed);
            vi_translator_core::fork::linelog::note_source_bad_frames(player.stats.bad_frames.load(Ordering::Relaxed)); // fork: measurement log
            if last.as_ref() != Some(&st) || last_line.elapsed() > Duration::from_secs(1) {
                set(status_line(&st, &c.name, pads));
                last_line = Instant::now();
                if st == Status::Playing && last.as_ref() != Some(&Status::Playing) {
                    *g().error.lock().unwrap() = String::new();
                    emit();
                }
            }
            if st.over() {
                if let Status::Failed(why) = &st {
                    if why == xb::play::SIGNED_OUT {
                        signed_out();
                    }
                    set_error(why.clone());
                }
                drop(player);
                set(status_line(&st, &c.name, pads));
                return wait(if matches!(st, Status::Failed(_)) { 15 } else { 3 });
            }
            last = Some(st);
            std::thread::sleep(Duration::from_millis(100));
        }
        drop(player);
    });
    XboxCapture { stop, handle }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_saved_file_keeps_its_fields_and_hides_the_token_in_logs() {
        let s = Saved {
            refresh: "secret-refresh".into(),
            gamertag: "Player".into(),
            console: Some(Console { id: "F400".into(), name: "Living room".into(), kind: "XboxSeriesX".into() }),
            resolution: 720,
            ..Default::default()
        };
        let back: Saved = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert!(back == s);
        assert!(!format!("{s:?}").contains("secret"));
        // (an old or empty file is a signed-out default)
        let empty: Saved = serde_json::from_str("{}").unwrap();
        assert!(empty.refresh.is_empty() && empty.console.is_none());
    }

    #[test]
    fn status_lines_name_the_console() {
        assert_eq!(status_line(&Status::Waking, "Den", 0), "Waking Den …");
        assert_eq!(status_line(&Status::Playing, "Den", 2), "Playing on Den (2 controller(s))");
        assert_eq!(status_line(&Status::Failed("x".into()), "Den", 0), "x");
    }
}
