//! Reading SFZ instrument files (the text format most free sample libraries use).
//!
//! Supported: `<control>` (`default_path`), `<global>`, `<master>`, `<group>` and
//! `<region>` headers with inheritance, `#define $NAME value`, `#include "file"`,
//! comments, and the opcodes Musiton's sample player understands (see [`Region`]).
//! Unknown opcodes are ignored.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use thiserror::Error;

/// Errors while reading an SFZ file.
#[derive(Debug, Error)]
pub enum SfzError {
    /// The file could not be read.
    #[error("cannot read {path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The cause.
        source: std::io::Error,
    },
    /// `#include` nests too deep (probably a loop).
    #[error("#include nests too deep at {0}")]
    IncludeDepth(PathBuf),
    /// The file has no playable region.
    #[error("{0} has no regions with samples")]
    Empty(PathBuf),
}

/// When a region plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Trigger {
    /// On note-on.
    #[default]
    Attack,
    /// On note-off (e.g. piano damper noise).
    Release,
}

/// How a region's sample loops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LoopMode {
    /// Plays once to the end (also after release).
    #[default]
    NoLoop,
    /// Plays once to the end, ignoring note-off (drums).
    OneShot,
    /// Loops between `loop_start` and `loop_end` while sounding.
    Continuous,
    /// Loops while the key is held, then plays to the end.
    Sustain,
}

/// One playable zone of an instrument.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    /// The sample file.
    pub sample: PathBuf,
    /// Key range (inclusive).
    pub lokey: u8,
    /// Key range (inclusive).
    pub hikey: u8,
    /// Velocity range (inclusive, 1–127).
    pub lovel: u8,
    /// Velocity range (inclusive, 1–127).
    pub hivel: u8,
    /// The key at which the sample plays at its recorded pitch.
    pub pitch_keycenter: f32,
    /// Cents of pitch change per key (100 = normal, 0 = same pitch on every key).
    pub pitch_keytrack: f32,
    /// Fine tuning in cents (plus `transpose` × 100).
    pub tune: f32,
    /// Level in dB.
    pub volume: f32,
    /// Pan −100 … 100.
    pub pan: f32,
    /// How much velocity changes the level (0 … 100 %).
    pub amp_veltrack: f32,
    /// Amplitude envelope (seconds; sustain 0–100 %).
    pub ampeg_attack: f32,
    /// Hold time.
    pub ampeg_hold: f32,
    /// Decay time.
    pub ampeg_decay: f32,
    /// Sustain level in percent.
    pub ampeg_sustain: f32,
    /// Release time.
    pub ampeg_release: f32,
    /// Start offset in samples.
    pub offset: u64,
    /// When it plays.
    pub trigger: Trigger,
    /// Looping.
    pub loop_mode: LoopMode,
    /// Loop start in samples (`None`: from the sample file, else the whole sample).
    pub loop_start: Option<u64>,
    /// Loop end in samples (inclusive).
    pub loop_end: Option<u64>,
    /// Round robin: cycle length and this region's place (1-based).
    pub seq_length: u32,
    /// Round robin position.
    pub seq_position: u32,
    /// Exclusive group: a note in group `off_by` stops this region's voices.
    pub group: i64,
    /// See `group`.
    pub off_by: i64,
    /// Random choice: plays when a random number 0 … 1 falls in `lorand..hirand`.
    pub lorand: f32,
    /// See `lorand`.
    pub hirand: f32,
}

impl Default for Region {
    fn default() -> Self {
        Self {
            sample: PathBuf::new(),
            lokey: 0,
            hikey: 127,
            lovel: 1,
            hivel: 127,
            pitch_keycenter: 60.0,
            pitch_keytrack: 100.0,
            tune: 0.0,
            volume: 0.0,
            pan: 0.0,
            amp_veltrack: 100.0,
            ampeg_attack: 0.0,
            ampeg_hold: 0.0,
            ampeg_decay: 0.0,
            ampeg_sustain: 100.0,
            ampeg_release: 0.001,
            offset: 0,
            trigger: Trigger::Attack,
            loop_mode: LoopMode::NoLoop,
            loop_start: None,
            loop_end: None,
            seq_length: 1,
            seq_position: 1,
            group: 0,
            off_by: 0,
            lorand: 0.0,
            hirand: 1.0,
        }
    }
}

