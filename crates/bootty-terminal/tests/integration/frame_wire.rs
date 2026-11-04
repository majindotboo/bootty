use bootty_terminal::{frame_wire::*, geometry::TerminalGeometry, terminal_engine::TerminalEngine};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
fn engine(cols: u16) -> TerminalEngine {
    TerminalEngine::new(TerminalGeometry {
        cols,
        rows: 12,
        cell_width: 10,
        cell_height: 20,
    })
    .unwrap()
}
fn roundtrip(engine: &mut TerminalEngine, encoder: &mut FrameEncoder, decoder: &mut FrameDecoder) {
    let source = engine.extract_frame().unwrap();
    let update = encoder.encode(source);
    for content in encoder.take_image_contents() {
        decoder
            .insert_image(decode_image_content(&encode_image_content(&content).unwrap()).unwrap())
            .unwrap();
    }
    let frame = decoder
        .apply(decode_update(&encode_update(&update).unwrap()).unwrap())
        .unwrap();
    assert_eq!(frame.cells, source.cells);
    assert_eq!(frame.text, source.text);
    assert_eq!(frame.row_wraps, source.row_wraps);
    assert_eq!(frame.cursor, source.cursor);
    assert_eq!(frame.selections, source.selections);
    assert_eq!(frame.images, source.images);
    assert_eq!(frame.scrollbar, source.scrollbar);
    assert_eq!(
        encode_update(&FrameEncoder::default().encode(&frame)).unwrap(),
        encode_update(&FrameEncoder::default().encode(source)).unwrap()
    );
}
proptest! {
 #![proptest_config(ProptestConfig { failure_persistence:None,..ProptestConfig::default() })]
 #[test]
 fn arbitrary_stream_frames_roundtrip(chunks in prop::collection::vec((prop::collection::vec(any::<u8>(),0..100),any::<bool>()),0..40)) {
 let mut engine=engine(40);let mut encoder=FrameEncoder::default();let mut decoder=FrameDecoder::default();
 for (bytes,send) in chunks { engine.write_vt(&bytes); let _=engine.extract_frame().unwrap(); if send {roundtrip(&mut engine,&mut encoder,&mut decoder);} }
 roundtrip(&mut engine,&mut encoder,&mut decoder);
 }
}
#[rstest]
#[case([b"\t\t\t\t\t".as_slice(), &[b' ';39], "\nあ".as_bytes()].concat())]
#[case([&[b' ';40], "䀀\x08 \0".as_bytes()].concat())]
#[case([vec![32;24],vec![214,145],vec![32;23],vec![195,128],vec![32;30],vec![204,128,32,10,227,128,128]].concat())]
#[case([vec![32;24],vec![214,145],vec![32;24],vec![195,128],vec![32;32],vec![204,128,32,10,227,128,128]].concat())]
#[case(vec![32, 32, 32, 32, 32, 32, 32, 32, 9, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 32, 13, 224, 179, 128, 0, 0, 0, 14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 14, 14, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 14, 14, 0, 0, 0, 0, 0, 0, 14, 0, 0, 0, 0, 0])]
fn snapshot_regressions_roundtrip(#[case] bytes: Vec<u8>) {
    let mut e = engine(40);
    e.write_vt(&bytes);
    roundtrip(
        &mut e,
        &mut FrameEncoder::default(),
        &mut FrameDecoder::default(),
    );
}
#[rstest]
fn rejected_updates_are_atomic() {
    let mut e = engine(40);
    let mut encoder = FrameEncoder::default();
    let mut decoder = FrameDecoder::default();
    roundtrip(&mut e, &mut encoder, &mut decoder);
    let before = decoder.frame().unwrap().clone();
    let update = encoder.encode(e.extract_frame().unwrap());
    let mut bytes = encode_update(&update).unwrap();
    bytes.pop();
    assert!(decode_update(&bytes).is_err());
    let mut bad = update.clone();
    bad.rows = FrameRows::Rows {
        base: 0,
        rows: vec![],
    };
    assert_eq!(decoder.apply(bad).unwrap_err(), WireError::WrongBase);
    let mut bad = update.clone();
    bad.rows = FrameRows::Rows {
        base: 1,
        rows: vec![(
            12,
            WireRow {
                cells: vec![],
                text: vec![],
            },
        )],
    };
    assert_eq!(decoder.apply(bad).unwrap_err(), WireError::InvalidRow);
    assert!(std::sync::Arc::ptr_eq(&before, decoder.frame().unwrap()));
    let next = decoder.apply(update).unwrap();
    assert!(next.lineage.follows(&before.lineage));
    encoder.reset();
    let full = decoder
        .apply(encoder.encode(e.extract_frame().unwrap()))
        .unwrap();
    assert!(!full.lineage.follows(&next.lineage));
}
#[rstest]
fn geometry_forces_full() {
    let mut encoder = FrameEncoder::default();
    let _ = encoder.encode(engine(40).extract_frame().unwrap());
    assert!(matches!(
        encoder.encode(engine(41).extract_frame().unwrap()).rows,
        FrameRows::Full(_)
    ));
}

