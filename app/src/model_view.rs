//! The model manager in the UI: the offer bar ("Repainting needs a download …
//! Download · Not now", then its progress) and Settings → Models (each pack,
//! its size, Download or Remove). No dialogs: the bar sits under the top bar.

use crate::models::{self, Readiness, Update};
use crate::state::EditorState;
use crate::{analyse, audio, describe, paths, same_song, start_remix, App};
use makepad_widgets::*;
use std::path::PathBuf;

/// What to do once a download is in.
#[derive(Clone)]
pub enum Resume {
    /// Split this song.
    Analyse(PathBuf),
    /// Run this Describe on this song again.
    Describe(PathBuf, String),
    /// Make the remix this song waits for.
    Remix(PathBuf),
    /// Make a new song from this prompt.
    MakeSong(String),
    /// Hand this free-form request to the assistant (it waited for its model).
    Ask(PathBuf, String),
}

pub struct Offer {
    feature: &'static str,
    resume: Resume,
}

/// None when `feature` can run now. Otherwise the line to show, and the
/// download is offered (once the action returns) when there is one to make.
pub fn gate(st: &mut EditorState, feature: &'static str, resume: Resume) -> Option<String> {
    match models::readiness(feature) {
        Readiness::Ready => None,
        Readiness::CantRun(why) => Some(why),
        Readiness::Needs(..) => {
            st.needs_download = Some((feature, resume));
            Some("That needs a download first: see the bar at the top.".into())
        }
    }
}

const ROWS: usize = 6;

fn row_ids(i: usize) -> (&'static [LiveId], &'static [LiveId], &'static [LiveId], &'static [LiveId]) {
    match i {
        0 => (ids!(mp0), ids!(mp0_name), ids!(mp0_info), ids!(mp0_btn)),
        1 => (ids!(mp1), ids!(mp1_name), ids!(mp1_info), ids!(mp1_btn)),
        2 => (ids!(mp2), ids!(mp2_name), ids!(mp2_info), ids!(mp2_btn)),
        3 => (ids!(mp3), ids!(mp3_name), ids!(mp3_info), ids!(mp3_btn)),
        4 => (ids!(mp4), ids!(mp4_name), ids!(mp4_info), ids!(mp4_btn)),
        _ => (ids!(mp5), ids!(mp5_name), ids!(mp5_info), ids!(mp5_btn)),
    }
}

impl App {
    /// Offer `feature`'s download in the bar (or say why it can't run here).
    /// True when the feature can't run yet.
    pub(crate) fn needs_download(&mut self, cx: &mut Cx, feature: &'static str, resume: Resume) -> bool {
        match models::readiness(feature) {
            Readiness::Ready => false,
            Readiness::CantRun(why) => {
                self.show_bar(cx, &why, None, Some("OK"));
                true
            }
            Readiness::Needs(..) => {
                self.offer_download(cx, feature, resume);
                true
            }
        }
    }

    pub(crate) fn offer_download(&mut self, cx: &mut Cx, feature: &'static str, resume: Resume) {
        if self.downloads.busy() {
            // one at a time: this one is offered again when it is asked for again
            return;
        }
        let Some(text) = models::offer_text(feature) else { return };
        self.offer = Some(Offer { feature, resume });
        self.show_bar(cx, &text, Some("Download"), Some("Not now"));
    }

    fn show_bar(&mut self, cx: &mut Cx, text: &str, yes: Option<&str>, no: Option<&str>) {
        let ui = self.ui.clone();
        ui.label(cx, ids!(offer_label)).set_text(cx, text);
        for (id, label) in [(ids!(offer_yes), yes), (ids!(offer_no), no)] {
            let b = ui.button(cx, id);
            b.set_visible(cx, label.is_some());
            if let Some(l) = label {
                b.set_text(cx, l);
            }
        }
        ui.view(cx, ids!(offer_bar)).set_visible(cx, true);
        ui.redraw(cx);
    }

    fn hide_bar(&mut self, cx: &mut Cx) {
        self.ui.view(cx, ids!(offer_bar)).set_visible(cx, false);
        self.ui.redraw(cx);
    }

    /// The offer bar's and the Models page's buttons. True when one was clicked.
    pub(crate) fn model_actions(&mut self, cx: &mut Cx, actions: &Actions) -> bool {
        let ui = self.ui.clone();
        if ui.button(cx, ids!(offer_yes)).clicked(actions) {
            match &self.offer {
                Some(o) if !self.downloads.busy() => {
                    self.downloads.start(o.feature);
                    self.show_bar(cx, "Starting the download", None, Some("Stop"));
                }
                _ => self.hide_bar(cx),
            }
            return true;
        }
        if ui.button(cx, ids!(offer_no)).clicked(actions) {
            if self.downloads.busy() {
                self.downloads.stop();
                self.show_bar(cx, "Stopping…", None, None);
            } else {
                let analysis = self.offer.as_ref().is_some_and(|o| o.feature == "analysis");
                self.offer = None;
                self.hide_bar(cx);
                if analysis {
                    self.set_status(cx, "The song plays as it is. Mark a range on it to repaint that; Settings → Models downloads the analysis later.");
                }
            }
            return true;
        }
        let packs = &models::registry().packs;
        for i in 0..packs.len().min(ROWS) {
            if !ui.button(cx, row_ids(i).3).clicked(actions) {
                continue;
            }
            let id = packs[i].id.clone();
            if self.downloads.what.as_deref() == Some(id.as_str()) {
                self.downloads.stop();
            } else if models::installed_pack(&id).is_some() {
                let note = match models::remove(&id) {
                    Ok(bytes) => format!("Removed {}: {} freed.", packs[i].title, models::gb(bytes)),
                    Err(e) => format!("Could not remove {}: {e}", packs[i].title),
                };
                ui.label(cx, ids!(models_note)).set_text(cx, &note);
                self.set_status(cx, &note);
                ui.label(cx, ids!(home_status)).set_text(cx, &note);
            } else if !self.downloads.busy() {
                self.offer = None;
                self.downloads.start(&id);
                self.show_bar(cx, "Starting the download", None, Some("Stop"));
            }
            self.refresh_models(cx);
            return true;
        }
        false
    }

