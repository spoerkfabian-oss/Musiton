//! Genre environments: which built-in instruments and factory presets fit a genre. A song
//! can be set to one (templates do it), and the app then lists only those, so a Raw song
//! shows no orchestra. "Init" presets, the sampler and the sample instrument fit every
//! genre.

/// One genre environment.
#[derive(Debug, Clone, Copy)]
pub struct Genre {
    /// Stable id (stored in the project).
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Instrument type ids that fit (the sampler and the sample instrument always do).
    pub instruments: &'static [&'static str],
    /// Factory preset names that fit ("Init" always does).
    pub presets: &'static [&'static str],
}

impl Genre {
    /// Whether instrument `type_id` belongs to this genre.
    pub fn has_instrument(&self, type_id: &str) -> bool {
        type_id == crate::SAMPLER || type_id == crate::SFZ || self.instruments.contains(&type_id)
    }

    /// Whether the factory preset `name` belongs to this genre.
    pub fn has_preset(&self, name: &str) -> bool {
        name == "Init" || self.presets.contains(&name)
    }
}

/// All genre environments, in menu order.
pub const GENRES: &[Genre] = &[
    Genre {
        id: "raw",
        name: "Raw",
        instruments: &["polysynth", "wavetable", "drums"],
        presets: &[
            "Raw stab",
            "Raw reese",
            "Dirty sub",
            "Supersaw",
            "Bass",
            "Talking lead",
            "Pulse bass",
            "Punchy",
            "808 style",
        ],
    },
    Genre {
        id: "phonk",
        name: "Phonk",
        instruments: &["polysynth", "drums", "fm", "wavetable"],
        presets: &[
            "Phonk cowbell",
            "Phonk 808",
            "Dirty sub",
            "Bell",
            "Talking lead",
            "Phonk",
            "808 style",
            "Lo-fi",
        ],
    },
    Genre {
        id: "tekk",
        name: "Tekk",
        instruments: &["polysynth", "drums", "wavetable", "acid"],
        presets: &[
            "Tekk lead",
            "Supersaw",
            "Hyper supersaw",
            "Bass",
            "Raw reese",
            "Screamer",
            "Tekk",
            "Punchy",
        ],
    },
    Genre {
        id: "trap",
        name: "Trap / hip-hop",
        instruments: &[
            "polysynth",
            "drums",
            "fm",
            "epiano",
            "pluck",
            "strings",
            "wavetable",
        ],
        presets: &[
            "Phonk 808",
            "Dirty sub",
            "Pluck",
            "Warm pad",
            "Bell",
            "Mellow",
            "Nylon guitar",
            "Dark pad",
            "Digital bells",
            "808 style",
            "Lo-fi",
        ],
    },
    Genre {
        id: "house",
        name: "House",
        instruments: &[
            "polysynth",
            "drums",
            "epiano",
            "organ",
            "fm",
            "acid",
            "strings",
        ],
        presets: &[
            "Bass",
            "Warm pad",
            "Pluck",
            "Suitcase",
            "Mellow",
            "Bright",
            "Jazz",
            "Gospel",
            "E-piano",
            "Deep square",
            "Lush strings",
            "House",
            "Punchy",
        ],
    },
    Genre {
        id: "techno",
        name: "Techno",
        instruments: &["polysynth", "drums", "acid", "wavetable", "strings", "fm"],
        presets: &[
            "Bass",
            "Warm pad",
            "Supersaw",
            "Squelch",
            "Deep square",
            "Screamer",
            "Sweep pad",
            "Pulse bass",
            "Dark pad",
            "Bell",
            "Punchy",
            "House",
        ],
    },
    Genre {
        id: "hyperpop",
        name: "Hyperpop",
        instruments: &["polysynth", "drums", "pluck", "wavetable", "fm"],
        presets: &[
            "Hyper supersaw",
            "Supersaw",
            "Phonk 808",
            "Dirty sub",
            "Pluck",
            "Steel strings",
            "Digital bells",
            "Talking lead",
            "Bell",
            "Punchy",
            "808 style",
        ],
    },
    Genre {
        id: "hardstyle",
        name: "Hardstyle",
        instruments: &["polysynth", "drums", "wavetable"],
        presets: &[
            "Hyper supersaw",
            "Supersaw",
            "Raw reese",
            "Tekk lead",
            "Sweep pad",
            "Tekk",
            "Punchy",
        ],
    },
    Genre {
        id: "edm",
        name: "EDM",
        instruments: &[
            "polysynth",
            "drums",
            "epiano",
            "pluck",
            "strings",
            "wavetable",
            "fm",
        ],
        presets: &[
            "EDM bass",
            "Big room lead",
            "Melodic pluck",
            "Hard lead",
            "Riser",
            "Supersaw",
            "Hyper supersaw",
            "Warm pad",
            "Pluck",
            "Bass",
            "Phonk 808",
            "Lush strings",
            "Dark pad",
            "Bright",
            "Mellow",
            "Sweep pad",
            "Digital bells",
            "Big room",
            "Punchy",
            "House",
        ],
    },
    Genre {
        id: "orchestral",
        name: "Orchestral / choir",
        instruments: &["strings", "pluck", "organ", "epiano", "fm", "drums"],
        presets: &[
            "Lush strings",
            "Dark pad",
            "Bright violins",
            "Harp",
            "Pizzicato",
            "Nylon guitar",
            "Full organ",
            "Soft flute",
            "Gospel",
            "Mellow",
            "Brass",
            "Bell",
        ],
    },
];

/// The genre with `id`.
pub fn genre(id: &str) -> Option<&'static Genre> {
    GENRES.iter().find(|g| g.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_instrument_and_preset_exists() {
        for g in GENRES {
            for &t in g.instruments {
                assert!(
                    crate::instrument_type(t).is_some(),
                    "{}: no instrument {t}",
                    g.id
                );
            }
            for &p in g.presets {
                let found = crate::INSTRUMENTS
                    .iter()
                    .filter(|t| g.has_instrument(t.type_id))
                    .any(|t| t.presets.iter().any(|(n, _)| *n == p));
                assert!(found, "{}: no preset {p} on its instruments", g.id);
            }
        }
    }

    #[test]
    fn ids_are_unique() {
        for (i, a) in GENRES.iter().enumerate() {
            assert!(GENRES[i + 1..].iter().all(|b| b.id != a.id));
        }
    }
}
