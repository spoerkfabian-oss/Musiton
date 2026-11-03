//! Typed IDs. Never pass raw indices across thread boundaries.

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
        pub struct $name(pub u64);
    };
}

id_type!(
    /// Identifies a parameter of a node/device.
    ParamId
);
