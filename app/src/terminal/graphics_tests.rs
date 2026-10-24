use super::*;
use crate::terminal::{TerminalCore, TerminalEvent};

fn core() -> TerminalCore {
    TerminalCore::new(12, 6, 2)
}

fn send(core: &mut TerminalCore, controls: &str, data: &[u8]) {
    let mut bytes = format!("\x1b_G{controls}").into_bytes();
    if !data.is_empty() {
        bytes.push(b';');
        bytes.extend_from_slice(data);
    }
    bytes.extend_from_slice(b"\x1b\\");
    core.feed(&bytes);
}

fn red(core: &mut TerminalCore, controls: &str) {
    send(
        core,
        &format!("a=T,f=32,s=1,v=1,C=1,q=2,{controls}"),
        b"/wAA/w==",
    );
}

fn replies(core: &mut TerminalCore) -> Vec<String> {
    core.take_events()
        .into_iter()
        .filter_map(|event| match event {
            TerminalEvent::PtyWrite(reply) => Some(reply),
            _ => None,
        })
        .collect()
}

fn png() -> Vec<u8> {
    let mut data = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255])))
        .write_to(&mut data, image::ImageFormat::Png)
        .unwrap();
    STANDARD.encode(data.into_inner()).into_bytes()
}

#[test]
fn pi_png_chunks_are_placed_once_without_moving_the_cursor() {
    let mut core = core();
    let png = STANDARD.encode(include_bytes!("../../assets/icon.png"));
    let chunks: Vec<_> = png.as_bytes().chunks(4096).collect();
    assert!(chunks.len() > 1);
    core.feed(b"A");
    for (i, chunk) in chunks.iter().enumerate() {
        let first = if i == 0 {
            "a=T,f=100,q=2,C=1,c=4,r=2,i=4278190122,"
        } else {
            ""
        };
        send(
            &mut core,
            &format!("{first}m={}", u8::from(i + 1 < chunks.len())),
            chunk,
        );
        assert_eq!(
            core.graphic_snapshots().len(),
            usize::from(i + 1 == chunks.len())
        );
    }
    let graphics = core.graphic_snapshots();
    assert_eq!(
        (graphics[0].column, graphics[0].columns, graphics[0].rows),
        (1, 4, 2)
    );
    assert_eq!(graphics[0].source, [0, 0, 512, 512]);
    assert_eq!(core.cursor().col, 1);
    assert!(replies(&mut core).is_empty());
    send(&mut core, "a=d,d=a,q=2", b"");
    assert!(core.graphic_snapshots().is_empty());
    send(&mut core, "a=p,q=2,i=4278190122,c=4,r=2,C=1", b"");
    assert_eq!(core.graphic_snapshots().len(), 1);
}

#[test]
fn a_canceled_or_escaped_apc_does_not_swallow_the_next_graphics_packet() {
    for ending in [0x18, 0x1a, 0x1b] {
        let mut interrupted = b"\x1b_Ga=T,f=32,s=1,v=1,C=1,q=2;partial".to_vec();
        interrupted.push(ending);
        for split in 0..=interrupted.len() {
            let mut core = core();
            core.feed(&interrupted[..split]);
            core.feed(&interrupted[split..]);
            red(&mut core, "c=1,r=1");
            assert_eq!(
                core.graphic_snapshots().len(),
                1,
                "terminator {ending:#x}, split {split}"
            );
        }
    }
}

#[test]
fn oversized_osc_bel_preserves_following_text_and_graphics_in_same_or_split_chunks() {
    const OSC_CAP: usize = 64 * 1024;
    let mut bytes = b"AB\x1b]0;".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', OSC_CAP + 1));
    bytes.extend_from_slice(b"\x07C\x1b_Ga=T,f=32,s=1,v=1,C=1,q=2;/wAA/w==\x1b\\D\x1b]2;after\x07");

    for split in [
        0,
        bytes.len(),
        2 + 5 + OSC_CAP,
        2 + 5 + OSC_CAP + 1,
        2 + 5 + OSC_CAP + 2,
        2 + 5 + OSC_CAP + 3,
        2 + 5 + OSC_CAP + 4,
    ] {
        let split = split.min(bytes.len());
        let mut core = core();
        core.feed(&bytes[..split]);
        core.feed(&bytes[split..]);
        let graphics = core.graphic_snapshots();
        assert_eq!(graphics.len(), 1, "split {split}");
        assert_eq!(graphics[0].column, 3, "APC anchors after preceding ABC");
        assert_eq!(
            core.visible_rows()[0].cells[..4]
                .iter()
                .map(|c| c.c)
                .collect::<String>(),
            "ABCD"
        );
        assert_eq!(
            core.cursor().col,
            4,
            "graphic placement does not move cursor"
        );
        assert_eq!(core.title(), Some("after"));
    }
}

