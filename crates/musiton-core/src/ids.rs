//! Typed IDs. Never pass raw indices across thread boundaries.

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default,
            serde::Serialize, serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);
    };
}

id_type!(
    /// Identifies a parameter of a node/device.
    ParamId
);

id_type!(
    /// Identifies a track.
    TrackId
);

id_type!(
    /// Identifies a clip.
    ClipId
);

id_type!(
    /// Identifies an audio file in the project's asset pool.
    AssetId
);

id_type!(
    /// Identifies a marker.
    MarkerId
);

id_type!(
    /// Identifies a device (instrument/effect) in a track's chain.
    DeviceId
);
