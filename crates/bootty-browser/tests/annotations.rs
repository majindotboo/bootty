#![cfg(test)]

use assert_fs::{TempDir, prelude::*};
use bootty_browser::{
    Annotation, AnnotationAnchor, AnnotationEvent, AnnotationImageGeometry, AnnotationRect,
    AnnotationSelection, AnnotationStore, annotation_batch, apply_annotation_event,
    split_annotation_batch,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::{fixture, rstest};

#[fixture]
fn annotation() -> Annotation {
    Annotation {
        id: 1,
        page: 7,
        address: "https://example.com/page".into(),
        anchor: AnnotationAnchor {
            selector: "p:nth-of-type(1)".into(),
            text: "Example".into(),
            tag: "p".into(),
            selection: None,
        },
        note: "Saved note".into(),
        draft: Some("Unfinished edit".into()),
        conversation: None,
        revision: 0,
        image: None,
        pending_conversation: None,
    }
}

#[fixture]
fn png() -> Vec<u8> {
    let mut output = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2,
        2,
        image::Rgba([12, 34, 56, 255]),
    ))
    .write_to(&mut output, image::ImageFormat::Png)
    .unwrap();
    output.into_inner()
}

#[fixture]
fn image_geometry() -> AnnotationImageGeometry {
    let viewport = AnnotationRect {
        x: 0.0,
        y: 0.0,
        width: 500.0,
        height: 400.0,
    };
    let selection = AnnotationRect {
        x: 10.0,
        y: 20.0,
        width: 2.0,
        height: 2.0,
    };
    let source = AnnotationRect {
        x: -100.0,
        y: 50.0,
        width: 2.0,
        height: 2.0,
    };
    AnnotationImageGeometry {
        viewport,
        selection,
        crop: selection,
        requested_source: source,
        source,
        pixel_width: 2,
        pixel_height: 2,
    }
}

#[rstest]
#[case::region(AnnotationSelection::Region { x:12, y:80, width:48, height:30 }, 48.0, 30.0)]
#[case::drawing(AnnotationSelection::Drawing { points:vec![[12,80],[30,110],[60,90]] }, 48.0, 30.0)]
#[case::horizontal_drawing(AnnotationSelection::Drawing { points:vec![[12,80],[60,80]] }, 48.0, 1.0)]
#[case::vertical_drawing(AnnotationSelection::Drawing { points:vec![[12,80],[12,110]] }, 1.0, 30.0)]
fn capture_accepts_only_persisted_selection_bounds(
    #[case] selection: AnnotationSelection,
    #[case] width: f64,
    #[case] height: f64,
) {
    let mut context = bootty_browser::AnnotationCaptureContext {
        document: "0123456789abcdef0123456789abcdef".into(),
        viewport: AnnotationRect {
            x: 0.0,
            y: 0.0,
            width: 500.0,
            height: 400.0,
        },
        selection: AnnotationRect {
            x: 12.0,
            y: 80.0,
            width,
            height,
        },
    };
    assert!(context.validate_selection(Some(&selection)).is_ok());
    context.selection.x = 13.0;
    assert!(context.validate_selection(Some(&selection)).is_err());
    assert!(context.validate_selection(None).is_ok());
}