#[rstest]
fn images_travel_separately() {
    let mut e = engine(40);
    e.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/////w==\x1b\\");
    let mut encoder = FrameEncoder::default();
    let mut decoder = FrameDecoder::default();
    let source = e.extract_frame().unwrap();
    assert_eq!(source.images.placements.len(), 1);
    let update = encoder.encode(source);
    assert!(matches!(
        decoder.apply(update.clone()),
        Err(WireError::MissingImage(_))
    ));
    assert!(decoder.frame().is_none());
    let contents = encoder.take_image_contents();
    assert_eq!(contents.len(), 1);
    for content in contents {
        decoder
            .insert_image(decode_image_content(&encode_image_content(&content).unwrap()).unwrap())
            .unwrap();
    }
    assert_eq!(decoder.apply(update).unwrap().images, source.images);
    let _ = encoder.encode(source);
    assert!(encoder.take_image_contents().is_empty());
}

#[rstest]
fn invalid_offsets_are_atomic() {
    let mut e = engine(40);
    e.write_vt(b"text");
    let mut encoder = FrameEncoder::default();
    let mut decoder = FrameDecoder::default();
    let mut update = encoder.encode(e.extract_frame().unwrap());
    let FrameRows::Full(rows) = &mut update.rows else {
        panic!("first update must be Full")
    };
    rows.first_mut()
        .unwrap()
        .cells
        .first_mut()
        .unwrap()
        .text_len = usize::MAX;
    assert_eq!(decoder.apply(update).unwrap_err(), WireError::InvalidFrame);
    assert!(decoder.frame().is_none());
}

#[rstest]
fn unchanged_presentation_does_not_advance_wire_base() {
    let mut engine = engine(80);
    let mut encoder = FrameEncoder::default();
    let mut decoder = FrameDecoder::default();
    let initial = engine.extract_frame().unwrap().clone();
    decoder
        .apply(encoder.encode_changed(&initial).unwrap())
        .unwrap();
    for _ in 0..100 {
        let frame = engine.extract_frame().unwrap();
        assert!(encoder.encode_changed(frame).is_none());
    }
    engine.write_vt(b"changed");
    let update = encoder
        .encode_changed(engine.extract_frame().unwrap())
        .unwrap();
    let frame = decoder.apply(update).unwrap();
    assert!(frame.text.iter().collect::<String>().contains("changed"));
}

#[rstest]
fn state_only_changes_are_published_but_diagnostics_are_not() {
    let mut engine = engine(80);
    let mut encoder = FrameEncoder::default();
    let mut frame = engine.extract_frame().unwrap().clone();
    assert!(encoder.encode_changed(&frame).is_some());
    frame.stats.extraction_us = frame.stats.extraction_us.saturating_add(10);
    frame.row_dirty.fill(true);
    assert!(encoder.encode_changed(&frame).is_none());
    frame.current_working_directory = Some("/new".into());
    assert!(encoder.encode_changed(&frame).is_some());
    assert!(encoder.encode_changed(&frame).is_none());
    encoder.reset();
    assert!(matches!(
        encoder.encode_changed(&frame).unwrap().rows,
        FrameRows::Full(_)
    ));
}

