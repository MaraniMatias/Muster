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
    EscapeIntermediate,
    Csi,
    String(bool),
    StringEscape,
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
            (Scan::Escape, 0x18 | 0x1a) => Scan::Ground,
            (Scan::Escape, 0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f..=0xff) => Scan::Escape,
            (Scan::Escape, 0x20..=0x2f) => Scan::EscapeIntermediate,
            (Scan::EscapeIntermediate, 0x18 | 0x1a) => Scan::Ground,
            (Scan::EscapeIntermediate, 0x1b) => Scan::Escape,
            (
                Scan::EscapeIntermediate,
                0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f..=0xff | 0x20..=0x2f,
            ) => Scan::EscapeIntermediate,
            (Scan::EscapeIntermediate, _) => Scan::Ground,
            (Scan::Escape, b'[') => {
                self.csi = *b"\x1b[\0\0\0\0\0\0";
                self.length = 2;
                Scan::Csi
            }
            (Scan::Escape, b']') => Scan::String(true),
            (Scan::Escape, b'_' | b'P' | b'^' | b'X') => Scan::String(false),
            (Scan::Csi, 0x1b) => Scan::Escape,
            (Scan::Csi, 0x18 | 0x1a) => Scan::Ground,
            (Scan::Csi, 0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f..=0xff) => Scan::Csi,
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
            (Scan::String(_), 0x1b) => Scan::StringEscape,
            (Scan::String(osc), _) => Scan::String(*osc),
            (Scan::StringEscape, b'\\' | 0x18 | 0x1a) => Scan::Ground,
            (Scan::StringEscape, 0x1b) => Scan::Escape,
            (Scan::StringEscape, 0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f..=0xff) => Scan::Escape,
            (Scan::StringEscape, 0x20..=0x2f) => Scan::EscapeIntermediate,
            (Scan::StringEscape, b'[') => {
                self.csi = *b"\x1b[\0\0\0\0\0\0";
                self.length = 2;
                Scan::Csi
            }
            (Scan::StringEscape, b']') => Scan::String(true),
            (Scan::StringEscape, b'_' | b'P' | b'^' | b'X') => Scan::String(false),
            (Scan::StringEscape, _) => Scan::Ground,
            _ => Scan::Ground,
        };
        marker
    }

    pub(super) fn expired(&self, now: Instant) -> bool {
        self.started
            .is_some_and(|started| now.saturating_duration_since(started) >= TIMEOUT)
    }
}

#[cfg(test)]
mod tests {
    use super::SyncUpdates;

    #[test]
    fn canceled_and_escaped_strings_recover_before_a_sync_marker() {
        for (cancel, suffix) in [
            (0x18, b"\x1b[?2026h".as_slice()),
            (0x1a, b"\x1b[?2026h".as_slice()),
            (0x1b, b"[?2026h".as_slice()),
        ] {
            let mut scan = SyncUpdates::default();
            let mut bytes = b"\x1b]payload".to_vec();
            bytes.push(cancel);
            bytes.extend_from_slice(suffix);
            let markers: Vec<_> = bytes
                .into_iter()
                .filter_map(|byte| scan.scan(byte))
                .collect();
            assert_eq!(markers, [true], "cancellation byte {cancel:#x}");
        }
    }

    #[test]
    fn ignored_controls_after_string_escape_preserve_the_vt_escape_state() {
        for introducer in b"]_P^X".iter().copied() {
            for ignored in (0x00..=0x17)
                .chain([0x19])
                .chain(0x1c..=0x1f)
                .chain([0x7f, 0x80])
            {
                let bytes = [
                    b"\x1b".as_slice(),
                    &[introducer],
                    b"payload\x1b",
                    &[ignored],
                    b"[?2026h",
                ]
                .concat();
                for split in 0..=bytes.len() {
                    let mut scan = SyncUpdates::default();
                    let markers: Vec<_> = bytes[..split]
                        .iter()
                        .chain(&bytes[split..])
                        .filter_map(|byte| scan.scan(*byte))
                        .collect();
                    assert_eq!(
                        markers,
                        [true],
                        "introducer {introducer:#x}, ignored byte {ignored:#x}, split {split}"
                    );
                }
            }
        }
    }

    #[test]
    fn escape_intermediates_do_not_open_strings_or_find_sync_markers() {
        let mut scan = SyncUpdates::default();
        for byte in b"\x1b(]" {
            assert_eq!(scan.scan(*byte), None);
        }
        assert_eq!(
            b"\x1b[?2026h".iter().find_map(|byte| scan.scan(*byte)),
            Some(true)
        );
        assert_eq!(
            b"\x1b[?2026l".iter().find_map(|byte| scan.scan(*byte)),
            Some(false)
        );
    }

    #[test]
    fn ignored_csi_controls_do_not_hide_synchronization_markers() {
        for ignored in [0x00, 0x7f, 0x80] {
            let bytes = [b"\x1b[?".as_slice(), &[ignored], b"2026h"].concat();
            for split in 0..=bytes.len() {
                let mut scan = SyncUpdates::default();
                let markers: Vec<_> = bytes[..split]
                    .iter()
                    .chain(&bytes[split..])
                    .filter_map(|byte| scan.scan(*byte))
                    .collect();
                assert_eq!(markers, [true], "ignored byte {ignored:#x}, split {split}");
            }
        }
    }

    #[test]
    fn synchronization_markers_inside_string_payload_are_not_scanned() {
        let mut scan = SyncUpdates::default();
        for byte in b"\x1b]payload?2026h\x07" {
            assert_eq!(scan.scan(*byte), None);
        }
        assert_eq!(
            b"\x1b[?2026h".iter().find_map(|byte| scan.scan(*byte)),
            Some(true)
        );
    }
}