#[rstest]
fn visual_attachment_commits_image_before_publishing_and_survives_restart(
    mut annotation: Annotation,
    png: Vec<u8>,
    image_geometry: AnnotationImageGeometry,
) {
    let directory = TempDir::new().unwrap();
    let store = AnnotationStore::new(directory.child("browser").path());
    annotation.attach_to("old-conversation").unwrap();
    annotation
        .prepare_attachment("New visual comment".into(), "captured-conversation")
        .unwrap();
    assert_eq!(annotation.note, "Saved note");
    assert_eq!(annotation.conversation.as_deref(), Some("old-conversation"));
    store
        .commit(&[], std::slice::from_ref(&annotation))
        .unwrap();
    let staged = store.load().unwrap();
    assert_eq!(
        staged
            .first()
            .and_then(|note| note.pending_conversation.as_deref()),
        Some("captured-conversation")
    );
    let (image, _) = store.commit_image(&png, &image_geometry).unwrap();
    let mut attached = annotation.clone();
    attached.finish_attachment(&annotation, image).unwrap();
    store
        .commit(&staged, std::slice::from_ref(&attached))
        .unwrap();
    assert_eq!(
        store
            .load_image(&attached, "captured-conversation")
            .unwrap(),
        png
    );
    assert!(store.load_image(&attached, "old-conversation").is_err());
    let mut replaced = attached.clone();
    replaced
        .prepare_attachment("Changed while sending".into(), "captured-conversation")
        .unwrap();
    store
        .commit(
            std::slice::from_ref(&attached),
            std::slice::from_ref(&replaced),
        )
        .unwrap();
    assert!(
        store
            .load_image(&attached, "captured-conversation")
            .is_err()
    );
    assert_eq!(
        store
            .load()
            .unwrap()
            .first()
            .and_then(|note| note.image.as_ref()),
        replaced.image.as_ref()
    );
}

#[rstest]
fn failed_visual_capture_retains_draft_and_prior_attachment(
    mut annotation: Annotation,
    image_geometry: AnnotationImageGeometry,
) {
    let directory = TempDir::new().unwrap();
    let store = AnnotationStore::new(directory.child("browser").path());
    annotation.attach_to("prior").unwrap();
    annotation
        .prepare_attachment("Unfinished visual change".into(), "captured")
        .unwrap();
    store
        .commit(&[], std::slice::from_ref(&annotation))
        .unwrap();
    assert!(store.commit_image(b"not a PNG", &image_geometry).is_err());
    assert_eq!(store.load().unwrap(), vec![annotation]);
}

#[rstest]
#[case::missing(false)]
#[case::substituted(true)]
fn missing_or_substituted_image_never_resolves_as_text_success(
    mut annotation: Annotation,
    png: Vec<u8>,
    image_geometry: AnnotationImageGeometry,
    #[case] substitute: bool,
) {
    let directory = TempDir::new().unwrap();
    let store = AnnotationStore::new(directory.child("browser").path());
    annotation
        .prepare_attachment("Visual comment".into(), "captured")
        .unwrap();
    let (image, _) = store.commit_image(&png, &image_geometry).unwrap();
    let path = directory.child(format!("browser-annotation-images/{}.png", image.id));
    let intent = annotation.clone();
    annotation.finish_attachment(&intent, image).unwrap();
    store
        .commit(&[], std::slice::from_ref(&annotation))
        .unwrap();
    if substitute {
        path.write_binary(b"substituted").unwrap();
    } else {
        std::fs::remove_file(path.path()).unwrap();
    }
    assert!(store.load_image(&annotation, "captured").is_err());
    assert_eq!(store.load().unwrap(), vec![annotation]);
}

#[rstest]
fn cancelled_or_changed_intent_cannot_publish_late_image(
    mut annotation: Annotation,
    png: Vec<u8>,
    image_geometry: AnnotationImageGeometry,
) {
    let directory = TempDir::new().unwrap();
    let store = AnnotationStore::new(directory.child("browser").path());
    annotation
        .prepare_attachment("Captured change".into(), "captured")
        .unwrap();
    let intent = annotation.clone();
    let (image, _) = store.commit_image(&png, &image_geometry).unwrap();
    annotation
        .prepare_attachment("Newer change".into(), "captured")
        .unwrap();
    let prior = annotation.clone();
    assert!(annotation.finish_attachment(&intent, image).is_err());
    assert_eq!(annotation, prior);
}