#[rstest]
fn output_flood_coalesces_extractions_under_simulated_time() {
    use bootty_terminal::terminal_session::{
        PublishHold, output_settling, should_publish_frame_after_work,
        sync_output_suppresses_publish,
    };
    use std::time::Duration;
    let mut engine = engine(80);
    let mut last_publish = Duration::ZERO;
    let mut extractions = 0_u32;
    for tick in 1..=1000 {
        let now = Duration::from_millis(tick);
        engine.write_vt(b"output\r\n");
        let hold = if sync_output_suppresses_publish(false, false, Duration::ZERO) {
            PublishHold::SyncOutput
        } else if output_settling(Duration::ZERO, now.saturating_sub(last_publish)) {
            PublishHold::Settling
        } else {
            PublishHold::None
        };
        if should_publish_frame_after_work(
            true,
            false,
            hold,
            8192,
            Duration::ZERO,
            now.saturating_sub(last_publish),
        ) {
            engine.extract_frame().unwrap();
            extractions = extractions.saturating_add(1);
            last_publish = now;
        }
    }
    assert!(extractions > 0);
    assert!(
        extractions <= 63,
        "{extractions} extractions for 1000 chunks"
    );
    for tick in 1001..=2000 {
        assert!(!should_publish_frame_after_work(
            false,
            false,
            PublishHold::None,
            0,
            Duration::from_millis(tick),
            Duration::from_millis(tick)
        ));
    }
}

#[rstest]
#[case(false)]
#[case(true)]
fn image_replacements_release_old_content(#[case] distinct_ids: bool) {
    let mut e = engine(40);
    e.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/////w==\x1b\\");
    let mut source = e.extract_frame().unwrap().clone();
    let mut encoder = FrameEncoder::default();
    let mut decoder = FrameDecoder::default();
    let mut previous_data = std::sync::Weak::new();
    for generation in 0..1000_u32 {
        let placement = &mut source.images.placements[0];
        placement.image_id = if distinct_ids { generation } else { 1 };
        placement.data = std::sync::Arc::new(generation.to_le_bytes().to_vec());
        let data = placement.data.clone();
        let update = encoder.encode(&source);
        let contents = encoder.take_image_contents();
        assert_eq!(contents.len(), 1);
        for content in contents {
            decoder.insert_image(content).unwrap();
        }
        assert_eq!(decoder.apply(update).unwrap().images, source.images);
        assert_eq!(decoder.retained_image_bytes(), 4);
        assert!(previous_data.upgrade().is_none());
        previous_data = std::sync::Arc::downgrade(&data);
    }
}

#[rstest]
#[case(false)]
#[case(true)]
fn evicted_images_are_resent(#[case] reset: bool) {
    let mut e = engine(40);
    e.write_vt(b"\x1b_Ga=T,f=32,s=1,v=1,i=1;/////w==\x1b\\");
    let source = e.extract_frame().unwrap().clone();
    let mut encoder = FrameEncoder::default();
    let mut decoder = FrameDecoder::default();
    let update = encoder.encode(&source);
    for content in encoder.take_image_contents() {
        decoder.insert_image(content).unwrap();
    }
    decoder.apply(update).unwrap();
    if reset {
        encoder.reset();
        decoder = FrameDecoder::default();
    } else {
        let mut empty = source.clone();
        empty.images.placements.clear();
        decoder.apply(encoder.encode(&empty)).unwrap();
        assert_eq!(decoder.retained_image_bytes(), 0);
        assert!(encoder.take_image_contents().is_empty());
    }
    let update = encoder.encode(&source);
    let contents = encoder.take_image_contents();
    assert_eq!(contents.len(), 1);
    for content in contents {
        decoder.insert_image(content).unwrap();
    }
    assert_eq!(decoder.apply(update).unwrap().images, source.images);
}