#[test]
fn oversized_osc_terminators_recover_into_following_vt_sequences() {
    const OSC_CAP: usize = 64 * 1024;
    let ordinary = b"C\x1b[2C\x1b_Ga=T,f=32,s=1,v=1,C=1,q=2;/wAA/w==\x1b\\D\x1b]2;recovered\x07";
    let immediate_csi = b"[2C\x1b_Ga=T,f=32,s=1,v=1,C=1,q=2;/wAA/w==\x1b\\D\x1b]2;recovered\x07";
    let immediate_apc = b"_Ga=T,f=32,s=1,v=1,C=1,q=2;/wAA/w==\x1b\\C\x1b[2C D\x1b]2;recovered\x07";
    let immediate_osc = b"]2;recovered\x07C\x1b[2C\x1b_Ga=T,f=32,s=1,v=1,C=1,q=2;/wAA/w==\x1b\\D";
    let cases: &[(&[u8], &[u8], u32)] = &[
        (b"\x07", ordinary, 5),
        (b"\x18", ordinary, 5),
        (b"\x1a", ordinary, 5),
        (b"\x1b\\", ordinary, 5),
        (b"\x1b", immediate_csi, 4),
        (b"\x1b", immediate_apc, 2),
        (b"\x1b", immediate_osc, 5),
    ];

    for (ending, following, graphic_column) in cases {
        let mut bytes = b"AB\x1b]0;".to_vec();
        bytes.extend(std::iter::repeat_n(b'x', OSC_CAP + 1));
        let boundary = bytes.len();
        bytes.extend_from_slice(ending);
        bytes.extend_from_slice(following);
        let after_terminator = boundary + ending.len();
        let mut splits = vec![0, bytes.len(), boundary - 1, boundary, boundary + 1];
        for split in [
            after_terminator.saturating_sub(1),
            after_terminator,
            after_terminator + 1,
        ] {
            splits.push(split.min(bytes.len()));
        }

        for split in splits {
            let mut core = core();
            core.feed(&bytes[..split]);
            core.feed(&bytes[split..]);
            assert_eq!(
                core.title(),
                Some("recovered"),
                "ending {ending:?}, split {split}"
            );
            let graphics = core.graphic_snapshots();
            assert_eq!(
                graphics.len(),
                1,
                "ending {ending:?}, following {following:?}, split {split}"
            );
            assert_eq!(graphics[0].column, *graphic_column, "split {split}");
            assert!(core.visible_rows()[0]
                .cells
                .iter()
                .any(|cell| cell.c == 'D'));
        }
    }
}

#[test]
fn every_pty_chunk_boundary_preserves_the_graphics_command_and_following_text() {
    let message = format!(
        "A\x1b_Ga=T,f=100,q=2,C=1,c=2,r=1,i=7;{}\x1b\\B",
        String::from_utf8(png()).unwrap()
    );
    for split in 0..=message.len() {
        let mut core = core();
        core.feed(&message.as_bytes()[..split]);
        core.feed(&message.as_bytes()[split..]);
        let graphics = core.graphic_snapshots();
        assert_eq!(graphics.len(), 1, "split {split}");
        assert_eq!(graphics[0].column, 1, "split {split}");
        assert_eq!(core.visible_rows()[0].cells[1].c, 'B', "split {split}");
        assert!(replies(&mut core).is_empty());
    }
}

