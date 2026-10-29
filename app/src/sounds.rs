//! Named sounds: an installed instrument plugin plus the state that makes it
//! play one thing ("BBC SO violins", "Una Corda felt"). Sample players such as
//! Kontakt and the BBC Symphony Orchestra open empty, and some synths (Vital,
//! Massive X) are silent at their default patch, so a part names a sound, not a plugin.
//!
//! Two sources:
//! - NKS presets (`.nksf`, the files Komplete Kontrol browses): a RIFF file whose
//!   `PCHK` chunk is the plugin's own state and whose `NISI`/`PLID` chunks
//!   (MessagePack) say what it is and which plugin it is for. Found under the
//!   content folders Native Instruments' installers register, and the user's
//!   NI "User Content".
//! - Sounds saved from Poing: a lane's plugin as it is now (after choosing an
//!   instrument in its own window, the only way into a Kontakt library), kept
//!   in the per-user `sounds` folder.

use makepad_micro_serde::*;
use std::path::{Path, PathBuf};

/// One sound Poing can put on a MIDI lane.
#[derive(SerJson, DeJson, Clone, Debug, PartialEq)]
pub struct SoundInfo {
    pub name: String,
    /// The plugin's bundle name, as in [`crate::plugins::library`] ("Massive X").
    pub plugin: String,
    /// What it is, from the preset's tags ("Bowed Strings, Violin").
    pub types: String,
    /// The `.nksf` preset or the saved `.state`.
    pub file: String,
    /// Saved from Poing (else an NKS preset).
    pub saved: bool,
}

/// What an NKS preset holds.
#[derive(Clone, Debug, Default)]
pub struct Nks {
    pub name: String,
    pub types: String,
    pub plugin_name: Option<String>,
    /// The VST2 id the preset was made for (four characters as a number).
    pub vst_magic: Option<u32>,
    /// The plugin's own state (`PCHK` without its version word).
    pub state: Vec<u8>,
}

/// Plugins whose presets name only a VST2 id: (id, bundle name).
const BY_MAGIC: [(u32, &str); 2] = [(0x5361_6E74, "BBC Symphony Orchestra (64 Bit)"), (0x4E69_2448, "Massive X")];

/// Read an `.nksf` file's chunks.
pub fn read_nks(bytes: &[u8]) -> Result<Nks, String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"NIKS" {
        return Err("not an NKS preset".into());
    }
    let mut out = Nks::default();
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let n = u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        let body = bytes.get(i + 8..i + 8 + n).ok_or("a chunk runs past the end")?;
        // every NKS chunk starts with a 4-byte version
        let data = body.get(4..).unwrap_or_default();
        match id {
            b"PCHK" => out.state = data.to_vec(),
            b"NISI" => {
                let m = msgpack(data).ok_or("unreadable NISI")?;
                out.name = m.get("name").and_then(Mp::str).unwrap_or_default().to_string();
                out.types = m.get("types").map(Mp::flat).unwrap_or_default().join(", ");
            }
            b"PLID" => {
                let m = msgpack(data).ok_or("unreadable PLID")?;
                out.plugin_name = m.get("pluginName").and_then(Mp::str).map(str::to_string);
                out.vst_magic = m.get("VST.magic").and_then(Mp::int).filter(|v| *v != 0).map(|v| v as u32);
            }
            _ => {}
        }
        i += 8 + n + (n & 1);
    }
    if out.state.is_empty() {
        return Err("the preset has no plugin state".into());
    }
    Ok(out)
}

/// The installed plugin a preset is for, among `plugins` (name, bundle).
pub fn plugin_for(nks: &Nks, plugins: &[(String, PathBuf)]) -> Option<String> {
    let by_name = nks.plugin_name.as_deref().and_then(|n| plugins.iter().find(|(p, _)| p.eq_ignore_ascii_case(n)));
    let by_magic = || {
        let name = BY_MAGIC.iter().find(|(m, _)| Some(*m) == nks.vst_magic)?.1;
        plugins.iter().find(|(p, _)| p == name)
    };
    by_name.or_else(by_magic).map(|(p, _)| p.clone())
}

