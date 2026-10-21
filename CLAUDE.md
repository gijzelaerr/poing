# Poing: agent notes

AI-native music workstation on Makepad 2.0. **Read [docs/PLAN.md](docs/PLAN.md) first:** concepts (Song → Sections → Stems → Takes), principles, workflow, architecture and milestones.

## Ground rules

- **Fewer controls, not more.** Selection drives the one inspector, there's one Describe field, nothing is destructive (everything is a take), and there are no modal dialogs. Check new UI against PLAN.md §2 before adding a button.
- **Audio thread:** no locks, no allocation, no syscalls. Bounded channels in, snapshots out. `Arc` payloads whose last drop happens off the audio thread. The device clock is the transport master.
- **Makepad:** a pinned git dependency on the `work` branch. Upstream rewrites history, so every pin is tagged `poing-pin-YYYYMMDD` in `gijzelaerr/makepad`. Fixes go upstream as small PRs to `makepad/makepad:work`. Follow Makepad's `AGENTS.md` for Splash/widget conventions.
- **Model weights and reference audio never go in git.** `xtask` downloads them, with licence notices.
- **Verify in the real app**, not just builds: run with `--remote` (Makepad remote control) and `MAKEPAD_HIDE_WINDOWS=1` for agent runs, and set `POING_MUTE=1` so test instances make no sound.
- **Every milestone has an acceptance test** from the two reference songs (PLAN.md §6). A milestone is done when its test passes.

## Context

Earlier hands-on work that this plan comes from (pipelines, measurements, model notes) is in `gijzelaerr/claude-space` under `projects/music-gen/`. The prototype app is `apps/daw` on branch `daw` of `gijzelaerr/makepad`.
