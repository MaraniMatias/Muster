//! Keep complete synchronized updates at the raw-byte boundary. VT and
//! graphics must be interpreted in one order, not buffered by separate parsers.
use std::time::{Duration, Instant};

pub(super) const MAX_SYNC_BYTES: usize = 2 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_millis(150);

#[derive(Default)]
enum Scan {
    #[default]
    Ground,
    Escape,
    Csi,
    String(bool),
    StringEscape(bool),
}

#[derive(Default)]
pub(super) struct SyncUpdates {
    scan: Scan,
    csi: [u8; 8],
    length: usize,
    pub(super) started: Option<Instant>,
    pub(super) bytes: Vec<u8>,
}

impl SyncUpdates {
    /// Recognize only the two exact sequences the VT processor recognizes,
    /// never their lookalikes inside OSC, APC, DCS, PM or SOS strings.
    pub(super) fn scan(&mut self, byte: u8) -> Option<bool> {
        let mut marker = None;
        self.scan = match (&self.scan, byte) {
            (Scan::Ground, 0x1b) | (Scan::Escape, 0x1b) => Scan::Escape,
            (Scan::Escape, b'[') => {
                self.csi = *b"\x1b[\0\0\0\0\0\0";
                self.length = 2;
                Scan::Csi
            }
            (Scan::Escape, b']') => Scan::String(true),
            (Scan::Escape, b'_' | b'P' | b'^' | b'X') => Scan::String(false),
            (Scan::Csi, 0x1b) => Scan::Escape,
            (Scan::Csi, 0x18 | 0x1a) => Scan::Ground,
            (Scan::Csi, _) => {
                if self.length < self.csi.len() {
                    self.csi[self.length] = byte;
                }
                self.length = self.length.saturating_add(1);
                if (0x40..=0x7e).contains(&byte) {
                    if self.length == 8 {
                        if self.csi == *b"\x1b[?2026h" {
                            marker = Some(true);
                        } else if self.csi == *b"\x1b[?2026l" {
                            marker = Some(false);
                        }
                    }
                    Scan::Ground
                } else {
                    Scan::Csi
                }
            }
            (Scan::String(true), 0x07) | (Scan::String(_), 0x18 | 0x1a) => Scan::Ground,
            (Scan::String(osc), 0x1b) => Scan::StringEscape(*osc),
            (Scan::String(osc), _) => Scan::String(*osc),
            (Scan::StringEscape(_), b'\\') => Scan::Ground,
            (Scan::StringEscape(osc), 0x1b) => Scan::StringEscape(*osc),
            (Scan::StringEscape(osc), _) => Scan::String(*osc),
            _ => Scan::Ground,
        };
        marker
    }

    pub(super) fn expired(&self, now: Instant) -> bool {
        self.started
            .is_some_and(|started| now.saturating_duration_since(started) >= TIMEOUT)
    }
}
