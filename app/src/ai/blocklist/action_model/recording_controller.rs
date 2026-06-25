//! Process-global registry of in-progress video recordings.
//!
//! A recording's capture process must outlive the `StartRecording` tool call
//! that launches it and survive until a later `StopRecording` call (possibly
//! from a different subagent sharing the display), so the live handle lives here
//! rather than in a per-call executor.

use std::collections::HashMap;

use warpui::{Entity, SingletonEntity};

/// Holds the live capture handle for a single in-progress recording.
pub struct RecordingSession {
    handle: computer_use::RecordingHandle,
}

impl RecordingSession {
    pub fn new(handle: computer_use::RecordingHandle) -> Self {
        Self { handle }
    }

    pub fn into_handle(self) -> computer_use::RecordingHandle {
        self.handle
    }
}

/// Tracks recordings keyed by id and enforces a single active recording per
/// display. Stop is idempotent: an unknown id resolves to `None`.
pub struct RecordingController {
    sessions: HashMap<String, RecordingSession>,
    /// Set while a start is in flight (after reservation, before the session is
    /// registered) so a concurrent start cannot race past the single-slot guard.
    starting: bool,
}

impl RecordingController {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            starting: false,
        }
    }

    /// Reserves the single recording slot, failing if one is already active or
    /// starting.
    pub fn try_begin_start(&mut self) -> Result<(), String> {
        if self.starting || !self.sessions.is_empty() {
            return Err("A recording is already in progress on this display.".to_string());
        }
        self.starting = true;
        Ok(())
    }

    /// Registers a successfully started recording, releasing the start reservation.
    pub fn finish_start(&mut self, recording_id: String, session: RecordingSession) {
        self.starting = false;
        self.sessions.insert(recording_id, session);
    }

    /// Releases the start reservation after a failed start.
    pub fn abort_start(&mut self) {
        self.starting = false;
    }

    /// Removes and returns the session for `recording_id`, if present.
    pub fn take_session(&mut self, recording_id: &str) -> Option<RecordingSession> {
        self.sessions.remove(recording_id)
    }
}

impl Entity for RecordingController {
    type Event = ();
}

impl SingletonEntity for RecordingController {}