    /// Download progress into the bar; when it is done, carry on with what asked for it.
    pub(crate) fn drain_downloads(&mut self, cx: &mut Cx) {
        for u in self.downloads.poll() {
            match u {
                Update::Progress(text) => self.show_bar(cx, &text, None, Some("Stop")),
                Update::Failed(why) => {
                    let retry = self.offer.is_some().then_some("Try again");
                    self.show_bar(cx, &why, retry, Some("Not now"));
                }
                Update::Done(what) => {
                    self.hide_bar(cx);
                    let title = models::registry().features.iter().find(|f| f.id == what).map(|f| f.title.clone()).or_else(|| models::pack(&what).map(|p| p.title.clone()));
                    let note = format!("Downloaded: {} works now.", title.unwrap_or(what));
                    self.ui.label(cx, ids!(home_status)).set_text(cx, &note);
                    self.set_status(cx, &note);
                    if let Some(offer) = self.offer.take() {
                        self.resume(cx, offer.resume);
                    }
                }
            }
        }
        if self.settings_shown {
            self.refresh_models(cx);
        }
    }

    fn resume(&mut self, cx: &mut Cx, resume: Resume) {
        if let Resume::MakeSong(prompt) = resume {
            return self.make_song(cx, &prompt);
        }
        let jobs = self.jobs.as_ref();
        let Some(st) = self.state.as_mut() else { return };
        let note = match resume {
            Resume::Analyse(p) if same_song(&st.path, &p) => {
                analyse(jobs, st);
                return;
            }
            Resume::Describe(p, text) if same_song(&st.path, &p) => describe(st, jobs, &text),
            // as the Describe bar does it: describe() may hand it to the assistant
            Resume::Ask(p, text) if same_song(&st.path, &p) && self.agent_rx.is_none() => {
                st.agent_ok = true;
                let note = describe(st, jobs, &text);
                st.agent_ok = false;
                if let Some(request) = st.ask_agent.take() {
                    self.agent_rx = Some(crate::start_agent(st, request));
                }
                note
            }
            Resume::Remix(p) if same_song(&st.path, &p) => match jobs {
                Some(jobs) => start_remix(st, jobs),
                None => return,
            },
            _ => return,
        };
        self.set_status(cx, &note);
    }

    /// Settings → Models: where models live, what this PC has, then each pack.
    pub(crate) fn refresh_models(&mut self, cx: &mut Cx) {
        let ui = self.ui.clone();
        let dir = models::models_dir();
        let gpu = match models::gpu() {
            Some(g) => format!("{} ({:.0} GB, driver {})", g.name, g.vram_mb as f64 / 1024.0, g.driver),
            None => "no NVIDIA GPU found".into(),
        };
        let dev = if paths::dev_ready("repaint") { " ACE-Step runs from a development checkout here, so its packs are not needed." } else { "" };
        let yue2 = if audio::yue2_available() { "YuE2 (whole-song covers) is set up." } else { "YuE2 (whole-song covers) is not set up: remixes use ACE-Step." };
        ui.label(cx, ids!(models_status)).set_text(
            cx,
            &format!(
                "Each AI feature asks before it downloads. Models live in {} (POING_MODELS moves them). This PC: {gpu}.{dev} {yue2} New songs are saved in {}.",
                dir.display(),
                audio::songs_dir().display()
            ),
        );
        let packs = &models::registry().packs;
        for i in 0..ROWS {
            let (row, name, info, btn) = row_ids(i);
            let Some(p) = packs.get(i) else {
                ui.view(cx, row).set_visible(cx, false);
                continue;
            };
            ui.view(cx, row).set_visible(cx, true);
            ui.label(cx, name).set_text(cx, &p.title);
            let licence = if p.commercial { p.licence.clone() } else { format!("{}, personal non-commercial use only", p.licence) };
            let licence = format!("{licence} ({})", p.licence_url);
            let (state, action) = if self.downloads.what.as_deref() == Some(p.id.as_str()) {
                ("downloading".to_string(), Some("Stop"))
            } else if let Some(dir) = models::installed_pack(&p.id) {
                (format!("installed, {} on disk", models::gb(models::folder_bytes(&dir))), Some("Remove"))
            } else {
                let partial = models::folder_bytes(&dir.join(&p.id));
                let got = if partial > 0 { format!(", {} so far", models::gb(partial)) } else { String::new() };
                (format!("{}{got}", models::gb(models::pack_bytes(p))), (!self.downloads.busy()).then_some("Download"))
            };
            ui.label(cx, info).set_text(cx, &format!("{} · {licence} · {state}", p.what));
            let b = ui.button(cx, btn);
            b.set_visible(cx, action.is_some());
            if let Some(a) = action {
                b.set_text(cx, a);
            }
        }
        ui.redraw(cx);
    }
}
