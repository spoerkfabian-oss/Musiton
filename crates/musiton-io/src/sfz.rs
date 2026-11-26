//! Reading SFZ instrument files (the text format most free sample libraries use).
//!
//! Supported: `<control>` (`default_path`, `set_ccN`, `set_hdccN`), `<global>`, `<master>`,
//! `<group>` and `<region>` headers with inheritance, `#define $NAME value` and
//! `#include "file"` anywhere in the text (in order, as in sforzando), comments, key
//! switches (`sw_lokey`/`sw_hikey`/`sw_default`/`sw_last`), CC conditions
//! (`loccN`/`hiccN`) and the opcodes Musiton's sample player understands (see
//! [`Region`]). Unknown opcodes are ignored.

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
    /// Plays only while this key switch is the last one pressed.
    pub sw_last: Option<u8>,
    /// Plays only while each of these controllers is within its range: (cc, lo, hi).
    pub cc_ranges: Vec<(u8, f32, f32)>,
    /// Last sample frame played (`end`).
    pub end: Option<u64>,
    /// Velocity curve points `amp_velcurve_N` (velocity, gain 0 … 1), sorted.
    pub amp_velcurve: Vec<(u8, f32)>,
    /// Controller modulation (`amplitude_oncc`, `volume_oncc`, `ampeg_*_oncc`, `offset_oncc`).
    pub cc_mods: Vec<CcMod>,
    /// Controller crossfades: (cc, lo, hi, fades in) from `xfin_*cc` / `xfout_*cc`.
    pub crossfades: Vec<(u8, f32, f32, bool)>,
    /// dB per second held that a release sample gets quieter (`rt_decay`).
    pub rt_decay: f32,
}

/// What a controller modulates in a region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CcTarget {
    /// Gain in percent (several multiply).
    Amplitude,
    /// dB.
    Volume,
    /// Start offset in sample frames.
    Offset,
    /// Envelope attack (seconds).
    Attack,
    /// Envelope hold (seconds).
    Hold,
    /// Envelope decay (seconds).
    Decay,
    /// Envelope sustain (percent).
    Sustain,
    /// Envelope release (seconds).
    Release,
}

/// `depth` at controller value 127 for `target` (linear; curves are not read).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CcMod {
    /// What changes.
    pub target: CcTarget,
    /// Controller number.
    pub cc: u8,
    /// Amount at controller value 127.
    pub depth: f32,
}

/// Splits `name123` into (`name`, 123).
fn trailing_number(k: &str) -> Option<(&str, u8)> {
    let digits = k.len() - k.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return None;
    }
    let (name, n) = k.split_at(k.len() - digits);
    Some((name, n.parse().ok()?))
}

/// Reads a controller opcode (modulation, crossfade or velocity curve point).
fn controller_opcode(r: &mut Region, k: &str, x: f32) -> bool {
    let Some((name, n)) = trailing_number(k) else {
        return false;
    };
    let target = match name {
        "amplitude_cc" | "amplitude_oncc" => Some(CcTarget::Amplitude),
        "volume_cc" | "volume_oncc" | "gain_cc" | "gain_oncc" => Some(CcTarget::Volume),
        "offset_cc" | "offset_oncc" => Some(CcTarget::Offset),
        "ampeg_attackcc" | "ampeg_attack_oncc" => Some(CcTarget::Attack),
        "ampeg_holdcc" | "ampeg_hold_oncc" => Some(CcTarget::Hold),
        "ampeg_decaycc" | "ampeg_decay_oncc" => Some(CcTarget::Decay),
        "ampeg_sustaincc" | "ampeg_sustain_oncc" => Some(CcTarget::Sustain),
        "ampeg_releasecc" | "ampeg_release_oncc" => Some(CcTarget::Release),
        _ => None,
    };
    if let Some(target) = target {
        let m = CcMod {
            target,
            cc: n,
            depth: x,
        };
        match r
            .cc_mods
            .iter_mut()
            .find(|c| c.target == target && c.cc == n)
        {
            Some(c) => *c = m,
            None => r.cc_mods.push(m),
        }
        return true;
    }
    if name == "amp_velcurve_" {
        r.amp_velcurve.retain(|p| p.0 != n);
        r.amp_velcurve.push((n.min(127), x.clamp(0.0, 1.0)));
        r.amp_velcurve.sort_by_key(|p| p.0);
        return true;
    }
    let fade = match name {
        "xfin_locc" => Some((true, true)),
        "xfin_hicc" => Some((true, false)),
        "xfout_locc" => Some((false, true)),
        "xfout_hicc" => Some((false, false)),
        _ => None,
    };
    if let Some((fade_in, low)) = fade {
        let i = match r.crossfades.iter().position(|c| c.0 == n && c.3 == fade_in) {
            Some(i) => i,
            None => {
                // Unset ends: a fade-in is complete at 0, a fade-out starts at 127.
                let (lo, hi) = if fade_in { (0.0, 0.0) } else { (127.0, 127.0) };
                r.crossfades.push((n, lo, hi, fade_in));
                r.crossfades.len() - 1
            }
        };
        if let Some(c) = r.crossfades.get_mut(i) {
            if low {
                c.1 = x;
            } else {
                c.2 = x;
            }
        }
        return true;
    }
    false
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
            sw_last: None,
            cc_ranges: Vec::new(),
            end: None,
            amp_velcurve: Vec::new(),
            cc_mods: Vec::new(),
            crossfades: Vec::new(),
            rt_decay: 0.0,
        }
    }
}

