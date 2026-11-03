//! Undoable edits. Every change to a [`Project`] made by the user goes through an [`Edit`].

use musiton_core::ids::{ClipId, DeviceId, MarkerId, TrackId};
use musiton_core::{TempoMap, Tick, TimeSig};

use crate::{
    Asset, ChannelStrip, Clip, DeviceSlot, Marker, OutputRoute, Project, RecordSetup, Track,
};

/// Most undo steps kept.
const MAX_UNDO: usize = 1000;

/// An edit failed because its target no longer exists or an index is invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    /// No track with this id.
    #[error("track not found")]
    NoTrack,
    /// No clip with this id.
    #[error("clip not found")]
    NoClip,
    /// No device with this id.
    #[error("device not found")]
    NoDevice,
    /// No marker with this id.
    #[error("marker not found")]
    NoMarker,
    /// Index out of range.
    #[error("invalid position")]
    BadIndex,
    /// The edit was already applied (internal error).
    #[error("edit is in the wrong state")]
    State,
}

/// A reversible change. "Set" edits swap the stored value with the project's, so applying
/// and reverting are the same operation; "Add"/"Remove" edits hold the moved object.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    /// Insert a track at `index`.
    AddTrack {
        /// Position in the track list.
        index: usize,
        /// The track (held while not in the project).
        track: Option<Track>,
    },
    /// Remove a track.
    RemoveTrack {
        /// Which track.
        id: TrackId,
        /// Where it was and the track itself (filled by apply).
        removed: Option<(usize, Track)>,
    },
    /// Move a track from one position to another.
    MoveTrack {
        /// Old index.
        from: usize,
        /// New index.
        to: usize,
    },
    /// Rename a track.
    RenameTrack {
        /// Which track.
        id: TrackId,
        /// New (after apply: old) name.
        name: String,
    },
    /// Recolour a track.
    SetTrackColor {
        /// Which track.
        id: TrackId,
        /// New (after apply: old) colour.
        color: [u8; 3],
    },
    /// Change volume/pan/mute/solo of a track (`None` = master).
    SetStrip {
        /// Which track; `None` is the master.
        track: Option<TrackId>,
        /// New (after apply: old) values.
        strip: ChannelStrip,
    },
    /// Add a clip to a track.
    AddClip {
        /// Which track.
        track: TrackId,
        /// The clip (held while not in the project).
        clip: Option<Clip>,
    },
    /// Remove a clip.
    RemoveClip {
        /// Which track.
        track: TrackId,
        /// Which clip.
        clip: ClipId,
        /// Where it was and the clip itself (filled by apply).
        removed: Option<(usize, Clip)>,
    },
    /// Move a clip in time.
    MoveClip {
        /// Which track.
        track: TrackId,
        /// Which clip.
        clip: ClipId,
        /// New (after apply: old) start.
        start: Tick,
    },
    /// Replace the tempo map.
    SetTempo(TempoMap),
    /// Replace the time signature.
    SetTimeSig(TimeSig),
    /// Change the loop range.
    SetLoop(Option<(Tick, Tick)>),
    /// Add a marker.
    AddMarker(Option<Marker>),
    /// Remove a marker.
    RemoveMarker {
        /// Which marker.
        id: MarkerId,
        /// Where it was and the marker (filled by apply).
        removed: Option<(usize, Marker)>,
    },
    /// Replace a clip with an edited version (same id): trim, fades, gain, position.
    ReplaceClip {
        /// Which track.
        track: TrackId,
        /// New (after apply: old) clip.
        clip: Clip,
    },
    /// Move a clip to another track (appended there).
    MoveClipToTrack {
        /// Which clip.
        clip: ClipId,
        /// Current track.
        from: TrackId,
        /// New track.
        to: TrackId,
        /// Index in the old track (filled by apply).
        index: Option<usize>,
    },
    /// Change where a track's output goes.
    SetOutput {
        /// Which track.
        id: TrackId,
        /// New (after apply: old) route.
        output: OutputRoute,
    },
    /// Change arm, monitoring or input of a track.
    SetRecord {
        /// Which track.
        id: TrackId,
        /// New (after apply: old) settings.
        record: RecordSetup,
    },
    /// Replace a track's whole device chain (add/remove/reorder devices, presets).
    SetDevices {
        /// Which track.
        track: TrackId,
        /// New (after apply: old) chain.
        devices: Vec<DeviceSlot>,
    },
    /// Change one device parameter (`None` = back to the default).
    SetDeviceParam {
        /// Which track.
        track: TrackId,
        /// Which device.
        device: DeviceId,
        /// Parameter id.
        param: u64,
        /// New (after apply: old) value.
        value: Option<f32>,
    },
    /// Change the punch range.
    SetPunch(Option<(Tick, Tick)>),
    /// Add an audio file to the asset list.
    AddAsset(Option<Asset>),
    /// Several edits as one undo step (applied in order, reverted in reverse order).
    Batch(Vec<Edit>),
}

