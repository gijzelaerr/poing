use nih_plug::prelude::*;
use nih_plug_vizia::ViziaState;
use poing_core::audio_buffer::RingBuffer;
use poing_core::SharedState;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

pub struct Poing {
    params: Arc<PoingParams>,
    shared_state: SharedState,
    ring_buffer: RingBuffer,
}

#[derive(Params)]
struct PoingParams {
    #[persist = "model-path"]
    pub selected_model_path: Arc<Mutex<Option<String>>>,

    #[persist = "editor-state"]
    pub editor_state: Arc<ViziaState>,
}

impl Default for Poing {
    fn default() -> Self {
        // 30 seconds at 48kHz mono
        let max_recording_samples = 48_000 * 30;
        let shared_state = SharedState::new();

        // Initialize persist field from SharedState's loaded config
        let initial_path = shared_state
            .model_path
            .lock()
            .unwrap()
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());

        Self {
            params: Arc::new(PoingParams {
                selected_model_path: Arc::new(Mutex::new(initial_path)),
                editor_state: poing_editor::default_state(),
            }),
            shared_state,
            ring_buffer: RingBuffer::new(max_recording_samples),
        }
    }
}

impl Plugin for Poing {
    const NAME: &'static str = "Poing";
    const VENDOR: &'static str = "Poing";
    const URL: &'static str = "";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    const MIDI_INPUT: MidiConfig = MidiConfig::None;
    const MIDI_OUTPUT: MidiConfig = MidiConfig::None;

    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn initialize(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        buffer_config: &BufferConfig,
        _context: &mut impl InitContext<Self>,
    ) -> bool {
        if let Ok(mut sr) = self.shared_state.sample_rate.lock() {
            *sr = buffer_config.sample_rate;
        }

        // Sync persisted model path -> SharedState (DAW project reload)
        if let Ok(persisted) = self.params.selected_model_path.lock() {
            if let Some(path_str) = persisted.as_ref() {
                let path = PathBuf::from(path_str);
                *self.shared_state.model_path.lock().unwrap() = Some(path);
            }
        }

        true
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        // Sync host transport info to shared state for the GUI
        let transport = context.transport();
        if let Some(tempo) = transport.tempo {
            if let Ok(mut t) = self.shared_state.host_tempo.try_lock() {
                *t = Some(tempo);
            }
        }
        if let (Some(num), Some(den)) =
            (transport.time_sig_numerator, transport.time_sig_denominator)
        {
            if let Ok(mut ts) = self.shared_state.host_time_sig.try_lock() {
                *ts = Some((num, den));
            }
        }

        // Copy input to ring buffer when recording is armed
        if self.shared_state.is_recording.load(Ordering::Relaxed) {
            for sample_frame in buffer.iter_samples() {
                // Record the first (left) channel as mono
                if let Some(sample) = sample_frame.into_iter().next() {
                    self.ring_buffer.write(&[*sample]);
                }
            }

            // Snapshot recorded audio into shared state (for GUI waveform display)
            if let Ok(mut recorded) = self.shared_state.recorded_audio.try_lock() {
                *recorded = self.ring_buffer.read();
            }
        }

        // Sync SharedState model_path -> persist field for DAW project save
        if let Ok(current) = self.shared_state.model_path.try_lock() {
            if let Ok(mut persisted) = self.params.selected_model_path.try_lock() {
                let current_str = current.as_ref().map(|p| p.to_string_lossy().into_owned());
                if *persisted != current_str {
                    *persisted = current_str;
                }
            }
        }

        // Audio passes through unmodified
        ProcessStatus::Normal
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        poing_editor::create(
            self.shared_state.clone(),
            self.params.editor_state.clone(),
        )
    }
}

impl ClapPlugin for Poing {
    const CLAP_ID: &'static str = "com.poing.poing";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Neural network audio generation plugin");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Synthesizer,
        ClapFeature::Stereo,
    ];
}

impl Vst3Plugin for Poing {
    const VST3_CLASS_ID: [u8; 16] = *b"PoingNNAudioPlug";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] = &[
        Vst3SubCategory::Fx,
        Vst3SubCategory::Generator,
    ];
}

nih_export_clap!(Poing);
nih_export_vst3!(Poing);