/// A tiny MessagePack reader: the maps of strings, numbers and string arrays NKS uses.
#[derive(Clone, Debug, PartialEq)]
pub enum Mp {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Bin(Vec<u8>),
    Arr(Vec<Mp>),
    Map(Vec<(Mp, Mp)>),
}

impl Mp {
    pub fn get(&self, key: &str) -> Option<&Mp> {
        let Mp::Map(kv) = self else { return None };
        kv.iter().find(|(k, _)| k.str() == Some(key)).map(|(_, v)| v)
    }
    pub fn str(&self) -> Option<&str> {
        if let Mp::Str(s) = self { Some(s) } else { None }
    }
    pub fn int(&self) -> Option<i64> {
        if let Mp::Int(v) = self { Some(*v) } else { None }
    }
    /// Every string in it, depth first ("types": [["Bowed Strings", "Violin"]]).
    pub fn flat(&self) -> Vec<String> {
        match self {
            Mp::Str(s) => vec![s.clone()],
            Mp::Arr(a) => a.iter().flat_map(Mp::flat).collect(),
            _ => Vec::new(),
        }
    }
}

pub fn msgpack(bytes: &[u8]) -> Option<Mp> {
    let mut at = 0;
    let v = mp_value(bytes, &mut at, 0)?;
    Some(v)
}

fn mp_value(b: &[u8], at: &mut usize, depth: u32) -> Option<Mp> {
    if depth > 32 {
        return None;
    }
    let take = |at: &mut usize, n: usize| -> Option<&[u8]> {
        let s = b.get(*at..*at + n)?;
        *at += n;
        Some(s)
    };
    let be = |s: &[u8]| s.iter().fold(0u64, |a, x| a << 8 | *x as u64);
    let t = *take(at, 1)?.first()?;
    let str_of = |at: &mut usize, n: usize| Some(Mp::Str(String::from_utf8_lossy(take(at, n)?).into_owned()));
    let arr_of = |at: &mut usize, n: usize| (0..n).map(|_| mp_value(b, at, depth + 1)).collect::<Option<Vec<_>>>().map(Mp::Arr);
    let map_of = |at: &mut usize, n: usize| {
        (0..n).map(|_| Some((mp_value(b, at, depth + 1)?, mp_value(b, at, depth + 1)?))).collect::<Option<Vec<_>>>().map(Mp::Map)
    };
    Some(match t {
        0x00..=0x7f => Mp::Int(t as i64),
        0x80..=0x8f => return map_of(at, (t & 0x0f) as usize),
        0x90..=0x9f => return arr_of(at, (t & 0x0f) as usize),
        0xa0..=0xbf => return str_of(at, (t & 0x1f) as usize),
        0xc0 => Mp::Nil,
        0xc2 => Mp::Bool(false),
        0xc3 => Mp::Bool(true),
        0xc4..=0xc6 => {
            let n = be(take(at, 1 << (t - 0xc4))?) as usize;
            Mp::Bin(take(at, n)?.to_vec())
        }
        0xca => Mp::Float(f32::from_bits(be(take(at, 4)?) as u32) as f64),
        0xcb => Mp::Float(f64::from_bits(be(take(at, 8)?))),
        0xcc..=0xcf => Mp::Int(be(take(at, 1 << (t - 0xcc))?) as i64),
        0xd0 => Mp::Int(take(at, 1)?[0] as i8 as i64),
        0xd1 => Mp::Int(be(take(at, 2)?) as u16 as i16 as i64),
        0xd2 => Mp::Int(be(take(at, 4)?) as u32 as i32 as i64),
        0xd3 => Mp::Int(be(take(at, 8)?) as i64),
        0xd9..=0xdb => {
            let n = be(take(at, 1 << (t - 0xd9))?) as usize;
            return str_of(at, n);
        }
        0xdc | 0xdd => {
            let n = be(take(at, if t == 0xdc { 2 } else { 4 })?) as usize;
            return arr_of(at, n);
        }
        0xde | 0xdf => {
            let n = be(take(at, if t == 0xde { 2 } else { 4 })?) as usize;
            return map_of(at, n);
        }
        0xe0..=0xff => Mp::Int(t as i8 as i64),
        _ => return None, // ext types: not in NKS metadata
    })
}

