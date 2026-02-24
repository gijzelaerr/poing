# Poing - Claude Code Notes

## Project Structure

Poing is a neural audio generation plugin (VST3/CLAP) built with nih-plug.

```
poing-core/       - Core engine: SharedState, musicgen inference, audio_buffer, wav I/O, config
poing-editor/     - VIZIA-based plugin GUI (replaced Makepad in Feb 2025)
poing-plugin/     - nih-plug plugin entry point (cdylib for VST3/CLAP, bin for standalone)
xtask/            - Build automation (cargo xtask bundle poing-plugin --release)
```

## Build Commands

```sh
# Check everything compiles
cargo check --workspace

# Build plugin bundles (CLAP + VST3)
cargo xtask bundle poing-plugin --release

# Build standalone (for GUI testing without a DAW)
cargo build -p poing-plugin --no-default-features --features standalone --bin poing-standalone
```

## Architecture

### SharedState
`poing_core::SharedState` is the central cross-thread communication struct. It holds `Arc<Mutex<T>>` fields for prompt, model_path, generation_state, progress, generated_audio, recorded_audio, etc. It is shared between the audio thread, GUI, and inference thread.

### Editor (poing-editor)
Uses `nih_plug_vizia` with `create_vizia_editor()`. The editor has:
- `model.rs` - `PoingModel` with `#[derive(Lens)]` for reactive UI binding. `PoingEvent` enum handles all user actions. A 30Hz timer polls SharedState for updates.
- `waveform.rs` - Custom `WaveformView` implementing vizia's `View` trait with femtovg drawing (1024-column min/max waveform). Supports mouse drag to initiate file drag.
- `drag_source.rs` - Platform-specific OS file drag initiation. macOS implemented via objc/cocoa. Windows and Linux are stubs (TODO).
- `theme.css` - Dark theme included via `include_str!`.

### Plugin (poing-plugin)
- `lib.rs` - `Poing` struct implementing `Plugin`. Has `#[persist = "editor-state"]` for ViziaState and `#[persist = "model-path"]` for selected model path. Audio passes through unmodified; recording copies input to a ring buffer.
- `main.rs` - Standalone binary using `nih_export_standalone`. Requires `--features standalone`.

## Key Dependencies

- `nih_plug` / `nih_plug_vizia` - from `https://github.com/robbert-vdh/nih-plug.git` (commit 28b149e)
- vizia - from `https://github.com/robbert-vdh/vizia.git` tag `patched-2024-05-06` (robbert-vdh's fork with baseview patches)
- baseview - Patched local copy at `lib/baseview` (git submodule from `RustAudio/baseview` at `237d323c`), overriding both nih_plug revisions via `[patch]` in workspace `Cargo.toml`. Includes fixes for macOS null pointer crash and re-entrancy, plus additional deferrable event fixes for modal dialog compatibility.
- `poing-core` depends on `ort` (ONNX Runtime), `ndarray`, `hound`, `tokenizers`

## Known Issues

### Standalone mode on macOS
Standalone mode works with `-p 4096` or larger period size. Core Audio may deliver more samples than the default period size (e.g., 558 vs 512), causing the cpal backend to panic. Use:
```sh
./target/debug/poing-standalone -p 4096
```
The GUI can also be tested by loading the CLAP/VST3 bundle in a DAW (e.g., REAPER).

### Drag-and-drop
- macOS: Implemented via objc/cocoa NSPasteboardItem + beginDraggingSession
- Windows: Stub (needs COM IDataObject with CF_HDROP + DoDragDrop)
- Linux: Stub (needs XDND protocol implementation)

### assert_process_allocs
The `assert_process_allocs` feature is behind a cargo feature flag (enabled by default). It catches allocations in the audio `process()` callback. The ring buffer `read()` method allocates a Vec, which triggers this assertion. Disable with `--no-default-features` when running standalone.

## VIZIA Reference

The vizia fork used is checked out at `~/Github/robbert-vdh/vizia` (tag `patched-2024-05-06`).

Key VIZIA patterns used:
- `#[derive(Lens)]` on model struct for reactive bindings
- `impl Model` with `fn event(&mut self, cx: &mut EventContext, event: &mut Event)` for event handling
- `cx.add_timer()` / `cx.start_timer()` for polling (33ms interval = ~30Hz)
- `Dropdown::new(cx, |cx| label, |cx| content)` with `PopupEvent::Close` to close
- Custom `View` with `fn draw(&self, cx: &mut DrawContext, canvas: &mut Canvas)` using femtovg via `nih_plug_vizia::vizia::vg::{Paint, Path, Color}`
- Mouse events via `WindowEvent::MouseDown/MouseUp/MouseMove` in `fn event()`
- `cx.mouse()` returns `&MouseState<Entity>` (method, not field)

## CI

GitHub Actions builds on macOS, Windows, and Linux. No Makepad clone step needed (removed during VIZIA migration). Linux needs: libx11-dev, libx11-xcb-dev, libxcursor-dev, libxrandr-dev, libxi-dev, libgl1-mesa-dev, libegl1-mesa-dev, libwayland-dev, libxkbcommon-dev, libasound2-dev, libpulse-dev.
