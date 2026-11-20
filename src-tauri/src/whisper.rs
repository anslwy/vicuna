//! Optional on-device speech-to-text (Whisper via candle, pure Rust).
//!
//! Used by the Telegram bridge: a voice memo is transcribed on the server and
//! the transcript is shown on the phone before it goes to the lead. The model
//! is not bundled: the Telegram popup installs it into
//! `~/vicuna/models/whisper-<size>/` (config.json, tokenizer.json,
//! model.safetensors from Hugging Face) and uninstalls it again. Install is
//! offered only when the machine has enough RAM. Builds without the `whisper`
//! cargo feature report `available: false` and never transcribe.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use serde_json::{json, Value};

pub struct ModelSpec {
    pub id: &'static str,
    pub label: &'static str,
    pub repo: &'static str,
    pub size_mb: u64,
    /// total RAM the machine needs before we offer it
    pub min_ram_mb: u64,
}

pub const MODELS: &[ModelSpec] = &[
    ModelSpec { id: "tiny", label: "Whisper tiny (fast, multilingual)", repo: "openai/whisper-tiny", size_mb: 151, min_ram_mb: 2048 },
    ModelSpec { id: "base", label: "Whisper base (more accurate)", repo: "openai/whisper-base", size_mb: 290, min_ram_mb: 4096 },
];

const FILES: &[&str] = &["config.json", "tokenizer.json", "model.safetensors"];

pub fn spec(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id)
}

fn models_root() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VICUNA_MODELS_DIR") {
        return Some(PathBuf::from(p));
    }
    Some(crate::opencode::home_dir()?.join("vicuna").join("models"))
}

fn model_dir(id: &str) -> Option<PathBuf> {
    Some(models_root()?.join(format!("whisper-{id}")))
}

pub fn is_installed(id: &str) -> bool {
    model_dir(id).is_some_and(|d| FILES.iter().all(|f| d.join(f).is_file()))
}

/// The installed model to use (smallest first, so a tiny install stays fast).
pub fn installed_model() -> Option<&'static str> {
    MODELS.iter().map(|m| m.id).find(|id| is_installed(id))
}

pub fn available() -> bool {
    cfg!(feature = "whisper")
}

/// Total physical memory in MB, if we can tell.
pub fn total_ram_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let s = std::fs::read_to_string("/proc/meminfo").ok()?;
        return parse_meminfo_total(&s);
    }
    #[cfg(target_os = "macos")]
    {
        let o = std::process::Command::new("sysctl").args(["-n", "hw.memsize"]).output().ok()?;
        return String::from_utf8_lossy(&o.stdout).trim().parse::<u64>().ok().map(|b| b / 1024 / 1024);
    }
    #[allow(unreachable_code)]
    None
}

pub fn parse_meminfo_total(s: &str) -> Option<u64> {
    s.lines()
        .find(|l| l.starts_with("MemTotal:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|kb| kb.parse::<u64>().ok())
        .map(|kb| kb / 1024)
}

// ---------- install state ----------

#[derive(Default, Clone)]
struct Progress {
    model: String,
    done: u64,
    total: u64,
    error: String,
}

fn progress() -> &'static Mutex<Option<Progress>> {
    static P: OnceLock<Mutex<Option<Progress>>> = OnceLock::new();
    P.get_or_init(Default::default)
}

fn last_error() -> &'static Mutex<String> {
    static E: OnceLock<Mutex<String>> = OnceLock::new();
    E.get_or_init(Default::default)
}

pub fn status() -> Value {
    let ram = total_ram_mb();
    let models: Vec<Value> = MODELS
        .iter()
        .map(|m| {
            json!({
                "id": m.id, "label": m.label, "sizeMb": m.size_mb, "minRamMb": m.min_ram_mb,
                "installed": is_installed(m.id),
                // unknown RAM: let the user decide
                "fits": ram.map(|r| r >= m.min_ram_mb).unwrap_or(true),
            })
        })
        .collect();
    let p = progress().lock().ok().and_then(|g| g.clone());
    json!({
        "available": available(),
        "ramMb": ram,
        "models": models,
        "installing": p.as_ref().map(|p| json!({"model": p.model, "done": p.done, "total": p.total})),
        "error": last_error().lock().map(|e| e.clone()).unwrap_or_default(),
    })
}

