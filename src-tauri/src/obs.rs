//! OBS as a picture source, through OBS's built-in WebSocket server (obs-websocket v5).
//!
//! When OBS owns the capture card (its "Video Capture Device" opens the card exclusively), we ask
//! OBS for a snapshot of that source ~8 times a second instead. Only localhost is ever contacted.

use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::capture::{FrameData, Shared};

/// Prefix that marks an OBS source in the config's source field.
pub const PREFIX: &str = "obs:";

pub struct Obs {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

impl Obs {
    /// Connect to OBS on this PC and log in (password from OBS → Tools → WebSocket Server Settings).
    pub fn connect(port: u16, password: &str) -> Result<Self> {
        let addr = format!("127.0.0.1:{port}");
        let tcp = TcpStream::connect_timeout(&addr.parse()?, Duration::from_secs(2))
            .with_context(|| format!("OBS isn't answering on port {port} — in OBS, turn on Tools → WebSocket Server Settings → Enable WebSocket server"))?;
        tcp.set_read_timeout(Some(Duration::from_secs(5)))?;
        let (ws, _) = tungstenite::client(format!("ws://{addr}"), MaybeTlsStream::Plain(tcp)).map_err(|e| anyhow!("OBS WebSocket handshake failed: {e}"))?;
        let mut obs = Self { ws, next_id: 0 };
        let hello = obs.recv_op(0)?;
        let mut identify = json!({ "rpcVersion": 1, "eventSubscriptions": 0 });
        if let Some(auth) = hello.get("authentication") {
            if password.is_empty() {
                bail!("OBS asks for a password: enter the one from OBS → Tools → WebSocket Server Settings");
            }
            let (challenge, salt) = (auth["challenge"].as_str().unwrap_or(""), auth["salt"].as_str().unwrap_or(""));
            let secret = b64(&Sha256::digest(format!("{password}{salt}")));
            identify["authentication"] = json!(b64(&Sha256::digest(format!("{secret}{challenge}"))));
        }
        obs.send(json!({ "op": 1, "d": identify }))?;
        obs.recv_op(2).map_err(|_| anyhow!("OBS refused the password (check Tools → WebSocket Server Settings)"))?;
        Ok(obs)
    }

    fn send(&mut self, v: Value) -> Result<()> {
        self.ws.send(Message::Text(v.to_string().into()))?;
        Ok(())
    }

    /// Wait for a message with this op code; returns its "d".
    fn recv_op(&mut self, op: u64) -> Result<Value> {
        loop {
            match self.ws.read()? {
                Message::Text(t) => {
                    let v: Value = serde_json::from_str(t.as_str())?;
                    if v["op"].as_u64() == Some(op) {
                        return Ok(v["d"].clone());
                    }
                }
                Message::Close(_) => bail!("OBS closed the connection"),
                _ => {}
            }
        }
    }

    fn request(&mut self, kind: &str, data: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id.to_string();
        self.send(json!({ "op": 6, "d": { "requestType": kind, "requestId": id, "requestData": data } }))?;
        loop {
            let d = self.recv_op(7)?;
            if d["requestId"].as_str() != Some(id.as_str()) {
                continue;
            }
            if d["requestStatus"]["result"].as_bool() != Some(true) {
                bail!("OBS: {}", d["requestStatus"]["comment"].as_str().unwrap_or("request failed"));
            }
            return Ok(d["responseData"].clone());
        }
    }

