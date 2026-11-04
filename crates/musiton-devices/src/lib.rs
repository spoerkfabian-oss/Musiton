//! Built-in instruments and effects for Musiton (they implement `musiton_engine::node::AudioNode`).

pub mod effects;
pub mod polysynth;
pub mod sampler;

use std::sync::Arc;

use musiton_core::audio::AudioData;
use musiton_core::ids::ParamId;
use musiton_core::params::ParamInfo;
use musiton_engine::node::AudioNode;

/// Type id of the polysynth.
pub const POLYSYNTH: &str = "polysynth";
/// Type id of the sampler.
pub const SAMPLER: &str = "sampler";

/// A built-in instrument type.
#[derive(Debug, Clone, Copy)]
pub struct InstrumentType {
    /// Stable id stored in projects.
    pub type_id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Its parameters.
    pub params: &'static [ParamInfo],
    /// Whether it plays audio samples.
    pub uses_samples: bool,
    /// Factory presets: (name, parameter values).
    pub presets: &'static [(&'static str, &'static [(u64, f32)])],
}

/// All built-in instruments.
pub const INSTRUMENTS: [InstrumentType; 2] = [
    InstrumentType {
        type_id: POLYSYNTH,
        name: "Polysynth",
        params: &polysynth::PARAMS,
        uses_samples: false,
        presets: polysynth::PRESETS,
    },
    InstrumentType {
        type_id: SAMPLER,
        name: "Sampler",
        params: &sampler::PARAMS,
        uses_samples: true,
        presets: &[],
    },
];

/// A built-in effect type.
#[derive(Debug, Clone, Copy)]
pub struct EffectType {
    /// Stable id stored in projects.
    pub type_id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Its parameters.
    pub params: &'static [ParamInfo],
}

/// All built-in effects.
pub const EFFECTS: [EffectType; 7] = [
    EffectType {
        type_id: "eq",
        name: "EQ",
        params: &effects::eq::PARAMS,
    },
    EffectType {
        type_id: "compressor",
        name: "Compressor",
        params: &effects::compressor::PARAMS,
    },
    EffectType {
        type_id: "delay",
        name: "Delay",
        params: &effects::delay::PARAMS,
    },
    EffectType {
        type_id: "reverb",
        name: "Reverb",
        params: &effects::reverb::PARAMS,
    },
    EffectType {
        type_id: "saturation",
        name: "Saturation",
        params: &effects::saturation::PARAMS,
    },
    EffectType {
        type_id: "limiter",
        name: "Limiter",
        params: &effects::limiter::PARAMS,
    },
    EffectType {
        type_id: "utility",
        name: "Utility",
        params: &effects::utility::PARAMS,
    },
];

/// Looks up an effect type.
pub fn effect_type(type_id: &str) -> Option<&'static EffectType> {
    EFFECTS.iter().find(|t| t.type_id == type_id)
}

/// Creates an effect (off the audio thread). The EQ also returns the analyzer its output
/// feeds (for the spectrum display). Unknown types give a utility.
pub fn create_effect(
    type_id: &str,
    params: &[(u64, f32)],
) -> (
    Box<dyn AudioNode>,
    Option<Arc<musiton_engine::nodes::Analyzer>>,
) {
    use effects::*;
    let mut analyzer = None;
    let mut node: Box<dyn AudioNode> = match type_id {
        "eq" => {
            let a = musiton_engine::nodes::Analyzer::new();
            analyzer = Some(a.clone());
            Box::new(eq::Eq::new(a))
        }
        "compressor" => Box::<compressor::Compressor>::default(),
        "delay" => Box::<delay::Delay>::default(),
        "reverb" => Box::<reverb::Reverb>::default(),
        "saturation" => Box::<saturation::Saturation>::default(),
        "limiter" => Box::<limiter::Limiter>::default(),
        _ => Box::<utility::Utility>::default(),
    };
    for &(id, value) in params {
        node.set_param(ParamId(id), value);
    }
    (node, analyzer)
}

/// Looks up an instrument type.
pub fn instrument_type(type_id: &str) -> Option<&'static InstrumentType> {
    INSTRUMENTS.iter().find(|t| t.type_id == type_id)
}

/// Creates an instrument (off the audio thread). Unknown types give a polysynth.
/// `samples` are (key, audio) pairs: the root sample, or one per pad in drum mode.
pub fn create_instrument(
    type_id: &str,
    params: &[(u64, f32)],
    samples: &[(u8, Arc<AudioData>)],
) -> Box<dyn AudioNode> {
    let mut node: Box<dyn AudioNode> = match type_id {
        SAMPLER => Box::new(sampler::Sampler::new(
            samples
                .iter()
                .map(|(k, a)| sampler::Zone::new(*k, a))
                .collect(),
        )),
        _ => Box::new(polysynth::Polysynth::new()),
    };
    for &(id, value) in params {
        node.set_param(ParamId(id), value);
    }
    node
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_ids_match_param_indices() {
        for t in INSTRUMENTS {
            for (i, p) in t.params.iter().enumerate() {
                assert_eq!(p.id.0, i as u64, "{} param {}", t.name, p.name);
                assert!(p.id.0 < 100, "ids ≥ 100 are reserved for the track");
            }
            for (name, values) in t.presets {
                assert!(
                    values.iter().all(|(id, _)| (*id as usize) < t.params.len()),
                    "{name}"
                );
            }
        }
        for t in EFFECTS {
            for (i, p) in t.params.iter().enumerate() {
                assert_eq!(p.id.0, i as u64, "{} param {}", t.name, p.name);
            }
            let (n, a) = create_effect(t.type_id, &[]);
            assert_eq!(n.num_outputs(), 2);
            assert_eq!(a.is_some(), t.type_id == "eq");
        }
        assert!(instrument_type("nope").is_none());
        let n = create_instrument(SAMPLER, &[(sampler::id::DRUM, 1.0)], &[]);
        assert_eq!(n.num_outputs(), 2);
    }
}