/// A parsed SFZ instrument.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SfzInstrument {
    /// Every region with a sample, in file order.
    pub regions: Vec<Region>,
    /// Keys that act as key switches (inclusive).
    pub sw_range: Option<(u8, u8)>,
    /// Key switch in effect before one is pressed.
    pub sw_default: Option<u8>,
    /// Controller start values (cc, 0 … 127).
    pub cc_defaults: Vec<(u8, f32)>,
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
    let dir = path.parent().unwrap_or(Path::new("."));
    load(path, dir, &mut text, &mut defines, 0)?;
    let inst = parse(&text, dir);
    if inst.regions.is_empty() {
        return Err(SfzError::Empty(path.to_owned()));
    }
    Ok(inst)
}

/// Reads `path` into `out`, resolving `#include` and `#define` wherever they appear, in
/// order: a name is replaced only in the text after its definition, so files can
/// redefine it between includes.
fn load(
    path: &Path,
    root: &Path,
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
    let text = strip_comments(&String::from_utf8_lossy(&raw));
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut rest = text.as_str();
    loop {
        let next = ["#define", "#include"]
            .iter()
            .filter_map(|d| rest.find(d).map(|i| (i, *d)))
            .min();
        let Some((at, directive)) = next else {
            out.push_str(&substitute(rest, defines));
            out.push('\n');
            return Ok(());
        };
        out.push_str(&substitute(&rest[..at], defines));
        out.push(' ');
        let after = rest[at + directive.len()..].trim_start_matches([' ', '\t']);
        if directive == "#define" {
            let mut words = after.splitn(3, |c: char| c.is_whitespace());
            let name = words.next().unwrap_or("");
            let value = words.next().unwrap_or("");
            if !name.is_empty() {
                defines.insert(name.to_owned(), value.to_owned());
            }
            rest = words.next().unwrap_or("");
        } else {
            let (name, tail) = match after.strip_prefix('"') {
                Some(q) => q.split_once('"').unwrap_or((q, "")),
                None => after.split_once(char::is_whitespace).unwrap_or((after, "")),
            };
            // Paths are relative to the main file (as in sforzando); a file next to the
            // including one is found too.
            let main = root.join(name);
            let target = if main.exists() { main } else { dir.join(name) };
            load(&target, root, out, defines, depth + 1)?;
            rest = tail;
        }
    }
}

/// Replaces defined `$NAMES` in `text` (longest names first, so `$A` does not replace
/// inside `$AB`).
fn substitute(text: &str, defines: &HashMap<String, String>) -> String {
    let mut keys: Vec<&String> = defines.keys().collect();
    keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
    let mut out = text.to_owned();
    for k in keys {
        if let Some(v) = defines.get(k) {
            out = out.replace(k.as_str(), v);
        }
    }
    out
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
/// A header (`None` before the first one) and its `opcode=value` pairs.
type Section = (Option<String>, Vec<(String, String)>);

fn tokens(text: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = vec![(None, Vec::new())];
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
        return u8::try_from(n.clamp(0, 127)).ok().filter(|_| n >= 0);
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
        // A negative key (e.g. `lokey=-1`) means "no keys": the region is not played by
        // notes (it is meant for controller triggers, which Musiton does not support).
        "lokey" => r.lokey = key(v).unwrap_or(if v.starts_with('-') { 127 } else { r.lokey }),
        "hikey" => r.hikey = key(v).unwrap_or(if v.starts_with('-') { 0 } else { r.hikey }),
        "sw_last" => r.sw_last = key(v),
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
        "end" => r.end = v.parse().ok(),
        "rt_decay" => r.rt_decay = f().unwrap_or(r.rt_decay),
        _ if f().is_some_and(|x| controller_opcode(r, k, x)) => {}
        _ => {
            let range = k
                .strip_prefix("locc")
                .map(|n| (n, true))
                .or_else(|| k.strip_prefix("hicc").map(|n| (n, false)));
            if let (Some((n, low)), Some(x)) = (range, f()) {
                if let Ok(cc) = n.parse::<u8>() {
                    let i = match r.cc_ranges.iter().position(|c| c.0 == cc) {
                        Some(i) => i,
                        None => {
                            r.cc_ranges.push((cc, 0.0, 127.0));
                            r.cc_ranges.len() - 1
                        }
                    };
                    if let Some(c) = r.cc_ranges.get_mut(i) {
                        if low {
                            c.1 = x;
                        } else {
                            c.2 = x;
                        }
                    }
                }
            }
        }
    }
}