impl Edit {
    /// Short description for the Edit menu ("Undo Rename track").
    pub fn label(&self) -> &'static str {
        match self {
            Edit::AddTrack { .. } => "Add track",
            Edit::RemoveTrack { .. } => "Delete track",
            Edit::MoveTrack { .. } => "Move track",
            Edit::RenameTrack { .. } => "Rename track",
            Edit::SetTrackColor { .. } => "Track colour",
            Edit::SetStrip { .. } => "Mixer change",
            Edit::AddClip { .. } => "Add clip",
            Edit::RemoveClip { .. } => "Delete clip",
            Edit::MoveClip { .. } => "Move clip",
            Edit::SetTempo(_) => "Tempo change",
            Edit::SetTimeSig(_) => "Time signature",
            Edit::SetLoop(_) => "Loop range",
            Edit::AddMarker(_) => "Add marker",
            Edit::RemoveMarker { .. } => "Delete marker",
            Edit::ReplaceClip { .. } => "Edit clip",
            Edit::MoveClipToTrack { .. } => "Move clip",
            Edit::AddAsset(_) => "Import audio",
            Edit::SetOutput { .. } => "Output routing",
            Edit::SetRecord { .. } => "Record setup",
            Edit::SetDevices { .. } => "Devices",
            Edit::SetDeviceParam { .. } => "Parameter change",
            Edit::SetPunch(_) => "Punch range",
            Edit::Batch(edits) => edits.last().map_or("Edit", Edit::label),
        }
    }

    /// Whether `next` changes the same value, so both can become one undo step.
    fn merges_with(&self, next: &Edit) -> bool {
        match (self, next) {
            (Edit::RenameTrack { id: a, .. }, Edit::RenameTrack { id: b, .. })
            | (Edit::SetTrackColor { id: a, .. }, Edit::SetTrackColor { id: b, .. }) => a == b,
            (Edit::SetStrip { track: a, .. }, Edit::SetStrip { track: b, .. }) => a == b,
            (
                Edit::SetDeviceParam {
                    track: a,
                    device: d1,
                    param: p1,
                    ..
                },
                Edit::SetDeviceParam {
                    track: b,
                    device: d2,
                    param: p2,
                    ..
                },
            ) => a == b && d1 == d2 && p1 == p2,
            (Edit::MoveClip { clip: a, .. }, Edit::MoveClip { clip: b, .. }) => a == b,
            (Edit::ReplaceClip { clip: a, .. }, Edit::ReplaceClip { clip: b, .. }) => {
                a.id() == b.id()
            }
            (Edit::Batch(a), Edit::Batch(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.merges_with(y))
            }
            (Edit::SetTempo(_), Edit::SetTempo(_))
            | (Edit::SetTimeSig(_), Edit::SetTimeSig(_))
            | (Edit::SetLoop(_), Edit::SetLoop(_))
            | (Edit::SetPunch(_), Edit::SetPunch(_)) => true,
            _ => false,
        }
    }

    /// Applies the edit.
    pub fn apply(&mut self, p: &mut Project) -> Result<(), EditError> {
        match self {
            Edit::AddTrack { index, track } => {
                if *index > p.tracks.len() {
                    return Err(EditError::BadIndex);
                }
                let t = track.take().ok_or(EditError::State)?;
                p.tracks.insert(*index, t);
            }
            Edit::RemoveTrack { id, removed } => {
                let i = p.track_index(*id).ok_or(EditError::NoTrack)?;
                *removed = Some((i, p.tracks.remove(i)));
            }
            Edit::MoveTrack { from, to } => move_item(&mut p.tracks, *from, *to)?,
            Edit::AddClip { track, clip } => {
                let t = p.track_mut(*track).ok_or(EditError::NoTrack)?;
                t.clips.push(clip.take().ok_or(EditError::State)?);
            }
            Edit::RemoveClip {
                track,
                clip,
                removed,
            } => {
                let t = p.track_mut(*track).ok_or(EditError::NoTrack)?;
                let i = t
                    .clips
                    .iter()
                    .position(|c| c.id() == *clip)
                    .ok_or(EditError::NoClip)?;
                *removed = Some((i, t.clips.remove(i)));
            }
            Edit::AddMarker(marker) => p.markers.push(marker.take().ok_or(EditError::State)?),
            Edit::RemoveMarker { id, removed } => {
                let i = p
                    .markers
                    .iter()
                    .position(|m| m.id == *id)
                    .ok_or(EditError::NoMarker)?;
                *removed = Some((i, p.markers.remove(i)));
            }
            Edit::MoveClipToTrack {
                clip,
                from,
                to,
                index,
            } => {
                p.track(*to).ok_or(EditError::NoTrack)?;
                let src = p.track_mut(*from).ok_or(EditError::NoTrack)?;
                let i = src
                    .clips
                    .iter()
                    .position(|c| c.id() == *clip)
                    .ok_or(EditError::NoClip)?;
                let c = src.clips.remove(i);
                *index = Some(i);
                p.track_mut(*to).ok_or(EditError::NoTrack)?.clips.push(c);
            }
            Edit::AddAsset(asset) => p.assets.push(asset.take().ok_or(EditError::State)?),
            Edit::Batch(edits) => {
                let mut applied = 0;
                let mut failed = None;
                for e in edits.iter_mut() {
                    if let Err(err) = e.apply(p) {
                        failed = Some(err);
                        break;
                    }
                    applied += 1;
                }
                if let Some(err) = failed {
                    for done in edits.iter_mut().take(applied).rev() {
                        done.revert(p)?;
                    }
                    return Err(err);
                }
            }
            _ => self.swap(p)?,
        }
        Ok(())
    }

    /// Undoes the edit (must have been applied).
    pub fn revert(&mut self, p: &mut Project) -> Result<(), EditError> {
        match self {
            Edit::AddTrack { index, track } => {
                if *index >= p.tracks.len() {
                    return Err(EditError::BadIndex);
                }
                *track = Some(p.tracks.remove(*index));
            }
            Edit::RemoveTrack { removed, .. } => {
                let (i, t) = removed.take().ok_or(EditError::State)?;
                if i > p.tracks.len() {
                    return Err(EditError::BadIndex);
                }
                p.tracks.insert(i, t);
            }
            Edit::MoveTrack { from, to } => move_item(&mut p.tracks, *to, *from)?,
            Edit::AddClip { track, clip } => {
                let t = p.track_mut(*track).ok_or(EditError::NoTrack)?;
                *clip = Some(t.clips.pop().ok_or(EditError::NoClip)?);
            }
            Edit::RemoveClip { track, removed, .. } => {
                let t = p.track_mut(*track).ok_or(EditError::NoTrack)?;
                let (i, c) = removed.take().ok_or(EditError::State)?;
                if i > t.clips.len() {
                    return Err(EditError::BadIndex);
                }
                t.clips.insert(i, c);
            }
            Edit::AddMarker(marker) => *marker = Some(p.markers.pop().ok_or(EditError::NoMarker)?),
            Edit::RemoveMarker { removed, .. } => {
                let (i, m) = removed.take().ok_or(EditError::State)?;
                if i > p.markers.len() {
                    return Err(EditError::BadIndex);
                }
                p.markers.insert(i, m);
            }
            Edit::MoveClipToTrack {
                clip,
                from,
                to,
                index,
            } => {
                let dst = p.track_mut(*to).ok_or(EditError::NoTrack)?;
                let i = dst
                    .clips
                    .iter()
                    .position(|c| c.id() == *clip)
                    .ok_or(EditError::NoClip)?;
                let c = dst.clips.remove(i);
                let src = p.track_mut(*from).ok_or(EditError::NoTrack)?;
                let at = index.take().ok_or(EditError::State)?;
                if at > src.clips.len() {
                    return Err(EditError::BadIndex);
                }
                src.clips.insert(at, c);
            }
            Edit::AddAsset(asset) => *asset = Some(p.assets.pop().ok_or(EditError::State)?),
            Edit::Batch(edits) => {
                for e in edits.iter_mut().rev() {
                    e.revert(p)?;
                }
            }
            _ => self.swap(p)?,
        }
        Ok(())
    }

    /// Exchanges the stored value with the project's (self-inverse "Set" edits).
    fn swap(&mut self, p: &mut Project) -> Result<(), EditError> {
        match self {
            Edit::RenameTrack { id, name } => {
                let t = p.track_mut(*id).ok_or(EditError::NoTrack)?;
                std::mem::swap(&mut t.name, name);
            }
            Edit::SetTrackColor { id, color } => {
                let t = p.track_mut(*id).ok_or(EditError::NoTrack)?;
                std::mem::swap(&mut t.color, color);
            }
            Edit::SetStrip { track, strip } => {
                let s = match track {
                    Some(id) => &mut p.track_mut(*id).ok_or(EditError::NoTrack)?.strip,
                    None => &mut p.master,
                };
                std::mem::swap(s, strip);
            }
            Edit::MoveClip { track, clip, start } => {
                let t = p.track_mut(*track).ok_or(EditError::NoTrack)?;
                let c = t
                    .clips
                    .iter_mut()
                    .find(|c| c.id() == *clip)
                    .ok_or(EditError::NoClip)?;
                let old = c.start();
                c.set_start(*start);
                *start = old;
            }
            Edit::SetTempo(map) => std::mem::swap(&mut p.tempo, map),
            Edit::SetTimeSig(sig) => std::mem::swap(&mut p.time_sig, sig),
            Edit::SetLoop(range) => std::mem::swap(&mut p.loop_range, range),
            Edit::SetRecord { id, record } => {
                let t = p.track_mut(*id).ok_or(EditError::NoTrack)?;
                std::mem::swap(&mut t.record, record);
            }
            Edit::SetPunch(range) => std::mem::swap(&mut p.punch_range, range),
            Edit::SetDevices { track, devices } => {
                let t = p.track_mut(*track).ok_or(EditError::NoTrack)?;
                std::mem::swap(&mut t.devices, devices);
            }
            Edit::SetDeviceParam {
                track,
                device,
                param,
                value,
            } => {
                let t = p.track_mut(*track).ok_or(EditError::NoTrack)?;
                let d = t
                    .devices
                    .iter_mut()
                    .find(|d| d.id == *device)
                    .ok_or(EditError::NoDevice)?;
                *value = d.set_param(*param, *value);
            }
            Edit::SetOutput { id, output } => {
                let t = p.track_mut(*id).ok_or(EditError::NoTrack)?;
                std::mem::swap(&mut t.output, output);
            }
            Edit::ReplaceClip { track, clip } => {
                let t = p.track_mut(*track).ok_or(EditError::NoTrack)?;
                let c = t
                    .clips
                    .iter_mut()
                    .find(|c| c.id() == clip.id())
                    .ok_or(EditError::NoClip)?;
                std::mem::swap(c, clip);
            }
            _ => return Err(EditError::State),
        }
        Ok(())
    }
}

