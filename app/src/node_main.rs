//! poing-node: Poing's GPU jobs served to a Poing on another machine (see
//! node.rs). It runs the app's own job code: the modules below are the
//! app's, compiled again without its UI.

#![allow(dead_code)]

pub use makepad_widgets;

#[path = "audio.rs"]
mod audio;
#[path = "deckmap.rs"]
mod deckmap;
#[path = "decks.rs"]
mod decks;
#[path = "djexport.rs"]
mod djexport;
#[path = "export.rs"]
mod export;
#[path = "fx.rs"]
mod fx;
#[path = "fx_writer.rs"]
mod fx_writer;
#[path = "live.rs"]
mod live;
#[path = "llm.rs"]
mod llm;
#[path = "midi_writer.rs"]
mod midi_writer;
/// The Models page is UI (it needs the App); state.rs only names its Resume.
mod model_view {
    pub enum Resume {}
}
#[path = "models.rs"]
mod models;
#[path = "node.rs"]
mod node;
#[path = "paths.rs"]
mod paths;
#[path = "plugins.rs"]
mod plugins;
#[path = "remote.rs"]
mod remote;
#[path = "song_writer.rs"]
mod song_writer;
#[path = "sounds.rs"]
mod sounds;
#[path = "state.rs"]
mod state;
#[path = "synth.rs"]
mod synth;
#[path = "wire.rs"]
mod wire;
pub(crate) use poing_model as model;

fn main() {
    node::main()
}