#[test]
fn unicode_placeholders_use_the_high_id_byte_and_inherit_omitted_coordinates() {
    let mut core = core();
    send(
        &mut core,
        "a=T,f=32,s=1,v=1,q=2,U=1,c=2,r=2,i=33554474",
        b"/wAA/w==",
    );
    assert!(
        core.graphic_snapshots().is_empty(),
        "a virtual placement is invisible"
    );
    let text = "\x1b[38;5;42m\u{10eeee}\u{305}\u{305}\u{30e}\u{10eeee}\r\n\u{10eeee}\u{30d}\u{305}\u{30e}\u{10eeee}\x1b[0m";
    for byte in text.as_bytes() {
        core.feed(&[*byte]);
    }
    let graphics = core.graphic_snapshots();
    assert_eq!(graphics.len(), 4);
    for (i, g) in graphics.iter().enumerate() {
        assert_eq!(g.id, 33554474);
        assert_eq!(g.placeholder, Some([(i % 2) as u32, (i / 2) as u32]));
        assert_eq!(g.image.as_bytes(0).unwrap(), &[0, 0, 255, 255]);
    }
    assert!(core.visible_rows().iter().all(|row| row
        .cells
        .iter()
        .all(|cell| cell.c != PLACEHOLDER && cell.zerowidth.is_empty())));
    send(&mut core, "a=d,d=A,q=2", b"");
    assert_eq!(
        core.graphic_snapshots().len(),
        4,
        "delete visible does not delete virtual prototypes"
    );
    core.feed(b"\x1b[2J");
    assert!(
        core.graphic_snapshots().is_empty(),
        "erasing cells erases the displayed placeholders"
    );
    send(&mut core, "a=d,d=I,i=33554474,q=2", b"");
    core.feed(text.as_bytes());
    assert!(core.graphic_snapshots().is_empty());
}

#[test]
fn placeholders_reflow_scroll_and_erase_with_the_actual_grid() {
    let mut core = TerminalCore::new(4, 2, 1);
    red(&mut core, "U=1,i=42,c=4,r=1");
    core.feed(
        "\x1b[38;5;42m\u{10eeee}\u{305}\u{305}\u{10eeee}\u{305}\u{30d}\u{10eeee}\u{305}\u{30e}\u{10eeee}\u{305}\u{310}\x1b[0m".as_bytes(),
    );
    core.resize(2, 2);
    core.scroll_by(1);
    assert_eq!(core.graphic_snapshots().len(), 4);
    core.set_display_offset(0);
    core.feed(b"\r\nnext\r\nlast");
    assert!(core.graphic_snapshots().is_empty());
    core.scroll_by(1);
    assert!(
        core.graphic_snapshots().is_empty(),
        "discarded history must not resurrect graphics"
    );
}

#[test]
fn classic_placements_follow_output_after_history_is_full() {
    let mut core = TerminalCore::new(8, 3, 1);
    core.feed(b"a\r\nb\r\nc\r\nd\r\n");
    assert_eq!(core.history_size(), 1);
    red(&mut core, "i=1,c=1,r=1");
    assert_eq!(core.graphic_snapshots()[0].row, 2);
    core.feed(b"e\r\n");
    assert_eq!(core.history_size(), 1);
    assert_eq!(core.graphic_snapshots()[0].row, 1);
    core.feed(b"f\r\ng\r\n");
    assert!(core.graphic_snapshots().is_empty());
    core.scroll_by(1);
    assert_eq!(core.graphic_snapshots()[0].row, 0);
    core.feed(b"h\r\n");
    assert!(core.graphic_snapshots().is_empty());
}

#[test]
fn scroll_regions_and_insert_delete_lines_move_only_their_images() {
    let mut core = core();
    red(&mut core, "i=1,c=1,r=1");
    core.feed(b"\x1b[3;1H");
    red(&mut core, "i=2,c=1,r=1");
    core.feed(b"\x1b[2;5r\x1b[5;1H\n");
    let graphics = core.graphic_snapshots();
    assert_eq!(graphics.iter().find(|g| g.id == 1).unwrap().row, 0);
    assert_eq!(graphics.iter().find(|g| g.id == 2).unwrap().row, 1);
    core.feed(b"\x1b[2;1H\x1b[L");
    assert_eq!(
        core.graphic_snapshots()
            .iter()
            .find(|g| g.id == 2)
            .unwrap()
            .row,
        2
    );
    core.feed(b"\x1b[M");
    assert_eq!(
        core.graphic_snapshots()
            .iter()
            .find(|g| g.id == 2)
            .unwrap()
            .row,
        1
    );
}