fn move_item<T>(v: &mut Vec<T>, from: usize, to: usize) -> Result<(), EditError> {
    if from >= v.len() || to >= v.len() {
        return Err(EditError::BadIndex);
    }
    let item = v.remove(from);
    v.insert(to, item);
    Ok(())
}

/// Undo/redo history plus "unsaved changes" tracking.
#[derive(Debug, Default)]
pub struct UndoStack {
    done: Vec<Edit>,
    undone: Vec<Edit>,
    /// `done.len()` when the project was last saved; `None` if that state is unreachable.
    saved_at: Option<usize>,
}

impl UndoStack {
    /// Empty history; the current state counts as saved.
    pub fn new() -> Self {
        Self {
            saved_at: Some(0),
            ..Self::default()
        }
    }

    /// Applies `edit` and records it. With `merge`, an edit that changes the same value as
    /// the previous one (e.g. while dragging a fader) joins it into one undo step.
    pub fn perform(
        &mut self,
        p: &mut Project,
        mut edit: Edit,
        merge: bool,
    ) -> Result<(), EditError> {
        edit.apply(p)?;
        self.undone.clear();
        if merge && self.done.last().is_some_and(|last| last.merges_with(&edit)) {
            // The previous edit still holds the value from before the first change.
            if self.saved_at == Some(self.done.len()) {
                self.saved_at = None;
            }
            return Ok(());
        }
        if self.saved_at.is_some_and(|s| s > self.done.len()) {
            self.saved_at = None;
        }
        self.done.push(edit);
        if self.done.len() > MAX_UNDO {
            self.done.remove(0);
            self.saved_at = self.saved_at.and_then(|s| s.checked_sub(1));
        }
        Ok(())
    }