/// Where sounds saved from Poing live: `POING_SOUNDS`, else next to the models folder.
pub fn sounds_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("POING_SOUNDS") {
        return PathBuf::from(d);
    }
    let models = crate::models::models_dir();
    models.parent().map(|p| p.join("sounds")).unwrap_or_else(|| models.join("sounds"))
}

/// Folders to look for NKS presets in: those Native Instruments' and partners'
/// installers register (`ContentDir`), and the user's NI User Content.
fn nks_roots() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = registered_content_dirs();
    if let Some(home) = std::env::var_os("USERPROFILE") {
        out.push(PathBuf::from(home).join("Documents").join("Native Instruments").join("User Content"));
    }
    if let Some(extra) = std::env::var_os("POING_NKS") {
        out.extend(std::env::split_paths(&extra));
    }
    out.retain(|p| p.is_dir());
    out.sort();
    out.dedup();
    out
}

/// `HKLM\SOFTWARE\Native Instruments\<product>\ContentDir` for every product.
#[cfg(windows)]
fn registered_content_dirs() -> Vec<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    type Hkey = isize;
    const HKEY_LOCAL_MACHINE: Hkey = 0x8000_0002u32 as i32 as isize;
    const KEY_READ: u32 = 0x20019;
    const RRF_RT_REG_SZ: u32 = 0x2;
    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(key: Hkey, sub: *const u16, opts: u32, sam: u32, out: *mut Hkey) -> i32;
        fn RegEnumKeyExW(key: Hkey, i: u32, name: *mut u16, len: *mut u32, r: *mut u32, c: *mut u16, cl: *mut u32, t: *mut u64) -> i32;
        fn RegGetValueW(key: Hkey, sub: *const u16, value: *const u16, flags: u32, ty: *mut u32, data: *mut u8, len: *mut u32) -> i32;
        fn RegCloseKey(key: Hkey) -> i32;
    }
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let mut out = Vec::new();
    let mut root: Hkey = 0;
    // SAFETY: plain registry reads into stack and heap buffers this function owns.
    unsafe {
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, wide("SOFTWARE\\Native Instruments").as_ptr(), 0, KEY_READ, &mut root) != 0 {
            return out;
        }
        for i in 0..4096 {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            if RegEnumKeyExW(root, i, name.as_mut_ptr(), &mut len, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) != 0 {
                break;
            }
            let mut sub = name[..len as usize].to_vec();
            sub.push(0);
            let mut buf = vec![0u16; 1024];
            let mut bytes = (buf.len() * 2) as u32;
            let ok = RegGetValueW(root, sub.as_ptr(), wide("ContentDir").as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr() as *mut u8, &mut bytes);
            if ok == 0 {
                let n = (bytes as usize / 2).saturating_sub(1);
                out.push(PathBuf::from(std::ffi::OsString::from_wide(&buf[..n])));
            }
        }
        RegCloseKey(root);
    }
    out
}

#[cfg(not(windows))]
fn registered_content_dirs() -> Vec<PathBuf> {
    Vec::new()
}

fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 6 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => walk(&p, depth + 1, out),
            Ok(_) if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("nksf")) => out.push(p),
            _ => {}
        }
    }
}