#[test]
fn autowrap_at_the_bottom_scrolls_images() {
    let mut core = TerminalCore::new(4, 2, 1);
    core.feed(b"\x1b[2;1H");
    red(&mut core, "i=1,c=1,r=1");
    core.feed(b"abcde");
    assert_eq!(core.graphic_snapshots()[0].row, 0);
}

#[test]
fn screen_switches_and_reset_never_leak_images_between_buffers() {
    let mut core = core();
    red(&mut core, "i=1,c=1,r=1");
    core.feed(b"\x1b[?1049h");
    assert!(core.graphic_snapshots().is_empty());
    red(&mut core, "i=2,c=1,r=1");
    assert_eq!(core.graphic_snapshots()[0].id, 2);
    core.feed(b"\x1b[?1049l");
    assert_eq!(core.graphic_snapshots()[0].id, 1);
    core.feed(b"\x1b[?1049h");
    assert!(core.graphic_snapshots().is_empty());
    core.feed(b"\x1b[?1049l\x1bc");
    assert!(core.graphic_snapshots().is_empty());
}

#[test]
fn cursor_policy_moves_both_rows_and_columns_unless_suppressed() {
    let mut core = core();
    send(&mut core, "a=T,f=32,s=1,v=1,i=1,c=2,r=3,q=2", b"/wAA/w==");
    assert_eq!((core.cursor().row, core.cursor().col), (3, 2));
}

#[test]
fn transfer_and_placement_deletions_have_distinct_lifetimes() {
    let mut core = core();
    red(&mut core, "i=7,c=1,r=1");
    send(&mut core, "a=d,d=i,i=7,q=2", b"");
    send(&mut core, "a=p,i=7,p=1,c=1,r=1,C=1", b"");
    assert_eq!(core.graphic_snapshots().len(), 1);
    assert_eq!(replies(&mut core), ["\x1b_Gi=7,p=1;OK\x1b\\"]);
    send(&mut core, "a=d,d=I,i=7,q=2", b"");
    send(&mut core, "a=p,i=7,c=1,r=1", b"");
    assert!(core.graphic_snapshots().is_empty());
    assert!(replies(&mut core)[0].contains("ENOENT"));
}

#[test]
fn malformed_unsupported_and_oversized_graphics_are_refused_without_affecting_text() {
    let mut core = core();
    for (controls, data) in [
        ("a=T,f=100,i=1", b"%%%%".as_slice()),
        (
            "a=T,f=32,s=4294967295,v=4294967295,i=1",
            b"/wAA/w==".as_slice(),
        ),
        ("a=T,f=100,t=f,i=1", b"L2V0Yy9wYXNzd2Q=".as_slice()),
        ("a=T,f=32,s=1,v=1,i=1,c=4294967295", b"/wAA/w==".as_slice()),
        ("a=T,f=100,i=1,o=z", b"eA==".as_slice()),
    ] {
        send(&mut core, controls, data);
        assert!(core.graphic_snapshots().is_empty());
        assert!(replies(&mut core).iter().all(|r| !r.contains(";OK")));
    }
    send(&mut core, "a=T,f=100,i=1", &vec![b'A'; MAX_PACKET + 1]);
    send(&mut core, "a=T,f=32,s=1,v=1,i=1,i=2", b"/wAA/w==");
    core.feed(b"hello");
    assert!(core.graphic_snapshots().is_empty());
    assert_eq!(
        core.visible_rows()[0].cells[..5]
            .iter()
            .map(|c| c.c)
            .collect::<String>(),
        "hello"
    );
}

#[test]
fn a_failed_replacement_preserves_the_previous_image_and_placement() {
    let mut core = core();
    red(&mut core, "i=7,c=1,r=1");
    let before = core.graphic_snapshots();
    send(&mut core, "a=T,f=32,s=1,v=1,i=7,c=1,r=1,X=999", b"AAAA/w==");
    assert_eq!(core.graphic_snapshots(), before);
}

