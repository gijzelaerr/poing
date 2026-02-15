use nih_plug::prelude::*;
use poing_core::audio_buffer::RingBuffer;
use poing_core::SharedState;
use std::sync::atomic::Ordering;
use std::sync::Arc;

struct Poing {
    params: Arc<PoingParams>,
    shared_state: SharedState,
    ring_buffer: RingBuffer,
}

#[derive(Params)]
struct PoingParams {}

impl Default for Poing {
    fn default() -> Self {
        // 30 seconds at 48kHz mono
        let max_recording_samples = 48_000 * 30;
        Self {
            params: Arc::new(PoingParams::default()),
            shared_state: SharedState::new(),
            ring_buffer: RingBuffer::new(max_recording_samples),
        }
    }
}

impl Default for PoingParams {
    fn default() -> Self {
        Self {}
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
        true
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
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

        // Audio passes through unmodified
        ProcessStatus::Normal
    }

    fn editor(&mut self, _async_executor: AsyncExecutor<Self>) -> Option<Box<dyn Editor>> {
        poing_makepad_bridge::create_editor(self.shared_state.clone(), (800, 600))
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