#[rstest]
fn unsolicited_images_exceeding_budget_are_rejected() {
    let mut decoder = FrameDecoder::default();
    for generation in 0..4 {
        decoder
            .insert_image(ImageContent {
                key: ImageKey {
                    image_id: 1,
                    generation,
                },
                bytes: vec![0; MAX_RETAINED_IMAGE_BYTES / 4 - 128],
            })
            .unwrap();
    }
    assert_eq!(
        decoder.insert_image(ImageContent {
            key: ImageKey {
                image_id: 1,
                generation: 4
            },
            bytes: vec![0; 1024],
        }),
        Err(WireError::ImageBudget)
    );
    assert_eq!(
        decoder.retained_image_bytes(),
        MAX_RETAINED_IMAGE_BYTES - 512
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn frame_image_budget_and_full_budget_swap(#[case] over_budget: bool) {
    let mut e = engine(40);
    e.write_vt(b"text\x1b_Ga=T,f=32,s=1,v=1,i=1;/////w==\x1b\\");
    let mut source = e.extract_frame().unwrap().clone();
    let entry = size_of::<(ImageKey, std::sync::Arc<Vec<u8>>, Vec<u8>)>();
    let mut encoder = FrameEncoder::default();
    let mut decoder = FrameDecoder::default();
    for generation in 0_u32..if over_budget { 1 } else { 2 } {
        let mut placement = source.images.placements[0].clone();
        placement.data =
            std::sync::Arc::new(vec![0; (MAX_FRAME_IMAGE_BYTES / 8).saturating_sub(entry)]);
        source.images.placements = (0..8)
            .map(|id| {
                let mut p = placement.clone();
                p.image_id = generation.saturating_mul(8).saturating_add(id);
                p
            })
            .collect();
        if over_budget {
            let mut extra = source.images.placements[0].clone();
            extra.image_id = 100;
            extra.data = std::sync::Arc::new(vec![1; 4]);
            source.images.placements.truncate(8);
            source.images.placements.push(extra.clone());
            source.images.placements.push(extra);
        }
        let update = encoder.encode(&source);
        let contents = encoder.take_image_contents();
        assert!(
            contents
                .iter()
                .map(|c| c.bytes.len().saturating_add(entry))
                .sum::<usize>()
                <= MAX_FRAME_IMAGE_BYTES
        );
        for content in contents {
            decoder.insert_image(content).unwrap();
        }
        let expected = &source.images.placements[..8];
        assert_eq!(decoder.apply(update).unwrap().images.placements, expected);
        for text in ['a', 'b'] {
            source.text[0] = text;
            let update = encoder.encode(&source);
            assert!(matches!(&update.rows, FrameRows::Rows { rows, .. } if rows.len() == 1));
            assert!(encoder.take_image_contents().is_empty());
            let decoded = decoder.apply(update).unwrap();
            assert_eq!(decoded.images.placements, expected);
            assert_eq!(decoded.text, source.text);
        }
    }
}

#[rstest]
#[case(0)]
#[case(255)]
fn image_bytes_have_bounded_wire_overhead(#[case] byte: u8) {
    let content = ImageContent {
        key: ImageKey {
            image_id: 1,
            generation: 1,
        },
        bytes: vec![byte; 64 * 1024],
    };
    let encoded = encode_image_content(&content).unwrap();
    assert!(encoded.len() < content.bytes.len().saturating_add(128));
    assert_eq!(decode_image_content(&encoded).unwrap().bytes, content.bytes);
}

#[rstest]
fn oversized_image_keeps_text_and_small_placement() {
    let mut e = engine(40);
    e.write_vt(b"text\x1b_Ga=T,f=32,s=1,v=1,i=1;/////w==\x1b\\");
    let mut source = e.extract_frame().unwrap().clone();
    let small = source.images.placements[0].clone();
    let mut large = small.clone();
    large.image_id = 2;
    large.data = std::sync::Arc::new(vec![0; 17 * 1024 * 1024]);
    source.images.placements = vec![large, small.clone()];
    let mut encoder = FrameEncoder::default();
    let mut decoder = FrameDecoder::default();
    let update = encoder.encode(&source);
    let contents = encoder.take_image_contents();
    assert_eq!(contents.len(), 1);
    for content in contents {
        decoder
            .insert_image(decode_image_content(&encode_image_content(&content).unwrap()).unwrap())
            .unwrap();
    }
    let decoded = decoder
        .apply(decode_update(&encode_update(&update).unwrap()).unwrap())
        .unwrap();
    assert_eq!(decoded.images.placements, vec![small]);
    assert_eq!(decoded.text, source.text);
}

#[rstest]
#[case("1", Some(42))]
#[case("0", None)]
fn progress_only_changes_survive_frame_diff(#[case] state: &str, #[case] value: Option<u8>) {
    let mut engine = engine(40);
    let mut encoder = FrameEncoder::default();
    let mut decoder = FrameDecoder::default();
    let mut frame = engine.extract_frame().unwrap().clone();
    let initial = decoder.apply(encoder.encode(&frame)).unwrap();
    frame.progress = Some((state.to_owned(), value));
    assert!(!initial.same_presentation(&frame));
    let update = encoder.encode(&frame);
    let decoded = decoder
        .apply(decode_update(&encode_update(&update).unwrap()).unwrap())
        .unwrap();
    assert_eq!(decoded.progress, frame.progress);
    assert!(decoded.same_presentation(&frame));
}