#[test]
fn delete_aborts_an_incomplete_transfer_and_cannot_publish_its_tail() {
    let mut core = core();
    let data = png();
    send(&mut core, "a=T,f=100,i=1,m=1", &data[..8]);
    send(&mut core, "a=d,d=A,q=2", b"");
    send(&mut core, "m=0", &data[8..]);
    assert!(core.graphic_snapshots().is_empty());
    red(&mut core, "i=2,c=1,r=1");
    assert_eq!(core.graphic_snapshots().len(), 1);
}

#[test]
fn queries_acknowledge_supported_data_without_storing_or_displaying_it() {
    let mut core = core();
    send(&mut core, "a=q,f=32,s=1,v=1,i=99", b"/wAA/w==");
    assert_eq!(replies(&mut core), ["\x1b_Gi=99;OK\x1b\\"]);
    assert!(core.graphic_snapshots().is_empty());
    send(&mut core, "a=p,i=99", b"");
    assert!(replies(&mut core)[0].contains("ENOENT"));
}

#[test]
fn aggregate_chunk_budget_is_bounded_and_the_next_upload_still_works() {
    let mut graphics = Graphics::new(6);
    struct Events;
    impl EventListener for Events {
        fn send_event(&self, _: alacritty_terminal::event::Event) {}
    }
    let mut term = Term::new(
        Default::default(),
        &alacritty_terminal::term::test::TermSize::new(12, 6),
        Events,
    );
    graphics.apply(b"Ga=T,f=100,i=1,m=1;AAAA", &mut term);
    let packet = [b"Gm=1;".as_slice(), &vec![b'A'; 4096]].concat();
    for _ in 0..=MAX_TRANSFER / 4096 {
        graphics.apply(&packet, &mut term);
    }
    let upload = graphics.buffers[0].upload.as_ref().unwrap();
    assert!(upload.refused);
    assert!(upload.encoded.is_empty());
    graphics.apply(b"Gm=0;", &mut term);
    assert!(graphics.buffers[0].images.is_empty());
    graphics.apply(b"Ga=T,f=32,s=1,v=1,C=1,c=1,r=1,i=2;/wAA/w==", &mut term);
    assert_eq!(graphics.snapshot(&term).len(), 1);
}

#[test]
fn synchronized_updates_publish_text_and_graphics_together_in_order() {
    let mut core = core();
    core.feed(b"\x1b[?2026h\x1b[3;4H");
    red(&mut core, "i=7,c=2,r=1");
    core.feed(b"x");
    assert!(core.graphic_snapshots().is_empty());
    assert_eq!(core.visible_rows()[2].cells[3].c, ' ');
    core.feed(b"\x1b[?2026l");
    let graphics = core.graphic_snapshots();
    assert_eq!(graphics.len(), 1);
    assert_eq!((graphics[0].row, graphics[0].column), (2, 3));
    assert_eq!(core.visible_rows()[2].cells[3].c, 'x');
}

#[test]
fn every_synchronized_frame_split_preserves_graphics_position() {
    let frame = b"\x1b[?2026h\x1b[4;2H\x1b_Ga=T,f=32,s=1,v=1,C=1,c=2,r=1,i=5,q=2;/wAA/w==\x1b\\z\x1b[?2026l";
    for split in 0..=frame.len() {
        let mut core = core();
        core.feed(&frame[..split]);
        core.feed(&frame[split..]);
        let graphics = core.graphic_snapshots();
        assert_eq!(graphics.len(), 1, "split {split}");
        assert_eq!(
            (graphics[0].row, graphics[0].column),
            (3, 1),
            "split {split}"
        );
    }
}