/// A parsed SFZ instrument.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SfzInstrument {
    /// Every region with a sample, in file order.
    pub regions: Vec<Region>,
}

impl SfzInstrument {
    /// The sample files the regions use (each once).
    pub fn samples(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for r in &self.regions {
            if !out.contains(&r.sample) {
                out.push(r.sample.clone());
            }
        }
        out
    }
}

/// Reads the SFZ file at `path`.
pub fn read_sfz(path: &Path) -> Result<SfzInstrument, SfzError> {
    let mut text = String::new();
    let mut defines = HashMap::new();
    load(path, &mut text, &mut defines, 0)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    let inst = parse(&text, dir);
    if inst.regions.is_empty() {
        return Err(SfzError::Empty(path.to_owned()));
    }
    Ok(inst)
}

/// Reads `path` into `out`, resolving `#include` and `#define`.
fn load(
    path: &Path,
    out: &mut String,
    defines: &mut HashMap<String, String>,
    depth: usize,
) -> Result<(), SfzError> {
    if depth > 16 {
        return Err(SfzError::IncludeDepth(path.to_owned()));
    }
    let raw = std::fs::read(path).map_err(|source| SfzError::Io {
        path: path.to_owned(),
        source,
    })?;
    let text = String::from_utf8_lossy(&raw);
    let dir = path.parent().unwrap_or(Path::new("."));
    for line in strip_comments(&text).lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("#define") {
            let mut parts = rest.split_whitespace();
            if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
                defines.insert(k.to_owned(), v.to_owned());
            }
            continue;
        }
        if let Some(rest) = t.strip_prefix("#include") {
            let name = rest.trim().trim_matches('"');
            load(&dir.join(name), out, defines, depth + 1)?;
            continue;
        }
        let mut line = line.to_owned();
        // Longest names first so `$A` does not replace inside `$AB`.
        let mut keys: Vec<&String> = defines.keys().collect();
        keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
        for k in keys {
            if let Some(v) = defines.get(k) {
                line = line.replace(k.as_str(), v);
            }
        }
        out.push_str(&line);
        out.push('\n');
    }
    Ok(())
}

/// Removes `//` line comments and `/* */` block comments.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let line = rest.find("//");
        let block = rest.find("/*");
        match (line, block) {
            (Some(l), b) if b.is_none_or(|b| l < b) => {
                out.push_str(&rest[..l]);
                rest = rest[l..].find('\n').map_or("", |n| &rest[l + n..]);
            }
            (_, Some(b)) => {
                out.push_str(&rest[..b]);
                rest = rest[b..].find("*/").map_or("", |e| &rest[b + e + 2..]);
            }
            _ => {
                out.push_str(rest);
                rest = "";
            }
        }
    }
    out
}

/// Splits the text into headers and `opcode=value` pairs. Values may contain spaces
/// (sample paths): a value runs until the next `name=` or header.
fn tokens(text: &str) -> Vec<(Option<String>, Vec<(String, String)>)> {
    let mut sections: Vec<(Option<String>, Vec<(String, String)>)> = vec![(None, Vec::new())];
    for line in text.lines() {
        let mut rest = line.trim();
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('<') {
                let end = after.find('>').unwrap_or(after.len());
                sections.push((Some(after[..end].trim().to_lowercase()), Vec::new()));
                rest = after.get(end + 1..).unwrap_or("").trim_start();
                continue;
            }
            let Some(eq) = rest.find('=') else {
                break;
            };
            let key = rest[..eq].trim().to_lowercase();
            let after = &rest[eq + 1..];
            // The value ends where the next "word=" or "<" begins.
            let mut end = after.len();
            let bytes = after.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'<' {
                    end = i;
                    break;
                }
                if bytes[i].is_ascii_whitespace() {
                    let word_start = i + 1;
                    let next = after[word_start..]
                        .find(|c: char| c.is_whitespace() || c == '=' || c == '<')
                        .map(|n| word_start + n);
                    if let Some(n) = next {
                        if bytes.get(n) == Some(&b'=') && n > word_start {
                            end = i;
                            break;
                        }
                    }
                }
                i += 1;
            }
            let value = after[..end].trim().to_owned();
            if let Some(s) = sections.last_mut() {
                s.1.push((key, value));
            }
            rest = after[end..].trim_start();
        }
    }
    sections
}

