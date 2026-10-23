//! Where the native analysis models come from: Makepad's model registry and
//! its resumable, hash-checked downloader (`makepad_ai_hub::local`), in the
//! shared `~/.makepad/weights` folder (`MAKEPAD_ASSET_AI_CACHE` moves it).
//! Each model can also be pointed at a file directly with an env var.
//!
//! Two files are pinned here rather than in Makepad's registry: Whisper
//! large-v3 (the registry has only turbo, which gets three times the lyric
//! errors on sung vocals), and a Hugging Face mirror of the Beat This!
//! checkpoint for when its home server is down. Both carry the size and
//! SHA-256 the downloader checks. large-v3 is then converted to Q8_0 here
//! (`stt/ggml-large-v3-q8_0.bin`, same lyrics on the test songs in about
//! half the time) and the f16 download is removed.

use crate::emit;
use makepad_ai_hub::backend::CancelToken;
use makepad_ai_hub::download::{content_is_verified, DownloadProgress, Downloader};
use makepad_ai_hub::local::{GraphDevice, InstallMsg, InstallState, LocalModels};
use makepad_ai_hub::registry::FileSpec;
use makepad_micro_serde::*;
use std::path::{Path, PathBuf};
use std::time::Duration;

const STEMS_ID: &str = "bs-roformer-4stem";
const BEATS_ID: &str = "beat-this";
const TURBO_ID: &str = "whisper-large-v3-turbo";
const TURBO_FILE: &str = "ggml-large-v3-turbo.bin";
const WHISPER_Q8: [&str; 2] = ["stt", "ggml-large-v3-q8_0.bin"];

/// Whisper large-v3 in ggml form, at the whisper.cpp commit the registry pins for turbo.
fn whisper_v3() -> FileSpec {
    pinned(
        "ggerganov/whisper.cpp",
        "ggml-large-v3.bin",
        "5359861c739e955e79d9a303bcbc70fb988958b1",
        "stt/ggml-large-v3.bin",
        3_095_033_483,
        "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2",
    )
}
const WHISPER_V3_LICENCE: &str = "Whisper large-v3: Apache-2.0 (OpenAI); ggml conversion MIT (whisper.cpp)";

/// The Beat This! checkpoint mirrored on Hugging Face (MIT): the same bytes,
/// and the same cache path, as the registry's file.
fn beats_mirror() -> FileSpec {
    pinned(
        "teamup-tech/Beat-This-weights",
        "final0.ckpt",
        "4afbe376535aaab71b76ae2ca2d7c16b08877321",
        "beats/beat_this_final0.ckpt",
        makepad_ai_beats::MODEL_BYTES,
        makepad_ai_beats::MODEL_SHA256,
    )
}

fn pinned(repo: &str, path: &str, revision: &str, cache_as: &str, size: u64, sha256: &str) -> FileSpec {
    FileSpec {
        role: None,
        repo: repo.into(),
        path: path.into(),
        revision: Some(revision.into()),
        cache_as: cache_as.into(),
        size: Some(size),
        sha256: Some(sha256.into()),
        local: false,
        optional: false,
        converts_to: None,
        conversion: None,
    }
}

pub struct AnalysisPaths {
    pub stems: PathBuf,
    pub beats: PathBuf,
    pub whisper: PathBuf,
}

pub struct Models {
    local: LocalModels,
    weights: PathBuf,
}

#[derive(SerJson)]
struct Status {
    device: String,
    /// Where Whisper runs ("cuda", "metal", "cpu"): "cpu" on Windows and
    /// Linux means this exe was built without CUDA kernels.
    whisper_backend: String,
    weights: String,
    stems: Option<String>,
    beats: Option<String>,
    lyrics: Option<String>,
    licences: Vec<String>,
}

fn env_path(var: &str) -> Option<Option<PathBuf>> {
    let p = std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)?;
    Some(p.is_file().then_some(p))
}

