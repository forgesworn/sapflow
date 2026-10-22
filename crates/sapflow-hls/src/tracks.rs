//! Declared media topology, distinct from which codec header arrived first.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackSet {
    AudioOnly,
    VideoOnly,
    AudioVideo,
}

impl TrackSet {
    pub const fn has_video(self) -> bool {
        matches!(self, Self::VideoOnly | Self::AudioVideo)
    }

    pub const fn has_audio(self) -> bool {
        matches!(self, Self::AudioOnly | Self::AudioVideo)
    }
}