    /// Undoes the last edit. Returns its label, or `None` if there is nothing to undo.
    pub fn undo(&mut self, p: &mut Project) -> Result<Option<&'static str>, EditError> {
        let Some(mut edit) = self.done.pop() else {
            return Ok(None);
        };
        let label = edit.label();
        edit.revert(p)?;
        self.undone.push(edit);
        Ok(Some(label))
    }

    /// Redoes the last undone edit.
    pub fn redo(&mut self, p: &mut Project) -> Result<Option<&'static str>, EditError> {
        let Some(mut edit) = self.undone.pop() else {
            return Ok(None);
        };
        let label = edit.label();
        edit.apply(p)?;
        self.done.push(edit);
        Ok(Some(label))
    }

    /// Label of the edit [`Self::undo`] would undo.
    pub fn undo_label(&self) -> Option<&'static str> {
        self.done.last().map(Edit::label)
    }

    /// Label of the edit [`Self::redo`] would redo.
    pub fn redo_label(&self) -> Option<&'static str> {
        self.undone.last().map(Edit::label)
    }

    /// Remember the current state as saved.
    pub fn mark_saved(&mut self) {
        self.saved_at = Some(self.done.len());
    }

    /// `true` if the project differs from the last saved state.
    pub fn is_dirty(&self) -> bool {
        self.saved_at != Some(self.done.len())
    }
}