/// Start downloading a model in the background; poll `status()` for progress.
pub fn start_install(id: &str) -> Result<Value, String> {
    if !available() {
        return Err("This Vicuna build has no voice transcription (built without the whisper feature).".into());
    }
    let m = spec(id).ok_or("unknown model")?;
    if let Some(r) = total_ram_mb() {
        if r < m.min_ram_mb {
            return Err(format!("{} needs at least {} GB of RAM; this machine has {:.1} GB.", m.label, m.min_ram_mb / 1024, r as f64 / 1024.0));
        }
    }
    {
        let mut g = progress().lock().map_err(|_| "busy")?;
        if g.is_some() {
            return Err("A model is already downloading.".into());
        }
        *g = Some(Progress { model: id.to_string(), ..Default::default() });
    }
    if let Ok(mut e) = last_error().lock() {
        e.clear();
    }
    let id = id.to_string();
    tokio::spawn(async move {
        let res = download(&id).await;
        if let Err(e) = res {
            if let Ok(mut le) = last_error().lock() {
                *le = e;
            }
            if let Some(d) = model_dir(&id) {
                let _ = std::fs::remove_dir_all(d);
            }
        }
        if let Ok(mut g) = progress().lock() {
            *g = None;
        }
    });
    Ok(status())
}

async fn download(id: &str) -> Result<(), String> {
    use futures_util::StreamExt;
    use std::io::Write;
    let m = spec(id).ok_or("unknown model")?;
    let dir = model_dir(id).ok_or("no home directory")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    for f in FILES {
        let url = format!("https://huggingface.co/{}/resolve/main/{}", m.repo, f);
        let resp = client.get(&url).send().await.map_err(|e| format!("download {f}: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("download {f}: HTTP {}", resp.status()));
        }
        let len = resp.content_length().unwrap_or(0);
        if let Ok(mut g) = progress().lock() {
            if let Some(p) = g.as_mut() {
                p.total += len;
            }
        }
        let part = dir.join(format!("{f}.part"));
        let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("download {f}: {e}"))?;
            out.write_all(&chunk).map_err(|e| e.to_string())?;
            if let Ok(mut g) = progress().lock() {
                if let Some(p) = g.as_mut() {
                    p.done += chunk.len() as u64;
                }
            }
        }
        out.flush().map_err(|e| e.to_string())?;
        std::fs::rename(&part, dir.join(f)).map_err(|e| e.to_string())?;
    }
    // drop a cached model of the same id so the new files load
    if let Ok(mut c) = cache().lock() {
        *c = None;
    }
    Ok(())
}

pub fn uninstall(id: &str) -> Result<Value, String> {
    spec(id).ok_or("unknown model")?;
    if progress().lock().map(|g| g.as_ref().is_some_and(|p| p.model == id)).unwrap_or(false) {
        return Err("That model is still downloading.".into());
    }
    if let Ok(mut c) = cache().lock() {
        *c = None;
    }
    if let Some(d) = model_dir(id) {
        if d.exists() {
            std::fs::remove_dir_all(&d).map_err(|e| e.to_string())?;
        }
    }
    Ok(status())
}

// ---------- transcription ----------

#[cfg(feature = "whisper")]
type Cached = Option<(String, engine::Loaded)>;
#[cfg(not(feature = "whisper"))]
type Cached = Option<()>;

fn cache() -> &'static Mutex<Cached> {
    static C: OnceLock<Mutex<Cached>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Transcribe an OGG/Opus voice memo with the installed model.
/// `Ok(None)` when no model is installed (or the build lacks the feature).
pub async fn transcribe_ogg(bytes: Vec<u8>) -> Result<Option<String>, String> {
    transcribe_ogg_partial(bytes, |_| {}).await
}

/// Same, calling `on_partial` with the text so far after each 30 s window.
pub async fn transcribe_ogg_partial(bytes: Vec<u8>, on_partial: impl FnMut(&str) + Send + 'static) -> Result<Option<String>, String> {
    #[cfg(feature = "whisper")]
    {
        let Some(id) = installed_model() else { return Ok(None) };
        let dir = model_dir(id).ok_or("no home directory")?;
        let id = id.to_string();
        return tokio::task::spawn_blocking(move || {
            let pcm = engine::decode_ogg_opus_16k(&bytes)?;
            let mut g = cache().lock().map_err(|_| "model lock poisoned")?;
            if g.as_ref().map(|(i, _)| i != &id).unwrap_or(true) {
                *g = Some((id.clone(), engine::Loaded::load(&dir)?));
            }
            let (_, m) = g.as_mut().ok_or("model not loaded")?;
            let mut on_partial = on_partial;
            m.transcribe(&pcm, &mut on_partial).map(Some)
        })
        .await
        .map_err(|e| e.to_string())?;
    }
    #[cfg(not(feature = "whisper"))]
    {
        let _ = (bytes, on_partial);
        Ok(None)
    }
}

