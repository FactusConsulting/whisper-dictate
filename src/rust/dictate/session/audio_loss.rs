//! Recording-local queue diagnostics, independent of STT backend metadata.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RecordingAudioLoss {
    pub capture_chunks_dropped: u64,
    pub pipeline_events_dropped: u64,
    pub pending_frames_dropped: u64,
}

impl RecordingAudioLoss {
    pub(crate) fn fields(self) -> [(&'static str, u64); 3] {
        [
            ("capture_chunks_dropped", self.capture_chunks_dropped),
            ("pipeline_events_dropped", self.pipeline_events_dropped),
            ("pending_frames_dropped", self.pending_frames_dropped),
        ]
    }
}

#[cfg(test)]
#[path = "audio_loss_tests.rs"]
mod tests;
