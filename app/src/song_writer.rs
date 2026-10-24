//! The language model (`llm::ask`) writes a song sheet from a prompt ("a summer song about the farm,
//! in Dutch, acoustic"): a title, the style as ACE-Step caption tags, tempo,
//! key, language, length and section-tagged lyrics. ACE-Step then sings it.

use makepad_micro_serde::*;
use std::path::Path;

#[derive(DeJson, SerJson, Clone, Debug, Default)]
pub struct Sheet {
    pub title: String,
    pub caption: String,
    pub bpm: Option<f64>,
    pub key: Option<String>,
    pub language: Option<String>,
    pub duration: Option<f64>,
    pub lyrics: String,
}

const BRIEF: &str = r#"You write the sheet for a new song that a music model (ACE-Step) will sing and play.

Reply with one fenced ```json block and nothing else:
{"title": "...", "caption": "...", "bpm": 120, "key": "A minor", "language": "en",
 "duration": 150, "lyrics": "[Verse]\n...\n\n[Chorus]\n..."}

- title: a short song title.
- caption: the sound as comma-separated tags the music model understands: genre and
  era, the main instruments, the mood, the vocal type (e.g. "male vocals",
  "female vocals", "choir"), production style. 8-15 tags, English.
- bpm and key fit the style. key like "A minor" or "F# major".
- language: ISO 639-1 code of the lyrics (en, nl, af, de, fr, es, ...): the language
  the prompt asks for, else the prompt's own language.
- duration: seconds, 90-210 (150 unless the prompt asks otherwise).
- lyrics: written to be sung, in that language, each section tagged on its own line:
  [Intro] [Verse] [Pre-Chorus] [Chorus] [Bridge] [Outro]. Every line under a tag is
  sung: an instrumental section is the tag alone, never a description of what plays.
  Short lines that scan, 4-8 lines per section, a chorus that returns.
  Enough sections to fill the duration. Empty string "" for an instrumental."#;

/// Read the sheet out of the model's answer.
pub fn parse_answer(text: &str) -> Result<Sheet, String> {
    let start = text.find('{').ok_or("no JSON in the answer")?;
    let end = text.rfind('}').ok_or("no JSON in the answer")?;
    let mut sheet = Sheet::deserialize_json_lenient(&text[start..=end]).map_err(|e| format!("the sheet is not valid JSON: {e:?}"))?;
    sheet.title = sheet.title.trim().to_string();
    if sheet.title.is_empty() {
        sheet.title = "New song".into();
    }
    if sheet.caption.trim().is_empty() {
        return Err("the sheet has no caption".into());
    }
    sheet.lyrics = unsung_removed(&sheet.lyrics);
    sheet.duration = Some(sheet.duration.unwrap_or(150.0).clamp(60.0, 240.0));
    sheet.bpm = sheet.bpm.filter(|b| (40.0..=220.0).contains(b));
    Ok(sheet)
}

/// The lyrics without what a model wrote about the music instead of words to
/// sing ("guitar strumming pattern" under [Intro]): ACE-Step would sing it.
/// Only short lines naming instruments or playing, under an instrumental tag.
fn unsung_removed(lyrics: &str) -> String {
    const PLAYING: [&str; 16] = [
        "guitar", "strum", "piano", "drum", "bass", "synth", "solo", "instrumental", "beat", "riff", "melody", "fade", "chords", "pattern", "plays", "strings",
    ];
    let mut instrumental = false;
    let mut out = Vec::new();
    for line in lyrics.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            let tag = t.to_lowercase();
            instrumental = ["intro", "outro", "interlude", "instrumental", "solo", "break"].iter().any(|k| tag.contains(k));
        } else if instrumental && t.split_whitespace().count() <= 6 {
            let l = t.trim_matches(|c: char| c == '(' || c == ')' || c == '*').to_lowercase();
            if PLAYING.iter().any(|w| l.contains(w)) {
                continue;
            }
        }
        out.push(line);
    }
    out.join("\n")
}

/// Ask the model for a sheet for `prompt` (one retry when the answer is unreadable).
pub fn write_sheet(prompt: &str, work: &Path) -> Result<Sheet, String> {
    std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
    // (repeated from the brief: a small model forgets it there)
    let ask_for = format!("The song: \"{prompt}\"

Write the lyrics in the language the song asks for; if it names none, in the language its description above is written in.");
    let mut ask = ask_for.clone();
    let mut last_error = String::new();
    for round in 1..=2 {
        let answer = crate::llm::ask(BRIEF, &ask, 3000, work)?;
        let _ = std::fs::write(work.join(format!("sheet-{round}.md")), &answer);
        match parse_answer(&answer) {
            Ok(sheet) => return Ok(sheet),
            Err(e) => {
                ask = format!("{ask_for}\n\nYour last answer could not be read: {e}. Reply with the JSON block only.");
                last_error = e;
            }
        }
    }
    Err(last_error)
}

/// A file-system-safe folder name for a title.
pub fn folder_name(title: &str) -> String {
    let s: String = title.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { ' ' }).collect();
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.is_empty() { "New song".into() } else { s.chars().take(60).collect() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sheet_is_read_and_kept_in_range() {
        let a = "```json\n{\"title\": \"Dans met mij\", \"caption\": \"80s disco, slap bass\", \"bpm\": 120, \"key\": \"A minor\", \"language\": \"nl\", \"duration\": 900, \"lyrics\": \"[Verse]\\nDe zon\"}\n```";
        let s = parse_answer(a).unwrap();
        assert_eq!(s.title, "Dans met mij");
        assert_eq!(s.duration, Some(240.0));
        assert_eq!(s.lyrics, "[Verse]\nDe zon");
        assert!(parse_answer("no sheet").is_err());
        assert_eq!(folder_name("Dans: met/mij?"), "Dans met mij");
    }

    #[test]
    fn descriptions_of_the_music_are_not_sung() {
        let l = "[Intro]\nguitar strumming pattern\n\n[Verse 1]\nDe zon schijnt warm op het gras\nDe gitaar speelt zacht\n\n[Outro]\n(fade out)\nTot morgen, boerderij";
        assert_eq!(unsung_removed(l), "[Intro]\n\n[Verse 1]\nDe zon schijnt warm op het gras\nDe gitaar speelt zacht\n\n[Outro]\nTot morgen, boerderij");
    }
}