impl Models {
    pub fn open() -> Result<Models, String> {
        let local = LocalModels::open().map_err(|e| format!("model store: {e}"))?;
        // The folder LocalModels::open uses.
        let weights = std::env::var_os("MAKEPAD_ASSET_AI_CACHE").map(PathBuf::from).unwrap_or_else(makepad_ai_hub::home::weights_dir);
        Ok(Models { local, weights })
    }

    fn stems(&self) -> Option<PathBuf> {
        env_path("POING_STEMS_MODEL").unwrap_or_else(|| self.local.installed_path(STEMS_ID, "weights"))
    }

    fn beats(&self) -> Option<PathBuf> {
        env_path("POING_BEATS_MODEL").unwrap_or_else(|| self.local.installed_path(BEATS_ID, "weights"))
    }

    fn whisper_v3(&self) -> Option<PathBuf> {
        let v3 = whisper_v3();
        let dest = v3.dest_path(&self.weights);
        content_is_verified(&dest, v3.sha256.as_deref().unwrap_or_default(), v3.size.unwrap_or_default()).then_some(dest)
    }

    /// Written by `quantize_q8` with an atomic rename, so it is whole when it is there.
    fn whisper_q8(&self) -> Option<PathBuf> {
        let path = self.weights.join(WHISPER_Q8[0]).join(WHISPER_Q8[1]);
        path.is_file().then_some(path)
    }

    /// large-v3 (Q8_0, else f16), else turbo when that is what is installed.
    fn whisper(&self) -> Option<PathBuf> {
        env_path("POING_WHISPER_MODEL").unwrap_or_else(|| {
            self.whisper_q8()
                .or_else(|| self.whisper_v3())
                .or_else(|| self.local.installed_dir(TURBO_ID).map(|d| d.join(TURBO_FILE)).filter(|p| p.is_file()))
        })
    }

    pub fn analysis_paths(&self) -> Result<AnalysisPaths, String> {
        let (stems, beats, whisper) = (self.stems(), self.beats(), self.whisper());
        let missing: Vec<&str> = [(&stems, "stems"), (&beats, "beats"), (&whisper, "lyrics")]
            .iter()
            .filter(|(p, _)| p.is_none())
            .map(|(_, task)| *task)
            .collect();
        match (stems, beats, whisper) {
            (Some(stems), Some(beats), Some(whisper)) => Ok(AnalysisPaths { stems, beats, whisper }),
            _ => Err(format!("native models not installed for {} (run `poing-engine install`)", missing.join(", "))),
        }
    }

    pub fn status_json(&self) -> String {
        let show = |p: Option<PathBuf>| p.map(|p| p.display().to_string());
        let licence = |id: &str| {
            self.local.license(id).map_or(String::new(), |l| {
                let accepted = if self.local.license_acknowledged(id) { "accepted" } else { "not accepted" };
                format!("{id}: {} ({:?}, {accepted})", l.name, l.restriction)
            })
        };
        let device = match self.local.device() {
            GraphDevice::Cuda => "cuda",
            GraphDevice::Metal => "metal",
        };
        Status {
            device: device.into(),
            whisper_backend: makepad_ai_speech::whisper::accel_backend_name().into(),
            weights: self.weights.display().to_string(),
            stems: show(self.stems()),
            beats: show(self.beats()),
            lyrics: show(self.whisper()),
            licences: vec![licence(STEMS_ID), licence(BEATS_ID), WHISPER_V3_LICENCE.into()],
        }
        .serialize_json()
    }