/// Note name or number to a MIDI key ("c4" = 60, "f#3" = 54, "60" = 60).
fn key(v: &str) -> Option<u8> {
    if let Ok(n) = v.parse::<i32>() {
        return u8::try_from(n.clamp(0, 127)).ok();
    }
    let v = v.to_lowercase();
    let mut chars = v.chars();
    let base = match chars.next()? {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let (acc, oct) = if let Some(r) = rest.strip_prefix('#') {
        (1, r)
    } else if let Some(r) = rest.strip_prefix('b') {
        (-1, r)
    } else {
        (0, rest.as_str())
    };
    let octave: i32 = oct.parse().ok()?;
    u8::try_from(((octave + 1) * 12 + base + acc).clamp(0, 127)).ok()
}

fn apply(r: &mut Region, transpose: &mut f32, k: &str, v: &str, dir: &Path, default_path: &str) {
    let f = || v.parse::<f32>().ok();
    match k {
        "sample" => {
            let rel = format!("{default_path}{v}").replace('\\', "/");
            r.sample = dir.join(rel);
        }
        "key" => {
            if let Some(n) = key(v) {
                r.lokey = n;
                r.hikey = n;
                r.pitch_keycenter = f32::from(n);
            }
        }
        "lokey" => r.lokey = key(v).unwrap_or(r.lokey),
        "hikey" => r.hikey = key(v).unwrap_or(r.hikey),
        "lovel" => r.lovel = v.parse().unwrap_or(r.lovel),
        "hivel" => r.hivel = v.parse().unwrap_or(r.hivel),
        "pitch_keycenter" => {
            r.pitch_keycenter = key(v).map_or(r.pitch_keycenter, f32::from);
        }
        "pitch_keytrack" => r.pitch_keytrack = f().unwrap_or(r.pitch_keytrack),
        "tune" | "pitch" => r.tune = f().unwrap_or(r.tune),
        "transpose" => *transpose = f().unwrap_or(*transpose),
        "volume" => r.volume = f().unwrap_or(r.volume),
        "pan" => r.pan = f().unwrap_or(r.pan),
        "amp_veltrack" => r.amp_veltrack = f().unwrap_or(r.amp_veltrack),
        "ampeg_attack" => r.ampeg_attack = f().unwrap_or(r.ampeg_attack),
        "ampeg_hold" => r.ampeg_hold = f().unwrap_or(r.ampeg_hold),
        "ampeg_decay" => r.ampeg_decay = f().unwrap_or(r.ampeg_decay),
        "ampeg_sustain" => r.ampeg_sustain = f().unwrap_or(r.ampeg_sustain),
        "ampeg_release" => r.ampeg_release = f().unwrap_or(r.ampeg_release),
        "offset" => r.offset = v.parse().unwrap_or(r.offset),
        "trigger" => {
            r.trigger = if v == "release" {
                Trigger::Release
            } else {
                Trigger::Attack
            }
        }
        "loop_mode" | "loopmode" => {
            r.loop_mode = match v {
                "one_shot" => LoopMode::OneShot,
                "loop_continuous" => LoopMode::Continuous,
                "loop_sustain" => LoopMode::Sustain,
                _ => LoopMode::NoLoop,
            }
        }
        "loop_start" | "loopstart" => r.loop_start = v.parse().ok(),
        "loop_end" | "loopend" => r.loop_end = v.parse().ok(),
        "seq_length" => r.seq_length = v.parse().unwrap_or(1).max(1),
        "seq_position" => r.seq_position = v.parse().unwrap_or(1).max(1),
        "group" => r.group = v.parse().unwrap_or(0),
        "off_by" => r.off_by = v.parse().unwrap_or(0),
        "lorand" => r.lorand = f().unwrap_or(r.lorand),
        "hirand" => r.hirand = f().unwrap_or(r.hirand),
        _ => {}
    }
}

/// Parses SFZ text whose sample paths are relative to `dir`.
pub fn parse(text: &str, dir: &Path) -> SfzInstrument {
    let mut default_path = String::new();
    // Opcodes of the enclosing headers, innermost last.
    let mut global: Vec<(String, String)> = Vec::new();
    let mut master: Vec<(String, String)> = Vec::new();
    let mut group: Vec<(String, String)> = Vec::new();
    let mut regions = Vec::new();
    for (header, ops) in tokens(&strip_comments(text)) {
        match header.as_deref() {
            Some("control") => {
                for (k, v) in &ops {
                    if k == "default_path" {
                        default_path = v.clone();
                    }
                }
            }
            Some("global") => {
                global = ops;
                master.clear();
                group.clear();
            }
            Some("master") => {
                master = ops;
                group.clear();
            }
            Some("group") => group = ops,
            Some("region") => {
                let mut r = Region::default();
                let mut transpose = 0.0;
                for (k, v) in global.iter().chain(&master).chain(&group).chain(&ops) {
                    apply(&mut r, &mut transpose, k, v, dir, &default_path);
                }
                r.tune += transpose * 100.0;
                if !r.sample.as_os_str().is_empty() {
                    regions.push(r);
                }
            }
            _ => {}
        }
    }
    SfzInstrument { regions }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_inherit_and_values_may_contain_spaces() {
        let text = r#"
            // A comment
            <control> default_path=samples/
            <global> ampeg_release=0.5 /* block
            comment */
            <group> lovel=1 hivel=64 volume=-6
            <region> sample=Piano C4 soft.wav key=c4
            <region> sample=Piano E4 soft.wav lokey=62 hikey=65 pitch_keycenter=e4 tune=-3
            <group> lovel=65 seq_length=2
            <region> sample=a.wav seq_position=2 trigger=release loop_mode=one_shot
        "#;
        let inst = parse(text, Path::new("/lib"));
        assert_eq!(inst.regions.len(), 3);
        let a = &inst.regions[0];
        assert_eq!(a.sample, PathBuf::from("/lib/samples/Piano C4 soft.wav"));
        assert_eq!((a.lokey, a.hikey, a.pitch_keycenter), (60, 60, 60.0));
        assert_eq!(
            (a.lovel, a.hivel, a.volume, a.ampeg_release),
            (1, 64, -6.0, 0.5)
        );
        let b = &inst.regions[1];
        assert_eq!(
            (b.lokey, b.hikey, b.pitch_keycenter, b.tune),
            (62, 65, 64.0, -3.0)
        );
        let c = &inst.regions[2];
        assert_eq!(
            (c.lovel, c.hivel, c.volume),
            (65, 127, 0.0),
            "new group resets"
        );
        assert_eq!((c.seq_length, c.seq_position), (2, 2));
        assert_eq!(
            (c.trigger, c.loop_mode),
            (Trigger::Release, LoopMode::OneShot)
        );
        assert_eq!(inst.samples().len(), 3);
    }

    #[test]
    fn note_names_and_defines() {
        assert_eq!(key("c4"), Some(60));
        assert_eq!(key("f#3"), Some(54));
        assert_eq!(key("Bb2"), Some(46));
        assert_eq!(key("127"), Some(127));
        let dir = std::env::temp_dir().join(format!("musiton-sfz-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("inc.sfz"), "<region> sample=$NAME.wav key=$K\n").unwrap();
        std::fs::write(
            dir.join("main.sfz"),
            "#define $NAME kick\n#define $K 36\n#include \"inc.sfz\"\n",
        )
        .unwrap();
        let inst = read_sfz(&dir.join("main.sfz")).unwrap();
        assert_eq!(inst.regions[0].sample, dir.join("kick.wav"));
        assert_eq!(inst.regions[0].lokey, 36);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