#[test]
fn unterminated_synchronized_frames_have_a_deadline_and_size_limit() {
    let mut core = core();
    core.feed(b"\x1b[?2026hhello");
    assert_eq!(core.visible_rows()[0].cells[0].c, ' ');
    assert!(core.flush_sync_timeout(std::time::Instant::now() + std::time::Duration::from_secs(1)));
    assert_eq!(core.visible_rows()[0].cells[0].c, 'h');
    core.feed(b"\x1b[?2026h");
    core.feed(&vec![b'x'; crate::terminal::sync_updates::MAX_SYNC_BYTES]);
    assert!(!core.flush_sync_timeout(std::time::Instant::now() + std::time::Duration::from_secs(1)));
}

#[test]
fn opentui_raw_compressed_upload_and_cropped_placement_are_supported() {
    use std::io::Write;
    let mut core = core();
    // OpenTUI's exact startup probe (ansi.zig::kittyGraphicsQuery).
    core.feed(b"\x1b_Gi=31337,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
    assert_eq!(replies(&mut core), ["\x1b_Gi=31337;OK\x1b\\"]);
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder
        .write_all(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255])
        .unwrap();
    let encoded = STANDARD.encode(encoder.finish().unwrap());
    send(
        &mut core,
        "a=t,f=24,s=2,v=2,i=71,o=z,m=0,q=2",
        encoded.as_bytes(),
    );
    core.feed(b"\x1b[2;3H");
    send(
        &mut core,
        "a=p,i=71,p=17,c=2,r=1,x=0,y=1,w=2,h=1,C=1,z=-1073741807,q=2",
        b"",
    );
    let graphics = core.graphic_snapshots();
    assert_eq!(graphics.len(), 1);
    assert_eq!(graphics[0].source, [0, 1, 2, 1]);
    assert_eq!(
        (graphics[0].row, graphics[0].column, graphics[0].z),
        (1, 2, -1073741807)
    );
    assert_eq!(
        &graphics[0].image.as_bytes(0).unwrap()[..4],
        &[0, 0, 255, 255]
    );
    send(&mut core, "a=d,d=i,i=71,p=17,q=2", b"");
    assert!(core.graphic_snapshots().is_empty());
    send(&mut core, "a=p,i=71,p=17,c=2,r=1,C=1,q=2", b"");
    assert_eq!(core.graphic_snapshots().len(), 1);
}

#[test]
fn resident_image_and_placement_quotas_are_enforced() {
    let mut core = core();
    for id in 1..=MAX_IMAGES {
        red(&mut core, &format!("i={id},c=1,r=1"));
    }
    assert_eq!(core.graphic_snapshots().len(), MAX_IMAGES);
    send(&mut core, "a=T,f=32,s=1,v=1,i=999,c=1,r=1,C=1", b"/wAA/w==");
    assert!(replies(&mut core)[0].contains("ENOSPC"));
    send(&mut core, "a=d,d=I,i=1,q=2", b"");
    red(&mut core, "i=999,c=1,r=1");
    assert!(core.graphic_snapshots().iter().any(|g| g.id == 999));
    for _ in MAX_IMAGES..MAX_PLACEMENTS {
        send(&mut core, "a=p,i=999,c=1,r=1,C=1,q=2", b"");
    }
    assert_eq!(core.graphic_snapshots().len(), MAX_PLACEMENTS);
    send(&mut core, "a=p,i=999,c=1,r=1,C=1", b"");
    assert!(replies(&mut core)[0].contains("ENOSPC"));
    assert_eq!(core.graphic_snapshots().len(), MAX_PLACEMENTS);
}

#[test]
fn compressed_expansion_cannot_exceed_the_decoded_budget() {
    use std::io::Write;
    let control = Control::parse(b"f=32,s=8192,v=1024,o=z").unwrap();
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&vec![0; MAX_PIXELS_BYTES + 1]).unwrap();
    let encoded = STANDARD.encode(encoder.finish().unwrap());
    assert!(decode(&control, encoded.as_bytes())
        .unwrap_err()
        .contains("quota"));
}

#[test]
fn decoded_dimensions_and_protocol_alphabet_are_bounded() {
    assert!(pixel_bytes(u32::MAX, u32::MAX).is_err());
    assert!(pixel_bytes(8192, 8192).is_err());
    assert_eq!(DIACRITICS.len(), 297);
    assert!(DIACRITICS.windows(2).all(|w| w[0] < w[1]));
}