    /// Names of OBS's sources (inputs), video capture devices first.
    pub fn inputs(&mut self) -> Result<Vec<(String, String)>> {
        let r = self.request("GetInputList", json!({}))?;
        let mut out: Vec<(String, String)> = r["inputs"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|i| (i["inputName"].as_str().unwrap_or("").to_string(), i["inputKind"].as_str().unwrap_or("").to_string()))
                    .filter(|(n, _)| !n.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        out.sort_by_key(|(_, k)| !k.contains("dshow") && !k.contains("capture"));
        Ok(out)
    }

    fn image(&mut self, source: &str, extra: Value) -> Result<Vec<u8>> {
        let mut req = json!({ "sourceName": source });
        if let (Some(o), Some(e)) = (req.as_object_mut(), extra.as_object()) {
            o.extend(e.clone());
        }
        let r = self.request("GetSourceScreenshot", req)?;
        let url = r["imageData"].as_str().context("no image from OBS")?;
        let data = url.split_once(',').map(|(_, d)| d).unwrap_or(url);
        Ok(base64::engine::general_purpose::STANDARD.decode(data)?)
    }

    /// A small JPEG of the source for the live preview (≤ PREVIEW_W wide).
    pub fn preview_jpeg(&mut self, source: &str) -> Result<Vec<u8>> {
        self.image(source, json!({ "imageFormat": "jpg", "imageWidth": crate::capture::preview_w(), "imageCompressionQuality": 80 }))
    }

    /// The source's current picture, as OBS renders it (full size).
    pub fn snapshot(&mut self, source: &str) -> Result<FrameData> {
        // BMP: no compression to spend time on, on either side (localhost only)
        // high-quality JPEG: a 1080p BMP takes ~250 ms through base64, a JPEG ~30 ms, text reads the same
        let bytes = self.image(source, json!({ "imageFormat": "jpg", "imageCompressionQuality": 95 }))?;
        let img = image::load_from_memory(&bytes)?.to_rgba8();
        let (w, h) = img.dimensions();
        if w < 16 || h < 16 {
            bail!("OBS source \"{source}\" has no picture (is it showing in a scene?)");
        }
        Ok(FrameData { w, h, rgba: img.into_raw(), seq: 0 })
    }
}

impl Obs {
    /// OBS's canvas size (Settings → Video → Base (Canvas) Resolution).
    pub fn canvas(&mut self) -> Result<(u32, u32)> {
        let r = self.request("GetVideoSettings", json!({}))?;
        let w = r["baseWidth"].as_u64().context("no canvas width from OBS")? as u32;
        let h = r["baseHeight"].as_u64().context("no canvas height from OBS")? as u32;
        Ok((w, h))
    }
}

/// OBS sources for the Source list, and the canvas size if OBS gives it.
pub fn list(port: u16, password: &str) -> Result<(Vec<(String, String)>, Option<(u32, u32)>)> {
    let mut obs = Obs::connect(port, password)?;
    let inputs = obs.inputs()?;
    Ok((inputs, obs.canvas().ok()))
}

pub struct ObsCapture {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
    preview: JoinHandle<()>,
}

impl ObsCapture {
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.join();
        let _ = self.preview.join();
    }
}

/// Read OBS source `name` into `shared`: full frames (for reading text) at the capture rate on one
/// connection, small preview JPEGs at the preview rate on a second one, so neither waits for the other.
pub fn start(name: String, port: u16, password: String, shared: Arc<Shared>) -> ObsCapture {
    let stop = Arc::new(AtomicBool::new(false));
    let (s2, s3) = (stop.clone(), stop.clone());
    let (name2, password2, shared2) = (name.clone(), password.clone(), shared.clone());
    let handle = std::thread::spawn(move || {
        let mut obs = match Obs::connect(port, &password) {
            Ok(o) => o,
            Err(e) => {
                shared.set_status(format!("{e:#}"));
                std::thread::sleep(Duration::from_secs(2)); // the manager retries after this
                s2.store(true, Ordering::Relaxed); // and the preview thread ends too
                return;
            }
        };
        let mut said = false;
        let mut last = Instant::now() - Duration::from_secs(1);
        while !s2.load(Ordering::Relaxed) {
            if !shared.full_due(last) {
                std::thread::sleep(Duration::from_millis(4));
                continue;
            }
            last = Instant::now();
            match obs.snapshot(&name) {
                Ok(f) => {
                    if !said {
                        shared.set_status(format!("Capturing OBS source \"{name}\" ({}×{}, {} ms per frame)", f.w, f.h, last.elapsed().as_millis()));
                        said = true;
                    }
                    shared.put_full(f.w, f.h, f.rgba);
                }
                Err(e) => {
                    shared.set_status(format!("OBS source \"{name}\": {e:#}"));
                    std::thread::sleep(Duration::from_secs(2));
                    s2.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }
    });
    let preview = std::thread::spawn(move || {
        let mut obs: Option<Obs> = None;
        let mut last = Instant::now() - Duration::from_secs(1);
        while !s3.load(Ordering::Relaxed) {
            if !shared2.preview_due(last) || last.elapsed() < shared2.preview_interval() {
                std::thread::sleep(Duration::from_millis(3));
                continue;
            }
            last = Instant::now();
            if obs.is_none() {
                obs = Obs::connect(port, &password2).ok(); // connect only once someone watches
            }
            let Some(o) = obs.as_mut() else {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            };
            match o.preview_jpeg(&name2) {
                Ok(j) => {
                    vi_translator_core::fork::linelog::note_preview(); // fork: measurement log (OBS sends its preview ready-made)
                    shared2.put_preview_jpeg(j)
                }
                Err(_) => obs = None, // reconnect next time
            }
        }
    });
    ObsCapture { stop, handle, preview }
}