#[rstest]
fn cancelled_capture_cannot_publish_a_late_image(
    mut annotation: Annotation,
    png: Vec<u8>,
    image_geometry: AnnotationImageGeometry,
) {
    let directory = TempDir::new().unwrap();
    let store = AnnotationStore::new(directory.child("browser").path());
    annotation
        .prepare_attachment("Captured change".into(), "captured")
        .unwrap();
    let intent = annotation.clone();
    store.commit(&[], std::slice::from_ref(&intent)).unwrap();

    let cancelled = apply_annotation_event(
        std::slice::from_ref(&intent),
        intent.page,
        &intent.address,
        AnnotationEvent::Cancel {
            address: intent.address.clone(),
            id: intent.id.to_string(),
        },
    )
    .unwrap();
    store
        .commit(std::slice::from_ref(&intent), &cancelled)
        .unwrap();
    let (image, _) = store.commit_image(&png, &image_geometry).unwrap();
    let mut late = cancelled
        .first()
        .cloned()
        .expect("missing cancelled annotation");
    let prior = late.clone();

    assert!(late.finish_attachment(&intent, image).is_err());
    assert_eq!(late, prior);
    assert_eq!(store.load().unwrap(), cancelled);
}

#[rstest]
fn cancellation_after_attachment_commit_restores_only_its_record(
    mut annotation: Annotation,
    png: Vec<u8>,
    image_geometry: AnnotationImageGeometry,
) {
    let directory = TempDir::new().unwrap();
    let store = AnnotationStore::new(directory.child("browser").path());
    annotation.attach_to("prior-conversation").unwrap();
    annotation
        .prepare_attachment("New visual comment".into(), "captured-conversation")
        .unwrap();
    let prior = annotation.clone();
    let mut sibling = annotation.clone();
    sibling.id = 2;
    sibling.note = "Other saved note".into();
    sibling.draft = None;
    sibling.conversation = None;
    sibling.revision = 0;
    sibling.pending_conversation = None;
    let before = vec![prior.clone(), sibling.clone()];
    store.commit(&[], &before).unwrap();

    let (image, _) = store.commit_image(&png, &image_geometry).unwrap();
    let mut attached = prior.clone();
    attached.finish_attachment(&prior, image).unwrap();
    let candidate = vec![attached.clone(), sibling];
    let writer_store = store.clone();
    let write_completed = std::sync::Arc::new(std::sync::Barrier::new(2));
    let publish = std::sync::Arc::clone(&write_completed);
    let writer_prior = prior.clone();
    let writer = std::thread::spawn(move || -> Result<Vec<Annotation>, _> {
        writer_store.commit(&before, &candidate)?;
        // Cancellation arrives after the atomic write but before the UI publishes its result.
        publish.wait();
        publish.wait();
        writer_store.restore_attachment_if_unchanged(&attached, &writer_prior)
    });
    write_completed.wait();

    let committed = store.load().unwrap();
    let mut concurrent = committed.clone();
    let other = concurrent
        .iter_mut()
        .find(|record| record.id == 2)
        .expect("missing sibling annotation");
    other.note = "Saved by another window".into();
    store.commit(&committed, &concurrent).unwrap();
    write_completed.wait();

    let restored = writer.join().unwrap().unwrap();
    let expected = vec![prior, concurrent[1].clone()];
    assert_eq!(restored, expected);
    assert_eq!(store.load().unwrap(), expected);
}

#[rstest]
#[case::region(AnnotationSelection::Region { x: 12, y: 80, width: 240, height: 160 })]
#[case::drawing(AnnotationSelection::Drawing { points: vec![[12, 80], [30, 110], [60, 90]] })]
fn geometric_selection_survives_restart_and_attachment(
    mut annotation: Annotation,
    #[case] selection: AnnotationSelection,
) {
    annotation.anchor.selection = Some(selection.clone());
    annotation.attach_to("conversation-1").unwrap();
    let directory = TempDir::new().unwrap();
    let store = AnnotationStore::new(directory.child("browser").path());
    store
        .commit(&[], std::slice::from_ref(&annotation))
        .unwrap();
    let restored = store.load().unwrap();
    assert_eq!(restored, vec![annotation]);
    assert!(
        annotation_batch(&restored)
            .unwrap()
            .contains(&serde_json::to_string(&selection).unwrap())
    );
}

