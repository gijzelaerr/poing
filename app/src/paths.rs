//! Where Poing's pieces are, in one place. In order:
//!
//! 1. development overrides: `POING_SIDECAR`, `POING_ACESTEP`, `POING_UV`
//!    (and `POING_YUE2_*` / the Hugging Face cache in audio.rs);
//! 2. the installed resources folder beside poing.exe: the sidecar scripts,
//!    their lockfiles and a pinned `uv`;
//! 3. the model manager's packs (models.rs): Python envs and weights.
//!
//! A build run from the repository (no `sidecar/` beside the exe) also finds
//! the repository's `sidecar/` and an `ACE-Step-1.5` checkout next to it, as
//! before; an installed Poing never looks there.

use crate::models;
use std::path::{Path, PathBuf};
use std::process::Command;

fn exe_dir() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.parent()?.to_path_buf())
}

/// The installed resources: `sidecar/` beside the exe (the installer puts it there).
fn resources_sidecar() -> Option<PathBuf> {
    exe_dir().map(|d| d.join("sidecar")).filter(|d| d.join("poing_analyze.py").is_file())
}

/// This is an installed Poing, not a build in the repository.
pub fn installed() -> bool {
    resources_sidecar().is_some()
}

/// The repository this build came from, when it runs from there.
fn repo() -> Option<PathBuf> {
    if installed() {
        return None;
    }
    Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("..")).filter(|r| r.join("sidecar").is_dir())
}

/// The sidecar scripts: `POING_SIDECAR`, the installed resources, else the repository's.
pub fn sidecar_dir() -> PathBuf {
    std::env::var_os("POING_SIDECAR")
        .map(PathBuf::from)
        .or_else(resources_sidecar)
        .or_else(|| repo().map(|r| r.join("sidecar")))
        .unwrap_or_else(|| PathBuf::from("sidecar"))
}

/// `uv`: `POING_UV`, the one shipped beside the exe, else on PATH.
pub fn uv() -> PathBuf {
    if let Some(p) = std::env::var_os("POING_UV") {
        return PathBuf::from(p);
    }
    let name = if cfg!(windows) { "uv.exe" } else { "uv" };
    exe_dir().map(|d| d.join(name)).filter(|p| p.is_file()).unwrap_or_else(|| PathBuf::from("uv"))
}

/// A development ACE-Step checkout (a uv project with its own `checkpoints/`):
/// `POING_ACESTEP`, else `ACE-Step-1.5` next to the repository.
fn dev_acestep() -> Option<PathBuf> {
    std::env::var_os("POING_ACESTEP")
        .map(PathBuf::from)
        .or_else(|| repo().map(|r| r.join("..").join("ACE-Step-1.5")).filter(|d| d.join("pyproject.toml").is_file()))
}

/// The analysis sidecar runs as a development uv project (not from a pack).
fn dev_sidecar() -> bool {
    std::env::var_os("POING_SIDECAR").is_some() || repo().is_some()
}

/// A feature works here without the model manager (a development setup).
pub fn dev_ready(feature: &str) -> bool {
    match feature {
        "analysis" => dev_sidecar(),
        "repaint" | "remix" | "new-song" => dev_acestep().is_some(),
        _ => false,
    }
}

/// The Python a script runs in.
#[derive(Clone, Copy)]
pub enum Runtime {
    /// `sidecar/`: Demucs, SheetSage2, Whisper.
    Analysis,
    /// `sidecar/dereverb/` (development only for now).
    Dereverb,
    /// ACE-Step 1.5.
    AceStep,
}

/// A command that runs `python` in `rt`, ready for the script and its
/// arguments: `uv run --project …` in development, the pack's env when installed.
pub fn python(rt: Runtime) -> Result<Command, String> {
    let uv_run = |project: &Path| {
        let mut cmd = Command::new(uv());
        cmd.arg("run").arg("--project").arg(project).arg("python");
        cmd
    };
    match rt {
        Runtime::Analysis if dev_sidecar() => Ok(uv_run(&sidecar_dir())),
        Runtime::Analysis => {
            let pack = models::installed_pack("py-analysis").ok_or("understanding songs needs a download (Settings → Models)")?;
            let mut cmd = Command::new(venv_python(&pack));
            // The scripts fetch their own weights (torch hub, Hugging Face) on first
            // use: into the pack, so removing it frees them too.
            cmd.env("HF_HOME", pack.join("hf")).env("TORCH_HOME", pack.join("torch"));
            Ok(cmd)
        }
        Runtime::Dereverb if dev_sidecar() => Ok(uv_run(&sidecar_dir().join("dereverb"))),
        Runtime::Dereverb => Err("Less echo can't be installed yet; it runs only in a development setup".into()),
        Runtime::AceStep => {
            if let Some(ace) = dev_acestep() {
                let mut cmd = uv_run(&ace);
                cmd.current_dir(&ace).env("POING_ACESTEP", &ace);
                return Ok(cmd);
            }
            let runtime = models::installed_pack("py-acestep").ok_or("ACE-Step needs a download (Settings → Models)")?;
            let weights = models::installed_pack("acestep-turbo").ok_or("ACE-Step needs a download (Settings → Models)")?;
            let src = runtime.join("src");
            let mut cmd = Command::new(venv_python(&runtime));
            cmd.current_dir(&src)
                .env("POING_ACESTEP", &src)
                .env("ACESTEP_CHECKPOINTS_DIR", &weights)
                .env("POING_NO_AUTO_DOWNLOAD", "1");
            if let Some(lm) = models::installed_pack("acestep-lm") {
                cmd.env("POING_ACESTEP_LM_DIR", lm);
            }
            Ok(cmd)
        }
    }
}

/// The Python inside a pack's env.
pub fn venv_python(pack: &Path) -> PathBuf {
    if cfg!(windows) {
        pack.join("venv").join("Scripts").join("python.exe")
    } else {
        pack.join("venv").join("bin").join("python")
    }
}
