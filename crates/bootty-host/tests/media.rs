use assert_fs::prelude::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bootty_host::{
    files::{FileRequest, FileResponse},
    media::{MediaDescriptor, MediaKind, MediaReader, serve},
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
use static_assertions::assert_impl_all;
use std::io::{Cursor, Read as _, Seek as _, SeekFrom};

assert_impl_all!(MediaReader: Send, std::io::Read, std::io::Seek);

fn descriptor(path: &std::path::Path) -> anyhow::Result<MediaDescriptor> {
    let FileResponse::Media(descriptor) = (FileRequest::Read {
        path: path.to_string_lossy().into_owned(),
    })
    .execute()?
    else {
        anyhow::bail!("expected media metadata")
    };
    Ok(descriptor)
}
fn payload(descriptor: &MediaDescriptor) -> anyhow::Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(descriptor)?))
}

proptest! {
    #[test]
    fn reads_and_seeks_match_a_file_oracle(tail in prop::collection::vec(any::<u8>(), 0..4096), operations in prop::collection::vec((0_u64..5000, 0_usize..200), 0..50)) {
        let directory = assert_fs::TempDir::new().unwrap();
        let file = directory.child("media");
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend(tail);
        file.write_binary(&bytes).unwrap();
        let mut source = MediaReader::open(&descriptor(file.path()).unwrap(), None).unwrap();
        let mut oracle = Cursor::new(bytes);
        for (offset, length) in operations {
            prop_assert_eq!(source.seek(SeekFrom::Start(offset)).unwrap(), oracle.seek(SeekFrom::Start(offset)).unwrap());
            let mut actual = vec![0; length];
            let mut expected = vec![0; length];
            let count = source.read(&mut actual).unwrap();
            prop_assert_eq!(count, oracle.read(&mut expected).unwrap());
            prop_assert_eq!(actual, expected);
        }
    }
}

#[rstest]
#[case(b"\x89PNG\r\n\x1a\n".as_slice(), MediaKind::Image)]
#[case(b"\0\0\0\x18ftypisom".as_slice(), MediaKind::Video)]
#[case(b"\x1a\x45\xdf\xa3".as_slice(), MediaKind::Video)]
fn large_sources_return_only_metadata_and_seek_without_copying_the_file(
    #[case] header: &[u8],
    #[case] kind: MediaKind,
) {
    use std::io::Write as _;
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.child("source");
    path.write_binary(header).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(path.path())
        .unwrap();
    // Unix set_len creates a sparse fixture; keep other filesystems' allocation small.
    let length = if cfg!(unix) {
        8_u64 * 1024 * 1024 * 1024
    } else {
        3 * 1024 * 1024
    };
    file.set_len(length).unwrap();
    file.seek(SeekFrom::End(-4)).unwrap();
    file.write_all(b"tail").unwrap();
    drop(file);
    let descriptor = descriptor(path.path()).unwrap();
    assert_eq!(descriptor.kind, kind);
    assert_eq!(descriptor.len, length);
    let encoded = serde_json::to_vec(&FileResponse::Media(descriptor.clone())).unwrap();
    assert!(encoded.len() < 1024);
    assert_eq!(
        serde_json::from_slice::<FileResponse>(&encoded).unwrap(),
        FileResponse::Media(descriptor.clone())
    );
    let mut source = MediaReader::open(&descriptor, None).unwrap();
    assert_eq!(source.len(), length);
    assert!(!source.is_empty());
    source.seek(SeekFrom::End(-4)).unwrap();
    let mut tail = [0; 4];
    source.read_exact(&mut tail).unwrap();
    assert_eq!(&tail, b"tail");
    assert_eq!(source.read(&mut tail).unwrap(), 0);
    assert!(source.seek(SeekFrom::Current(i64::MIN)).is_err());
    assert_eq!(source.stream_position().unwrap(), length);
}

#[rstest]
fn stale_descriptors_and_in_place_edits_are_rejected() {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("source");
    file.write_binary(b"GIF89aoriginal").unwrap();
    let descriptor = descriptor(file.path()).unwrap();
    let mut source = MediaReader::open(&descriptor, None).unwrap();
    file.write_binary(b"GIF89areplacement-longer").unwrap();
    assert!(MediaReader::open(&descriptor, None).is_err());
    assert!(source.read(&mut [0; 1]).is_err());
}

#[rstest]
fn cancellation_also_rejects_cached_reads_and_seeks() {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("source");
    file.write_binary(b"GIF89adata").unwrap();
    let mut reader = MediaReader::open(&descriptor(file.path()).unwrap(), None).unwrap();
    reader.read_exact(&mut [0; 1]).unwrap();
    reader.cancellation().cancel();
    assert_eq!(
        reader.read(&mut [0; 1]).unwrap_err().kind(),
        std::io::ErrorKind::ConnectionAborted
    );
    let error = reader.read_exact(&mut [0; 1]).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::ConnectionAborted);
    assert!(reader.seek(SeekFrom::Start(0)).is_err());
}

#[rstest]
fn daemon_stream_serves_multiple_discontiguous_binary_ranges() {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("source");
    let bytes = b"GIF89a\0\xff\nend";
    file.write_binary(bytes).unwrap();
    let descriptor = descriptor(file.path()).unwrap();
    let input =
        b"{\"offset\":9,\"length\":3}\n{\"offset\":0,\"length\":8}\n{\"offset\":12,\"length\":0}\n";
    let mut output = Vec::new();
    serve(
        &payload(&descriptor).unwrap(),
        Cursor::new(input),
        &mut output,
    )
    .unwrap();
    let mut output = std::io::BufReader::new(Cursor::new(output));
    let mut line = String::new();
    std::io::BufRead::read_line(&mut output, &mut line).unwrap();
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ready["len"], bytes.len());
    assert_eq!(ready["revision"], descriptor.revision);
    for expected in [&bytes[9..], &bytes[..8], &bytes[12..]] {
        line.clear();
        std::io::BufRead::read_line(&mut output, &mut line).unwrap();
        let header: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(header["length"], expected.len());
        let mut body = vec![0; expected.len()];
        output.read_exact(&mut body).unwrap();
        assert_eq!(body, expected);
    }
    assert_eq!(output.read(&mut [0; 1]).unwrap(), 0);
}

#[rstest]
#[case("{\"offset\":0,\"length\":1048577}\n")]
#[case("{\"offset\":18446744073709551615,\"length\":1}\n")]
#[case("{\"offset\":9,\"length\":2}\n")]
#[case("{\"offset\":0,\"length\":1}")]
#[case("not-json\n")]
fn daemon_rejects_invalid_ranges_before_sending_a_body(#[case] request: &str) {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("source");
    file.write_binary(b"GIF89adata").unwrap();
    let mut output = Vec::new();
    assert!(
        serve(
            &payload(&descriptor(file.path()).unwrap()).unwrap(),
            Cursor::new(request.as_bytes()),
            &mut output
        )
        .is_err()
    );
    let response = String::from_utf8(output).unwrap();
    let lines: Vec<serde_json::Value> = response
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["status"], "ready");
    assert_eq!(lines[1]["status"], "error");
}