/// A sent message splits back into the words written with it and the notes its batch names,
/// even when a note quotes a line that looks like a batch's.
#[rstest]
#[case::with_words("Fix both", "Fix both")]
#[case::notes_only("", "")]
fn sent_batches_split_back_into_words_and_notes(
    mut annotation: Annotation,
    #[case] words: &str,
    #[case] authored: &str,
) {
    annotation.note = "Annotation 99: not a header".to_owned();
    let mut second = annotation.clone();
    second.id = annotation.id.saturating_add(1);
    let batch = annotation_batch(&[annotation.clone(), second.clone()]).unwrap();
    let message = if words.is_empty() {
        batch
    } else {
        format!("{words}\n\n{batch}")
    };
    let (written, ids) = split_annotation_batch(&message).expect("a batch");
    assert_eq!(written, authored);
    assert!(
        ids.contains(&annotation.id) && ids.contains(&second.id),
        "{ids:?}"
    );
    assert_eq!(split_annotation_batch("Just words"), None);
}

#[rstest]
#[case::empty_region(AnnotationSelection::Region { x: 0, y: 0, width: 0, height: 2 })]
#[case::overflow(AnnotationSelection::Region { x: u32::MAX, y: 0, width: 2, height: 2 })]
#[case::empty_drawing(AnnotationSelection::Drawing { points: vec![] })]
#[case::stationary_drawing(AnnotationSelection::Drawing { points: vec![[1, 2], [1, 2]] })]
#[case::too_many_points(AnnotationSelection::Drawing { points: vec![[1, 2]; 513] })]
fn invalid_geometry_cannot_enter_a_batch(
    mut annotation: Annotation,
    #[case] selection: AnnotationSelection,
) {
    annotation.anchor.selection = Some(selection);
    assert!(annotation_batch(&[annotation]).is_err());
}

#[rstest]
fn restart_retains_notes_and_unfinished_drafts(annotation: Annotation) {
    let directory = TempDir::new().unwrap();
    let store = AnnotationStore::new(directory.child("browser").path());
    store
        .commit(&[], std::slice::from_ref(&annotation))
        .unwrap();
    assert_eq!(
        AnnotationStore::new(directory.child("browser").path())
            .load()
            .unwrap(),
        vec![annotation]
    );
}

#[rstest]
#[case::cancel(AnnotationEvent::Cancel { address: "https://example.com/page".into(), id: "1".into() }, "Saved note")]
#[case::save(AnnotationEvent::Save { address: "https://example.com/page".into(), id: "1".into(), note: "Replacement".into() }, "Replacement")]
fn editor_commits_or_discards_only_the_draft(
    annotation: Annotation,
    #[case] event: AnnotationEvent,
    #[case] note: &str,
) {
    let records =
        apply_annotation_event(&[annotation], 7, "https://example.com/page", event).unwrap();
    assert_eq!(
        records.first().map(|record| record.note.as_str()),
        Some(note)
    );
    assert_eq!(
        records.first().and_then(|record| record.draft.as_ref()),
        None
    );
}

#[rstest]
#[case(8, "https://example.com/page")]
#[case(7, "https://example.com/other")]
fn stale_page_or_navigation_cannot_edit_a_saved_note(
    annotation: Annotation,
    #[case] page: u64,
    #[case] address: &str,
) {
    let event = AnnotationEvent::Save {
        address: address.into(),
        id: "1".into(),
        note: "Replacement".into(),
    };
    assert!(apply_annotation_event(&[annotation], page, address, event).is_err());
}

#[rstest]
fn invalid_candidate_keeps_prior_document(annotation: Annotation) {
    let directory = TempDir::new().unwrap();
    let store = AnnotationStore::new(directory.child("browser").path());
    store
        .commit(&[], std::slice::from_ref(&annotation))
        .unwrap();
    let mut invalid = annotation.clone();
    invalid.note = "x".repeat(4097);
    assert!(
        store
            .commit(std::slice::from_ref(&annotation), &[invalid])
            .is_err()
    );
    assert_eq!(store.load().unwrap(), vec![annotation]);
}