/// librosa-style (slaney) mel filterbank, row-major `[n_mels][n_fft/2 + 1]`.
pub fn mel_filters(sr: f64, n_fft: usize, n_mels: usize) -> Vec<f32> {
    fn hz_to_mel(f: f64) -> f64 {
        let (f_sp, min_log_hz) = (200.0 / 3.0, 1000.0);
        let min_log_mel = min_log_hz / f_sp;
        let logstep = (6.4f64).ln() / 27.0;
        if f >= min_log_hz { min_log_mel + (f / min_log_hz).ln() / logstep } else { f / f_sp }
    }
    fn mel_to_hz(m: f64) -> f64 {
        let (f_sp, min_log_hz) = (200.0 / 3.0, 1000.0);
        let min_log_mel = min_log_hz / f_sp;
        let logstep = (6.4f64).ln() / 27.0;
        if m >= min_log_mel { min_log_hz * (logstep * (m - min_log_mel)).exp() } else { f_sp * m }
    }
    let n_freq = n_fft / 2 + 1;
    let fft_freqs: Vec<f64> = (0..n_freq).map(|i| i as f64 * sr / n_fft as f64).collect();
    let (lo, hi) = (hz_to_mel(0.0), hz_to_mel(sr / 2.0));
    let pts: Vec<f64> = (0..n_mels + 2).map(|i| mel_to_hz(lo + (hi - lo) * i as f64 / (n_mels + 1) as f64)).collect();
    let mut w = vec![0f32; n_mels * n_freq];
    for m in 0..n_mels {
        let (l, c, r) = (pts[m], pts[m + 1], pts[m + 2]);
        let enorm = 2.0 / (r - l);
        for (k, &f) in fft_freqs.iter().enumerate() {
            let v = ((f - l) / (c - l)).min((r - f) / (r - c)).max(0.0);
            w[m * n_freq + k] = (v * enorm) as f32;
        }
    }
    w
}

/// GPT-2 byte-level BPE: the printable char each byte is mapped to.
pub fn byte_decoder() -> std::collections::HashMap<char, u8> {
    let mut bs: Vec<u32> = (b'!' as u32..=b'~' as u32).chain(0xA1..=0xAC).chain(0xAE..=0xFF).collect();
    let mut cs = bs.clone();
    let mut n = 0;
    for b in 0..256u32 {
        if !bs.contains(&b) {
            bs.push(b);
            cs.push(256 + n);
            n += 1;
        }
    }
    bs.into_iter().zip(cs).filter_map(|(b, c)| char::from_u32(c).map(|c| (c, b as u8))).collect()
}

#[cfg(feature = "whisper")]
mod engine {
    use std::collections::HashMap;
    use std::path::Path;

    use candle_core::{DType, Device, IndexOp, Tensor};
    use candle_nn::VarBuilder;
    use candle_transformers::models::whisper::{self as w, audio, model::Whisper, Config};

    pub struct Loaded {
        model: Whisper,
        cfg: Config,
        filters: Vec<f32>,
        id_to_tok: HashMap<u32, String>,
        tok_to_id: HashMap<String, u32>,
        bytes: HashMap<char, u8>,
        device: Device,
    }

