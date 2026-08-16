//! Tiny in-game event log shown in the UI, used for diagnosing job failures.

use bevy::prelude::*;
use std::collections::VecDeque;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogKind {
    Info,
    Job,
    Fail,
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub time: f64,
    pub kind: LogKind,
    pub text: String,
}

#[derive(Resource, Default)]
pub struct EventLog {
    pub entries: VecDeque<LogEntry>,
}

impl EventLog {
    pub const VISIBLE: usize = 7;

    pub fn push(&mut self, time: f64, kind: LogKind, text: impl Into<String>) {
        if self.entries.len() >= 64 {
            self.entries.pop_front();
        }
        self.entries.push_back(LogEntry {
            time,
            kind,
            text: text.into(),
        });
    }

    /// Cooldown before retrying an unreachable target (sim seconds;
    /// 15 real seconds at 1× = 15 ship minutes).
    pub const UNREACHABLE_COOLDOWN: f64 = 15.0 * crate::simtime::BASE_SIM_RATE;
}
