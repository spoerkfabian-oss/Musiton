//! Snap grid.

use musiton_core::{Tick, TimeSig, PPQ};

/// Grid that positions snap to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Snap {
    /// No snapping.
    Off,
    /// Whole bars (follows the time signature).
    #[default]
    Bar,
    /// Note value 1/`n` (1, 2, 4, 8, 16, 32), optionally triplet.
    Note {
        /// Denominator.
        n: u8,
        /// Triplet (2/3 of the length).
        triplet: bool,
    },
}

impl Snap {
    /// All choices offered in the UI.
    pub const ALL: [Snap; 14] = [
        Snap::Off,
        Snap::Bar,
        Snap::Note {
            n: 1,
            triplet: false,
        },
        Snap::Note {
            n: 2,
            triplet: false,
        },
        Snap::Note {
            n: 4,
            triplet: false,
        },
        Snap::Note {
            n: 8,
            triplet: false,
        },
        Snap::Note {
            n: 16,
            triplet: false,
        },
        Snap::Note {
            n: 32,
            triplet: false,
        },
        Snap::Note {
            n: 2,
            triplet: true,
        },
        Snap::Note {
            n: 4,
            triplet: true,
        },
        Snap::Note {
            n: 8,
            triplet: true,
        },
        Snap::Note {
            n: 16,
            triplet: true,
        },
        Snap::Note {
            n: 32,
            triplet: true,
        },
        Snap::Note {
            n: 1,
            triplet: true,
        },
    ];

    /// Grid step in ticks (`None` when off).
    pub fn step(self, sig: TimeSig) -> Option<i64> {
        match self {
            Snap::Off => None,
            Snap::Bar => Some(sig.ticks_per_bar()),
            Snap::Note { n, triplet } => {
                let len = PPQ * 4 / i64::from(n.max(1));
                Some(if triplet { len * 2 / 3 } else { len })
            }
        }
    }

    /// Rounds `tick` to the nearest grid line.
    pub fn round(self, tick: f64, sig: TimeSig) -> Tick {
        match self.step(sig) {
            Some(step) => Tick(((tick / step as f64).round() as i64) * step),
            None => Tick(tick.round() as i64),
        }
    }

    /// Rounds `tick` down to the grid line at or before it.
    pub fn floor(self, tick: f64, sig: TimeSig) -> Tick {
        match self.step(sig) {
            Some(step) => Tick(((tick / step as f64).floor() as i64) * step),
            None => Tick(tick.floor() as i64),
        }
    }

    /// Text for menus ("1/8T").
    pub fn label(self) -> String {
        match self {
            Snap::Off => "Off".into(),
            Snap::Bar => "Bar".into(),
            Snap::Note { n, triplet } => format!("1/{n}{}", if triplet { "T" } else { "" }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps() {
        let s44 = TimeSig::default();
        assert_eq!(Snap::Off.step(s44), None);
        assert_eq!(Snap::Bar.step(s44), Some(3840));
        assert_eq!(Snap::Bar.step(TimeSig::new(3, 4)), Some(2880));
        assert_eq!(
            Snap::Note {
                n: 4,
                triplet: false
            }
            .step(s44),
            Some(960)
        );
        assert_eq!(
            Snap::Note {
                n: 8,
                triplet: true
            }
            .step(s44),
            Some(320)
        );
        assert_eq!(
            Snap::Note {
                n: 32,
                triplet: false
            }
            .step(s44),
            Some(120)
        );
    }

    #[test]
    fn rounding() {
        let s = TimeSig::default();
        let q = Snap::Note {
            n: 4,
            triplet: false,
        };
        assert_eq!(q.round(1400.0, s), Tick(960));
        assert_eq!(q.round(1500.0, s), Tick(1920));
        assert_eq!(q.floor(1919.0, s), Tick(960));
        assert_eq!(Snap::Off.round(12.6, s), Tick(13));
        assert_eq!(q.round(-500.0, s), Tick(-960));
        assert_eq!(
            Snap::Note {
                n: 8,
                triplet: true
            }
            .label(),
            "1/8T"
        );
    }
}