    impl Loaded {
        pub fn load(dir: &Path) -> Result<Self, String> {
            let device = Device::Cpu;
            let cfg: Config = serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).map_err(|e| e.to_string())?)
                .map_err(|e| format!("whisper config: {e}"))?;
            let tj: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("tokenizer.json")).map_err(|e| e.to_string())?)
                .map_err(|e| format!("whisper tokenizer: {e}"))?;
            let mut tok_to_id = HashMap::new();
            if let Some(v) = tj.pointer("/model/vocab").and_then(|v| v.as_object()) {
                for (k, id) in v {
                    if let Some(id) = id.as_u64() {
                        tok_to_id.insert(k.clone(), id as u32);
                    }
                }
            }
            for a in tj.get("added_tokens").and_then(|a| a.as_array()).into_iter().flatten() {
                if let (Some(c), Some(id)) = (a.get("content").and_then(|c| c.as_str()), a.get("id").and_then(|i| i.as_u64())) {
                    tok_to_id.insert(c.to_string(), id as u32);
                }
            }
            let id_to_tok = tok_to_id.iter().map(|(k, v)| (*v, k.clone())).collect();
            // SAFETY: the file is ours and not modified while mapped (uninstall drops the cache first)
            let vb = unsafe { VarBuilder::from_mmaped_safetensors(&[dir.join("model.safetensors")], w::DTYPE, &device) }
                .map_err(|e| format!("whisper weights: {e}"))?;
            let model = Whisper::load(&vb, cfg.clone()).map_err(|e| format!("whisper load: {e}"))?;
            let filters = super::mel_filters(w::SAMPLE_RATE as f64, w::N_FFT, cfg.num_mel_bins);
            Ok(Self { model, cfg, filters, id_to_tok, tok_to_id, bytes: super::byte_decoder(), device })
        }

        fn id(&self, t: &str) -> Result<u32, String> {
            self.tok_to_id.get(t).copied().ok_or_else(|| format!("tokenizer has no {t}"))
        }

        fn text(&self, ids: &[u32]) -> String {
            let mut bytes = Vec::new();
            for id in ids {
                if let Some(t) = self.id_to_tok.get(id) {
                    if t.starts_with("<|") {
                        continue;
                    }
                    bytes.extend(t.chars().filter_map(|c| self.bytes.get(&c).copied()));
                }
            }
            String::from_utf8_lossy(&bytes).trim().to_string()
        }

        pub fn transcribe(&mut self, pcm: &[f32], on_partial: &mut dyn FnMut(&str)) -> Result<String, String> {
            let e = |e: candle_core::Error| e.to_string();
            let mel = audio::pcm_to_mel(&self.cfg, pcm, &self.filters);
            let n_mels = self.cfg.num_mel_bins;
            let frames = mel.len() / n_mels;
            let mel = Tensor::from_vec(mel, (1, n_mels, frames), &self.device).map_err(e)?;
            let (sot, eot, transcribe, no_ts) = (self.id(w::SOT_TOKEN)?, self.id(w::EOT_TOKEN)?, self.id(w::TRANSCRIBE_TOKEN)?, self.id(w::NO_TIMESTAMPS_TOKEN)?);
            // language tokens: <|en|> … between sot and <|translate|>
            let langs: Vec<u32> = self.id_to_tok.iter()
                .filter(|(_, t)| t.len() <= 7 && t.starts_with("<|") && t.ends_with("|>") && t[2..t.len() - 2].chars().all(|c| c.is_ascii_lowercase()) && t.len() >= 6)
                .map(|(i, _)| *i)
                .filter(|i| *i > sot && *i < transcribe)
                .collect();
            let suppress: Vec<u32> = self.cfg.suppress_tokens.clone();
            let mut out = Vec::new();
            let mut seek = 0;
            // the audio is padded to 30 s per window; stop once real audio ends
            let content_frames = pcm.len() / w::HOP_LENGTH;
            while seek < content_frames {
                let len = usize::min(w::N_FRAMES, frames - seek);
                let seg = mel.narrow(2, seek, len).map_err(e)?;
                let seg = if len < w::N_FRAMES { seg.pad_with_zeros(2, 0, w::N_FRAMES - len).map_err(e)? } else { seg };
                seek += w::N_FRAMES;
                let feats = self.model.encoder.forward(&seg, true).map_err(e)?;
                // detect the language from the first decoder step
                let mut tokens = vec![sot];
                if !langs.is_empty() {
                    let t = Tensor::new(tokens.as_slice(), &self.device).map_err(e)?.unsqueeze(0).map_err(e)?;
                    let ys = self.model.decoder.forward(&t, &feats, true).map_err(e)?;
                    let logits = self.model.decoder.final_linear(&ys.i(..1).map_err(e)?).map_err(e)?.i(0).map_err(e)?.i(0).map_err(e)?;
                    let v: Vec<f32> = logits.to_vec1().map_err(e)?;
                    if let Some(l) = langs.iter().copied().max_by(|a, b| v[*a as usize].total_cmp(&v[*b as usize])) {
                        tokens.push(l);
                    }
                }
                tokens.extend([transcribe, no_ts]);
                let start = tokens.len();
                for i in 0..self.cfg.max_target_positions / 2 {
                    let t = Tensor::new(tokens.as_slice(), &self.device).map_err(e)?.unsqueeze(0).map_err(e)?;
                    let ys = self.model.decoder.forward(&t, &feats, i == 0).map_err(e)?;
                    let (_, seq, _) = ys.dims3().map_err(e)?;
                    let logits = self.model.decoder.final_linear(&ys.i((..1, seq - 1..)).map_err(e)?).map_err(e)?
                        .i(0).map_err(e)?.i(0).map_err(e)?.to_dtype(DType::F32).map_err(e)?;
                    let mut v: Vec<f32> = logits.to_vec1().map_err(e)?;
                    for s in &suppress {
                        if let Some(x) = v.get_mut(*s as usize) {
                            *x = f32::NEG_INFINITY;
                        }
                    }
                    let next = v.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i as u32).unwrap_or(eot);
                    if next == eot || tokens.len() > self.cfg.max_target_positions - 1 {
                        break;
                    }
                    tokens.push(next);
                }
                let seg_text = self.text(&tokens[start..]);
                if !seg_text.is_empty() {
                    out.push(seg_text);
                    if seek < content_frames {
                        on_partial(&out.join(" "));
                    }
                }
            }
            Ok(out.join(" "))
        }
    }

    /// OGG/Opus (Telegram voice memos) → mono 16 kHz f32.
    pub fn decode_ogg_opus_16k(bytes: &[u8]) -> Result<Vec<f32>, String> {
        let mut r = ogg::PacketReader::new(std::io::Cursor::new(bytes));
        let mut channels = 1usize;
        let mut pre_skip = 0usize;
        let mut dec: Option<opus_decoder::OpusDecoder> = None;
        let mut pcm48: Vec<f32> = Vec::new();
        let mut buf = vec![0f32; 5760 * 2];
        let mut idx = 0;
        loop {
            let p = match r.read_packet() {
                Ok(Some(p)) => p,
                Ok(None) => break,
                // a cut-off last page: keep what decoded so far
                Err(_) if !pcm48.is_empty() => break,
                Err(e) => return Err(format!("ogg: {e}")),
            };
            let data = &p.data;
            if idx == 0 {
                if !data.starts_with(b"OpusHead") || data.len() < 19 {
                    return Err("not an Opus voice memo".into());
                }
                channels = (data[9] as usize).clamp(1, 2);
                pre_skip = u16::from_le_bytes([data[10], data[11]]) as usize;
                dec = Some(opus_decoder::OpusDecoder::new(48_000, channels).map_err(|e| format!("opus: {e:?}"))?);
            } else if idx > 1 {
                let d = dec.as_mut().ok_or("opus: no header")?;
                let n = d.decode_float(data, &mut buf, false).map_err(|e| format!("opus: {e:?}"))?;
                for f in 0..n {
                    let s: f32 = (0..channels).map(|c| buf[f * channels + c]).sum::<f32>() / channels as f32;
                    pcm48.push(s);
                }
            }
            idx += 1;
        }
        let pcm48 = &pcm48[pre_skip.min(pcm48.len())..];
        Ok(super::downsample_48k_to_16k(pcm48))
    }
}