/// The index cached in the sounds folder: NKS presets read once.
#[derive(SerJson, DeJson, Default)]
struct Index {
    /// (file, size) of every preset read, with what it turned out to be (None: not usable).
    seen: Vec<(String, u64, Option<SoundInfo>)>,
}

/// Every sound for the installed `plugins`: presets (cached, new ones read)
/// and sounds saved from Poing, saved ones first.
pub fn library(plugins: &[(String, PathBuf)]) -> Vec<SoundInfo> {
    let dir = sounds_dir();
    let cache = dir.join("nks-index.json");
    let old = std::fs::read_to_string(&cache).ok().and_then(|s| Index::deserialize_json(&s).ok()).unwrap_or_default();
    let mut files = Vec::new();
    for root in nks_roots() {
        walk(&root, 0, &mut files);
    }
    let mut index = Index::default();
    for f in files {
        let key = f.to_string_lossy().into_owned();
        let size = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
        let known = old.seen.iter().find(|(k, s, _)| *k == key && *s == size).map(|(_, _, i)| i.clone());
        let info = known.unwrap_or_else(|| {
            let nks = std::fs::read(&f).ok().and_then(|b| read_nks(&b).ok())?;
            let plugin = plugin_for(&nks, plugins)?;
            let name = if nks.name.is_empty() { f.file_stem()?.to_string_lossy().into_owned() } else { nks.name.clone() };
            Some(SoundInfo { name, plugin, types: nks.types, file: key.clone(), saved: false })
        });
        index.seen.push((key, size, info));
    }
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&cache, index.serialize_json());
    let mut out = saved_sounds(&dir);
    out.extend(index.seen.into_iter().filter_map(|(_, _, i)| i).filter(|i| plugins.iter().any(|(p, _)| *p == i.plugin)));
    out
}

fn saved_sounds(dir: &Path) -> Vec<SoundInfo> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<SoundInfo> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.to_string_lossy().ends_with(".sound.json"))
        .filter_map(|p| SoundInfo::deserialize_json(&std::fs::read_to_string(p).ok()?).ok())
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

static CACHE: std::sync::Mutex<Option<std::sync::Arc<Vec<SoundInfo>>>> = std::sync::Mutex::new(None);

/// [`library`], read once per run (the first time can take seconds: every
/// preset is opened; later runs read the index). `warm` does it in the background.
pub fn cached(plugins: &[(String, PathBuf)]) -> std::sync::Arc<Vec<SoundInfo>> {
    if let Some(l) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return l;
    }
    let l = std::sync::Arc::new(library(plugins));
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(l.clone());
    l
}

/// Build the cache on a thread of its own (at start-up).
pub fn warm(plugins: Vec<(String, PathBuf)>) {
    static STARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if STARTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let _ = std::thread::Builder::new().name("poing-sounds".into()).spawn(move || {
        cached(&plugins);
    });
}

/// Read the library again next time (a sound was saved).
pub fn forget() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The sound's plugin state.
pub fn state_of(s: &SoundInfo) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(&s.file).map_err(|e| format!("{}: {e}", s.file))?;
    if s.saved {
        return Ok(bytes);
    }
    Ok(read_nks(&bytes)?.state)
}

/// Keep a plugin's state as a named sound (replacing one of the same name).
pub fn save(name: &str, plugin: &str, types: &str, state: &[u8]) -> Result<SoundInfo, String> {
    let dir = sounds_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let safe: String = name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
    let safe = safe.trim().to_string();
    if safe.is_empty() {
        return Err("a sound needs a name".into());
    }
    let file = dir.join(format!("{safe}.state"));
    std::fs::write(&file, state).map_err(|e| e.to_string())?;
    let info = SoundInfo { name: name.trim().to_string(), plugin: plugin.to_string(), types: types.to_string(), file: file.to_string_lossy().into_owned(), saved: true };
    std::fs::write(dir.join(format!("{safe}.sound.json")), info.serialize_json()).map_err(|e| e.to_string())?;
    Ok(info)
}