/// Parses SFZ text whose sample paths are relative to `dir`.
pub fn parse(text: &str, dir: &Path) -> SfzInstrument {
    let mut default_path = String::new();
    // Opcodes of the enclosing headers, innermost last.
    let mut global: Vec<(String, String)> = Vec::new();
    let mut master: Vec<(String, String)> = Vec::new();
    let mut group: Vec<(String, String)> = Vec::new();
    let mut inst = SfzInstrument::default();
    for (header, ops) in tokens(&strip_comments(text)) {
        // Instrument-wide settings, wherever they appear.
        for (k, v) in &ops {
            match k.as_str() {
                "sw_lokey" => {
                    let hi = inst.sw_range.map_or(0, |r| r.1);
                    inst.sw_range = key(v).map(|lo| (lo, hi.max(lo)));
                }
                "sw_hikey" => {
                    let lo = inst.sw_range.map_or(0, |r| r.0);
                    inst.sw_range = key(v).map(|hi| (lo.min(hi), hi));
                }
                "sw_default" => inst.sw_default = key(v),
                _ => {}
            }
        }
        match header.as_deref() {
            Some("control") => {
                for (k, v) in &ops {
                    if k == "default_path" {
                        default_path = v.clone();
                    }
                    let cc = k
                        .strip_prefix("set_hdcc")
                        .map(|n| (n, 127.0))
                        .or_else(|| k.strip_prefix("set_cc").map(|n| (n, 1.0)));
                    if let (Some((n, scale)), Ok(x)) = (cc, v.parse::<f32>()) {
                        if let Ok(cc) = n.parse::<u8>() {
                            inst.cc_defaults.push((cc, x * scale));
                        }
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
                    inst.regions.push(r);
                }
            }
            _ => {}
        }
    }
    inst
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
    fn controller_modulation_crossfades_and_velocity_curves() {
        let text = r#"
            <group> amplitude_oncc30=100 xfin_locc1=0 xfin_hicc1=64 amp_velcurve_64=1
                    ampeg_sustain_oncc4=50 end=999 rt_decay=6
            <region> sample=a.wav amplitude_oncc30=80 amplitude_cc31=50 xfout_locc1=100
        "#;
        let inst = parse(text, Path::new("/lib"));
        let r = &inst.regions[0];
        let m = |t, cc| {
            r.cc_mods
                .iter()
                .find(|m| m.target == t && m.cc == cc)
                .map(|m| m.depth)
        };
        assert_eq!(
            m(CcTarget::Amplitude, 30),
            Some(80.0),
            "the region overrides its group"
        );
        assert_eq!(m(CcTarget::Amplitude, 31), Some(50.0));
        assert_eq!(m(CcTarget::Sustain, 4), Some(50.0));
        assert_eq!(
            r.crossfades,
            vec![(1, 0.0, 64.0, true), (1, 100.0, 127.0, false)]
        );
        assert_eq!(r.amp_velcurve, vec![(64, 1.0)]);
        assert_eq!((r.end, r.rt_decay), (Some(999), 6.0));
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

    #[test]
    fn includes_mid_line_redefine_in_order_from_the_main_folder() {
        // Like the Salamander piano: includes on a header line, a name redefined between
        // includes, include paths relative to the main file.
        let dir = std::env::temp_dir().join(format!("musiton-sfz-order-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Data")).unwrap();
        std::fs::write(dir.join("Data/v1.txt"), "#define $VEL v1\n").unwrap();
        std::fs::write(dir.join("Data/v2.txt"), "#define $VEL v2\n").unwrap();
        std::fs::write(
            dir.join("Data/region.txt"),
            "<region> sample=C4$VEL.wav key=60\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("Data/notes.txt"),
            "<group> #include \"Data/v1.txt\" lovel=1 hivel=64 #include \"Data/region.txt\"\n\
             <group> #include \"Data/v2.txt\" lovel=65 hivel=127 #include \"Data/region.txt\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("main.sfz"),
            "<global> sw_lokey=c0 sw_hikey=c#0 sw_default=c0\n\
             <control> set_hdcc20=0.5 set_cc7=100\n\
             <master> sw_last=c0 #include \"Data/notes.txt\"\n\
             <master> locc20=1 hicc20=100 lokey=-1 hikey=-1\n<region> sample=noise.wav\n",
        )
        .unwrap();
        let inst = read_sfz(&dir.join("main.sfz")).unwrap();
        let names: Vec<String> = inst
            .regions
            .iter()
            .map(|r| r.sample.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["C4v1.wav", "C4v2.wav", "noise.wav"]);
        assert_eq!((inst.regions[0].lovel, inst.regions[1].lovel), (1, 65));
        assert_eq!(inst.regions[0].sw_last, Some(12));
        assert_eq!(inst.sw_range, Some((12, 13)));
        assert_eq!(inst.sw_default, Some(12));
        assert_eq!(inst.cc_defaults, vec![(20, 63.5), (7, 100.0)]);
        let noise = &inst.regions[2];
        assert_eq!(noise.cc_ranges, vec![(20, 1.0, 100.0)]);
        assert!(noise.lokey > noise.hikey, "negative keys: no notes play it");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