/// 48 kHz → 16 kHz: a short low-pass (averaging) then keep every third sample.
pub fn downsample_48k_to_16k(x: &[f32]) -> Vec<f32> {
    x.chunks(3).map(|c| c.iter().sum::<f32>() / c.len() as f32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meminfo_and_filters() {
        assert_eq!(parse_meminfo_total("MemTotal:       16384000 kB\nMemFree: 1 kB"), Some(16000));
        let f = mel_filters(16000.0, 400, 80);
        assert_eq!(f.len(), 80 * 201);
        assert!(f.iter().all(|v| v.is_finite() && *v >= 0.0));
        // every mel band has some weight
        assert!((0..80).all(|m| f[m * 201..(m + 1) * 201].iter().any(|v| *v > 0.0)));
        let bd = byte_decoder();
        assert_eq!(bd.len(), 256);
        assert_eq!(bd[&'Ġ'], b' ');
        assert_eq!(downsample_48k_to_16k(&[1.0; 9]).len(), 3);
    }

    /// Opt-in: `VICUNA_MODELS_DIR=<dir with whisper-tiny> VICUNA_TEST_OPUS=<file.opus>
    /// cargo test --release --no-default-features --features rustls,whisper --lib live_transcribe -- --ignored --nocapture`
    #[cfg(feature = "whisper")]
    #[tokio::test]
    #[ignore]
    async fn live_transcribe() {
        let f = std::env::var("VICUNA_TEST_OPUS").expect("VICUNA_TEST_OPUS");
        let t0 = std::time::Instant::now();
        let out = transcribe_ogg(std::fs::read(f).unwrap()).await.unwrap().expect("model installed");
        println!("[{:?}] {out}", t0.elapsed());
        assert!(!out.trim().is_empty());
    }
}