/// Sounds matching every word of `query` (in name, tags or plugin), best first:
/// an exact name, then the most words in the name, saved sounds before presets.
pub fn search<'a>(lib: &'a [SoundInfo], query: &str) -> Vec<&'a SoundInfo> {
    let q = query.to_lowercase();
    let words: Vec<&str> = q.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let mut hits: Vec<(i64, &SoundInfo)> = lib
        .iter()
        .filter_map(|s| {
            let name = s.name.to_lowercase();
            let all = format!("{name} {} {}", s.types.to_lowercase(), s.plugin.to_lowercase());
            if words.is_empty() || !words.iter().all(|w| all.contains(w)) {
                return None;
            }
            let in_name = words.iter().filter(|w| name.contains(*w)).count() as i64;
            Some(((name == q) as i64 * 100 + in_name * 10 + s.saved as i64, s))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
    hits.into_iter().map(|(_, s)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An NKS file as Komplete Kontrol writes it: PLID, PCHK, NISI.
    fn nksf(plid: &[u8], nisi: &[u8], state: &[u8]) -> Vec<u8> {
        let mut body = b"NIKS".to_vec();
        for (id, data) in [(b"PLID", plid), (b"PCHK", state), (b"NISI", nisi)] {
            let mut chunk = 1u32.to_le_bytes().to_vec();
            chunk.extend_from_slice(data);
            body.extend_from_slice(id);
            body.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            body.extend_from_slice(&chunk);
            if chunk.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn a_preset_names_its_sound_and_plugin() {
        // {"VST.magic": 0x53616E74}
        let plid = [0x81, 0xa9, b'V', b'S', b'T', b'.', b'm', b'a', b'g', b'i', b'c', 0xce, 0x53, 0x61, 0x6e, 0x74];
        // {"name": "Violins 1", "types": [["Bowed Strings", "Violin"]]}
        let mut nisi = vec![0x82, 0xa4];
        nisi.extend_from_slice(b"name");
        nisi.push(0xa9);
        nisi.extend_from_slice(b"Violins 1");
        nisi.push(0xa5);
        nisi.extend_from_slice(b"types");
        nisi.extend_from_slice(&[0x91, 0x92, 0xad]);
        nisi.extend_from_slice(b"Bowed Strings");
        nisi.push(0xa6);
        nisi.extend_from_slice(b"Violin");
        let nks = read_nks(&nksf(&plid, &nisi, b"VC2!state")).unwrap();
        assert_eq!(nks.name, "Violins 1");
        assert_eq!(nks.types, "Bowed Strings, Violin");
        assert_eq!(nks.vst_magic, Some(0x5361_6E74));
        assert_eq!(nks.state, b"VC2!state");
        let plugins = vec![("BBC Symphony Orchestra (64 Bit)".to_string(), PathBuf::from("bbc.vst3")), ("Serum2".to_string(), PathBuf::from("s.vst3"))];
        assert_eq!(plugin_for(&nks, &plugins).as_deref(), Some("BBC Symphony Orchestra (64 Bit)"));
        assert!(read_nks(b"RIFF\0\0\0\0WAVE").is_err());
    }

    #[test]
    fn search_wants_every_word_and_ranks_names_first() {
        let s = |name: &str, types: &str, saved| SoundInfo { name: name.into(), plugin: "BBC".into(), types: types.into(), file: String::new(), saved };
        let lib = vec![s("Strings: Discover - Violins 1", "Bowed Strings, Violin", false), s("Strings: Discover - Celli", "Bowed Strings, Cello", false), s("Warm violins", "", true)];
        let names = |q| search(&lib, q).iter().map(|x| x.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("violins"), ["Warm violins", "Strings: Discover - Violins 1"]);
        assert_eq!(names("bowed cello"), ["Strings: Discover - Celli"]);
        assert!(names("trumpet").is_empty());
    }
}
