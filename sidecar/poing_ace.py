"""What the ACE-Step scripts share: where ACE-Step and its weights are.

Two setups:
- development: `uv run --project <ACE-Step-1.5 checkout>`, weights in its
  `checkpoints/` (POING_ACESTEP, else the working directory);
- installed: Poing's model manager made the env and put the weights in
  per-pack folders, named by ACESTEP_CHECKPOINTS_DIR (DiT, VAE, text encoder)
  and POING_ACESTEP_LM_DIR (the song planner). POING_NO_AUTO_DOWNLOAD is set
  then: ACE-Step must not fetch weights behind the manager's back (it would
  pull the whole 10 GB repository when one part is missing).
"""
import os
import sys
from pathlib import Path


def root():
    """The ACE-Step source tree, put first on sys.path."""
    r = os.environ.get("POING_ACESTEP") or os.getcwd()
    sys.path.insert(0, r)
    if os.environ.get("POING_NO_AUTO_DOWNLOAD"):
        _no_auto_download()
    return r


def lm_dir(r):
    """Where the 5 Hz song planner lives."""
    return os.environ.get("POING_ACESTEP_LM_DIR") or os.path.join(r, "checkpoints")


def _no_auto_download():
    from acestep.handler import AceStepHandler
    from acestep.model_downloader import _contains_model_weights

    def ensure(self, *, checkpoint_path, config_path, **_):
        need = [config_path or "acestep-v15-turbo", "vae", "Qwen3-Embedding-0.6B"]
        missing = [n for n in need if not _contains_model_weights(Path(checkpoint_path) / n)]
        if missing:
            return f"ERROR: {', '.join(missing)} not installed (see Models in Poing)", False
        return None

    AceStepHandler._ensure_models_present = ensure
