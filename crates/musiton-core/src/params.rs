//! Parameter descriptions shared by the engine (devices) and the UI (knobs).

use crate::ids::ParamId;

/// How a parameter is shown and edited.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamKind {
    /// Continuous, linear knob.
    Linear,
    /// Continuous, logarithmic knob (frequencies, times).
    Log,
    /// Whole numbers.
    Integer,
    /// On/off.
    Toggle,
    /// One of several named options (value = index).
    Choice(&'static [&'static str]),
}

/// Describes one parameter of a node/device. Values are plain (e.g. Hz, seconds).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamInfo {
    /// Parameter id (unique within the node).
    pub id: ParamId,
    /// Display name.
    pub name: &'static str,
    /// Minimum value.
    pub min: f32,
    /// Maximum value.
    pub max: f32,
    /// Default value.
    pub default: f32,
    /// Knob behaviour.
    pub kind: ParamKind,
    /// Unit shown after the value ("Hz", "s", "dB", "st"…).
    pub unit: &'static str,
}

impl ParamInfo {
    /// A linear parameter.
    pub const fn new(id: u64, name: &'static str, min: f32, max: f32, default: f32) -> Self {
        Self {
            id: ParamId(id),
            name,
            min,
            max,
            default,
            kind: ParamKind::Linear,
            unit: "",
        }
    }

    /// Makes it logarithmic.
    pub const fn log(mut self) -> Self {
        self.kind = ParamKind::Log;
        self
    }

    /// Makes it integer-valued.
    pub const fn int(mut self) -> Self {
        self.kind = ParamKind::Integer;
        self
    }

    /// An on/off switch (0/1).
    pub const fn toggle(id: u64, name: &'static str, default: bool) -> Self {
        let mut p = Self::new(id, name, 0.0, 1.0, if default { 1.0 } else { 0.0 });
        p.kind = ParamKind::Toggle;
        p
    }

    /// A choice between named options.
    pub const fn choice(
        id: u64,
        name: &'static str,
        options: &'static [&'static str],
        default: usize,
    ) -> Self {
        let mut p = Self::new(id, name, 0.0, options.len() as f32 - 1.0, default as f32);
        p.kind = ParamKind::Choice(options);
        p
    }

    /// Sets the unit.
    pub const fn unit(mut self, unit: &'static str) -> Self {
        self.unit = unit;
        self
    }

    /// Maps a knob position 0..1 to a value.
    pub fn from_normalized(&self, x: f32) -> f32 {
        let x = x.clamp(0.0, 1.0);
        let v = match self.kind {
            ParamKind::Log if self.min > 0.0 => self.min * (self.max / self.min).powf(x),
            _ => self.min + (self.max - self.min) * x,
        };
        match self.kind {
            ParamKind::Integer | ParamKind::Choice(_) | ParamKind::Toggle => v.round(),
            _ => v,
        }
    }

    /// Maps a value to a knob position 0..1.
    pub fn to_normalized(&self, v: f32) -> f32 {
        let v = v.clamp(self.min.min(self.max), self.max.max(self.min));
        let x = match self.kind {
            ParamKind::Log if self.min > 0.0 => (v / self.min).ln() / (self.max / self.min).ln(),
            _ => (v - self.min) / (self.max - self.min),
        };
        if x.is_finite() {
            x.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Value as text, e.g. "1.20 kHz", "Saw", "On".
    pub fn format(&self, v: f32) -> String {
        match self.kind {
            ParamKind::Toggle => if v > 0.5 { "On" } else { "Off" }.to_owned(),
            ParamKind::Choice(opts) => opts
                .get(v.round().max(0.0) as usize)
                .copied()
                .unwrap_or("?")
                .to_owned(),
            ParamKind::Integer => format!("{} {}", v.round() as i64, self.unit)
                .trim_end()
                .to_owned(),
            _ if self.unit == "Hz" && v >= 1000.0 => format!("{:.2} kHz", v / 1000.0),
            _ if self.unit == "s" && v < 1.0 => format!("{:.0} ms", v * 1000.0),
            _ => {
                let digits = if v.abs() >= 100.0 {
                    0
                } else if v.abs() >= 10.0 {
                    1
                } else {
                    2
                };
                format!("{v:.digits$} {}", self.unit).trim_end().to_owned()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_mapping_round_trips() {
        let p = ParamInfo::new(0, "Cutoff", 20.0, 20_000.0, 1000.0)
            .log()
            .unit("Hz");
        assert!((p.from_normalized(0.5) - 632.46).abs() < 0.1);
        assert!((p.to_normalized(p.from_normalized(0.3)) - 0.3).abs() < 1e-5);
        assert_eq!(p.format(1200.0), "1.20 kHz");
        let c = ParamInfo::choice(1, "Wave", &["Saw", "Square"], 0);
        assert_eq!(c.from_normalized(0.8), 1.0);
        assert_eq!(c.format(1.0), "Square");
        assert_eq!(
            ParamInfo::new(2, "A", 0.0, 2.0, 0.0).unit("s").format(0.25),
            "250 ms"
        );
    }
}