#[rstest]
fn damaged_storage_is_reported_without_overwriting() {
    let directory = TempDir::new().unwrap();
    directory
        .child("browser-annotations.json")
        .write_str("broken")
        .unwrap();
    let store = AnnotationStore::new(directory.child("browser").path());
    assert!(store.load().is_err());
    directory.child("browser-annotations.json").assert("broken");
}

proptest! {
    #[test]
    fn untrusted_page_messages_never_parse_as_agent_delivery(action in "(send|prompt|paste|write|submit|agent)", note in ".{0,100}") {
        let message = serde_json::json!({"action": action, "address": "https://example.com/", "note": note});
        prop_assert!(AnnotationEvent::parse(&message.to_string()).is_none());
    }

    #[test]
    fn drafts_round_trip_as_plain_text(note in "[a-zA-Z0-9 <>#&]{0,100}") {
        let original = annotation();
        let event = AnnotationEvent::Draft { address: original.address.clone(), id: original.id.to_string(), note: note.clone() };
        let records = apply_annotation_event(&[original], 7, "https://example.com/page", event)?;
        prop_assert_eq!(records.first().and_then(|record| record.draft.as_deref()), Some(note.as_str()));
    }
}

#[rstest]
fn batch_excludes_unfinished_drafts_and_keeps_page_identity(annotation: Annotation) {
    let mut other = annotation.clone();
    other.id = 2;
    other.page = 8;
    other.note.clear();
    other.draft = Some("Unfinished other page".into());
    let selected = vec![annotation, other];
    let batch = annotation_batch(&selected).unwrap();
    assert!(batch.contains("Saved note"));
    assert!(!batch.contains("Unfinished edit"));
    assert!(!batch.contains("Unfinished other page"));
    assert!(batch.contains("Page 7:"));
}

#[rstest]
fn identity_directories_have_independent_annotation_documents(annotation: Annotation) {
    let directory = TempDir::new().unwrap();
    let production = AnnotationStore::new(directory.child("production/browser").path());
    let development = AnnotationStore::new(directory.child("development/browser").path());
    production.commit(&[], &[annotation]).unwrap();
    assert_eq!(development.load().unwrap(), Vec::<Annotation>::new());
}

#[rstest]
fn oversized_batch_can_be_reduced_without_losing_saved_notes(annotation: Annotation) {
    let records: Vec<_> = (1..=100)
        .map(|id| {
            let mut record = annotation.clone();
            record.id = id;
            record.note = "n".repeat(1024);
            record
        })
        .collect();
    assert!(matches!(
        annotation_batch(&records),
        Err(bootty_browser::AnnotationError::BatchLimit)
    ));
    let reduced: Vec<_> = records.iter().take(2).cloned().collect();
    assert!(annotation_batch(&reduced).is_ok());
    assert_eq!(records.len(), 100);
    assert_eq!(records.last().map(|record| record.note.len()), Some(1024));
}

#[rstest]
fn cancelling_a_new_draft_does_not_leave_an_empty_annotation() {
    let original = annotation();
    let picked = apply_annotation_event(
        &[],
        7,
        &original.address,
        AnnotationEvent::Pick {
            address: original.address.clone(),
            anchor: original.anchor,
        },
    )
    .unwrap();
    let id = picked.first().expect("missing draft").id.to_string();
    let cancelled = apply_annotation_event(
        &picked,
        7,
        &original.address,
        AnnotationEvent::Cancel {
            address: original.address.clone(),
            id,
        },
    )
    .unwrap();
    assert_eq!(cancelled, Vec::<Annotation>::new());
}