    /// Download whatever analysis model is missing. The registry models'
    /// licences must be accepted first: the BS-RoFormer checkpoint is for
    /// academic use only.
    pub fn install(&mut self, accept: bool) -> Result<(), String> {
        let mut todo = Vec::new();
        if self.stems().is_none() {
            todo.push(STEMS_ID);
        }
        if self.beats().is_none() {
            todo.push(BEATS_ID);
        }
        for id in &todo {
            if self.local.license_acknowledged(id) {
                continue;
            }
            let licence = self.local.license(id).ok_or_else(|| format!("{id}: not in the model registry"))?;
            eprintln!("{id}: {} ({:?})\n  {}\n  {}", licence.name, licence.restriction, licence.summary, licence.url);
            if !accept {
                return Err(format!("{id} needs its licence accepted: read it above, then run `poing-engine install --accept-licence`"));
            }
            self.local.acknowledge_license(id).map_err(|e| format!("{id}: {e}"))?;
        }
        let mut errors = Vec::new();
        for id in todo {
            if let Err(e) = self.install_registry(id) {
                // Beat This!'s home server goes down; its mirror has the same bytes.
                let mirrored = id == BEATS_ID && self.download(&beats_mirror()).is_ok();
                if !mirrored {
                    emit("install", 0.0, &format!("{id} failed: {e}"));
                    errors.push(format!("{id}: {e}"));
                }
            }
        }
        if self.whisper_q8().is_none() {
            eprintln!("{WHISPER_V3_LICENCE}");
            let q8 = self.weights.join(WHISPER_Q8[0]).join(WHISPER_Q8[1]);
            let made = self.download(&whisper_v3()).and_then(|f16| {
                emit("install", 1.0, "Converting Whisper to 8-bit");
                quantize_q8(&f16, &q8)?;
                let _ = std::fs::remove_file(&f16);
                let _ = std::fs::remove_file(makepad_ai_hub::download::verification_path(&f16));
                Ok(())
            });
            if let Err(e) = made {
                errors.push(format!("whisper large-v3: {e}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            // One model's host being down shouldn't stop the others; run again to resume.
            Err(errors.join("; "))
        }
    }

    fn install_registry(&self, id: &str) -> Result<(), String> {
        if matches!(self.local.install_state(id), InstallState::Installed) {
            return Ok(());
        }
        emit("install", 0.0, &format!("Downloading {id}"));
        let handle = self.local.start_install(id).map_err(|e| e.to_string())?;
        let mut failed = None;
        'wait: loop {
            for msg in handle.poll() {
                match msg {
                    InstallMsg::Progress { file, done, total } if total > 0 => download_progress(&file, done, total),
                    InstallMsg::Failed(e) => failed = Some(e),
                    InstallMsg::Cancelled => return Err("download cancelled".into()),
                    InstallMsg::Finished => break 'wait,
                    _ => {}
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        match failed {
            Some(e) if !matches!(self.local.install_state(id), InstallState::Installed) => Err(e),
            _ => Ok(()),
        }
    }

    /// One pinned file, resumable and hash-checked, into the weights folder.
    fn download(&self, file: &FileSpec) -> Result<PathBuf, String> {
        emit("install", 0.0, &format!("Downloading {} from {}", file.path, file.repo));
        let downloader = Downloader::from_env().map_err(|e| e.to_string())?;
        downloader
            .ensure_file(file, &self.weights, &mut |p: DownloadProgress| download_progress(&p.file, p.done, p.total.unwrap_or(0)), &CancelToken::new())
            .map_err(|e| e.to_string())
    }
}

fn download_progress(file: &str, done: u64, total: u64) {
    if total > 0 {
        emit("install", done as f64 / total as f64, &format!("{file}: {:.0} / {:.0} MB", done as f64 / 1e6, total as f64 / 1e6));
    }
}

/// Whisper's weights as Q8_0, which the CUDA backend keeps 8-bit on the
/// device: a quarter of the f32 copy it makes of an f16 file.
pub fn quantize_q8(input: &Path, output: &Path) -> Result<(), String> {
    use makepad_ai_speech::whisper::{quantize_ggml, WeightQuantization};
    let part = output.with_extension("part");
    let mut reader = std::io::BufReader::with_capacity(1 << 20, std::fs::File::open(input).map_err(|e| format!("{}: {e}", input.display()))?);
    let file = std::fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut writer = std::io::BufWriter::with_capacity(1 << 20, file);
    quantize_ggml(&mut reader, &mut writer, WeightQuantization::Q8_0).map_err(|e| format!("quantize {}: {e}", input.display()))?;
    writer.into_inner().map_err(|e| e.to_string())?.sync_all().map_err(|e| e.to_string())?;
    std::fs::rename(&part, output).map_err(|e| format!("{}: {e}", output.display()))
}