#[rstest]
fn annotation_storage_survives_site_data_directory_removal(annotation: Annotation) {
    let directory = TempDir::new().unwrap();
    let site_data = directory.child("browser");
    site_data.create_dir_all().unwrap();
    let store = AnnotationStore::new(site_data.path());
    store
        .commit(&[], std::slice::from_ref(&annotation))
        .unwrap();
    std::fs::remove_dir(site_data.path()).unwrap();
    assert_eq!(store.load().unwrap(), vec![annotation]);
}

#[rstest]
fn stale_window_snapshot_cannot_overwrite_another_windows_saved_note(annotation: Annotation) {
    let directory = TempDir::new().unwrap();
    let first = AnnotationStore::new(directory.child("browser").path());
    let second = AnnotationStore::new(directory.child("browser").path());
    first
        .commit(&[], std::slice::from_ref(&annotation))
        .unwrap();
    let stale = second.load().unwrap();
    let mut replacement = annotation.clone();
    replacement.note = "Other window edit".into();
    first
        .commit(
            std::slice::from_ref(&annotation),
            std::slice::from_ref(&replacement),
        )
        .unwrap();
    assert!(matches!(
        second.commit(&stale, &[]),
        Err(bootty_browser::AnnotationError::Conflict)
    ));
    assert_eq!(first.load().unwrap(), vec![replacement]);
}

#[rstest]
fn legacy_notes_load_without_inventing_conversation_attachment(annotation: Annotation) {
    let directory = TempDir::new().unwrap();
    let mut legacy = serde_json::to_value(&annotation).unwrap();
    let object = legacy.as_object_mut().expect("annotation is not an object");
    object.remove("conversation");
    object.remove("revision");
    directory
        .child("browser-annotations.json")
        .write_str(&serde_json::to_string(&vec![legacy]).unwrap())
        .unwrap();
    let restored = AnnotationStore::new(directory.child("browser").path())
        .load()
        .unwrap();
    assert_eq!(restored, vec![annotation]);
}

#[rstest]
fn attached_draft_survives_reopen_and_detachment_keeps_the_note(mut annotation: Annotation) {
    let directory = TempDir::new().unwrap();
    let store = AnnotationStore::new(directory.child("browser").path());
    annotation.attach_to("native:codex:7").unwrap();
    store
        .commit(&[], std::slice::from_ref(&annotation))
        .unwrap();
    let mut reopened = store
        .load()
        .unwrap()
        .into_iter()
        .next()
        .expect("missing note");
    assert!(reopened.is_attached_to("native:codex:7"));
    assert!(!reopened.is_attached_to("native:codex:8"));
    assert!(reopened.detach_if_unchanged(&annotation).unwrap());
    store
        .commit(
            std::slice::from_ref(&annotation),
            std::slice::from_ref(&reopened),
        )
        .unwrap();
    assert_eq!(store.load().unwrap(), vec![reopened.clone()]);
    assert_eq!(reopened.note, annotation.note);
    assert_eq!(reopened.conversation, None);
}

#[rstest]
fn stale_send_cannot_detach_an_identical_note_reattached_later(mut annotation: Annotation) {
    annotation.attach_to("native:codex:7").unwrap();
    let submitted = annotation.clone();
    assert!(annotation.detach_if_unchanged(&submitted).unwrap());
    annotation.attach_to("native:codex:7").unwrap();
    assert!(!annotation.detach_if_unchanged(&submitted).unwrap());
    assert!(annotation.is_attached_to("native:codex:7"));
    assert_eq!(annotation.note, submitted.note);
}

proptest! {
    #[test]
    fn edited_saved_notes_survive_old_submission_completion(note in "[a-zA-Z0-9 ]{1,100}") {
        let mut record = annotation();
        record.attach_to("native:codex:7")?;
        let submitted = record.clone();
        record.note = format!("Edited: {note}");
        record.attach_to("native:codex:7")?;
        prop_assert!(!record.detach_if_unchanged(&submitted)?);
        prop_assert!(record.is_attached_to("native:codex:7"));
        prop_assert_eq!(record.note, format!("Edited: {note}"));
    }
}
