use oxideav_core::{Demuxer, Error, NullCodecResolver, Packet};
use oxideav_mkv::{
    demux::{open_typed, MkvDemuxer},
    ebml::{crc32_ieee, write_element_id, write_vint},
    ids,
};
use std::{
    io::{Cursor, Read, Seek, SeekFrom},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

fn element(id: u32, body: &[u8]) -> Vec<u8> {
    [
        write_element_id(id),
        write_vint(body.len() as u64, 0),
        body.to_vec(),
    ]
    .concat()
}

fn uint(id: u32, value: u64) -> Vec<u8> {
    element(id, &value.to_be_bytes())
}

fn track(number: u64, kind: u64, codec: &str) -> Vec<u8> {
    element(
        ids::TRACK_ENTRY,
        &[
            uint(ids::TRACK_NUMBER, number),
            uint(ids::TRACK_UID, number),
            uint(ids::TRACK_TYPE, kind),
            element(ids::CODEC_ID, codec.as_bytes()),
        ]
        .concat(),
    )
}

fn block(track: u64, time: i16, payload: &[u8], group: bool) -> Vec<u8> {
    let body = [
        write_vint(track, 0),
        time.to_be_bytes().to_vec(),
        vec![0x80],
        payload.to_vec(),
    ]
    .concat();
    if group {
        element(
            ids::BLOCK_GROUP,
            &[element(ids::BLOCK, &body), uint(ids::BLOCK_DURATION, 400)].concat(),
        )
    } else {
        element(ids::SIMPLE_BLOCK, &body)
    }
}

#[derive(Clone, Copy, Default, Debug)]
enum Index {
    #[default]
    Valid,
    Missing,
    Duplicate,
    Time,
    Duration,
    Position,
    Bytes,
    NoStatistics,
}

fn fixture(crc: bool, index: Index) -> Vec<u8> {
    let header = element(
        ids::EBML_HEADER,
        &[
            uint(ids::EBML_VERSION, 1),
            uint(ids::EBML_READ_VERSION, 1),
            uint(ids::EBML_MAX_ID_LENGTH, 4),
            uint(ids::EBML_MAX_SIZE_LENGTH, 8),
            element(ids::EBML_DOC_TYPE, b"matroska"),
            uint(ids::EBML_DOC_TYPE_VERSION, 4),
            uint(ids::EBML_DOC_TYPE_READ_VERSION, 2),
        ]
        .concat(),
    );
    let info = element(ids::INFO, &uint(ids::TIMECODE_SCALE, 1_000_000));
    let tracks = element(
        ids::TRACKS,
        &[
            track(1, ids::TRACK_TYPE_VIDEO, "V_VP9"),
            track(2, ids::TRACK_TYPE_SUBTITLE, "S_TEXT/UTF8"),
        ]
        .concat(),
    );
    let tag = |name: &str, value: &str| {
        element(
            ids::SIMPLE_TAG,
            &[
                element(ids::TAG_NAME, name.as_bytes()),
                element(ids::TAG_STRING, value.as_bytes()),
            ]
            .concat(),
        )
    };
    let tags = if matches!(index, Index::NoStatistics) {
        Vec::new()
    } else {
        element(
            ids::TAGS,
            &element(
                ids::TAG,
                &[
                    element(ids::TARGETS, &uint(ids::TAG_TRACK_UID, 2)),
                    tag("NUMBER_OF_FRAMES", "40"),
                    tag(
                        "NUMBER_OF_BYTES",
                        if matches!(index, Index::Bytes) {
                            "281"
                        } else {
                            "280"
                        },
                    ),
                ]
                .concat(),
            ),
        )
    };
    let cluster_position = (info.len() + tracks.len() + tags.len()) as u64;
    let mut body = uint(ids::TIMECODE, 0);
    let mut positions = Vec::new();
    for i in 0..40 {
        body.extend(block(1, i * 500, &vec![0x55; 65536], i % 2 == 0));
        positions.push(body.len() as u64 + if crc { 6 } else { 0 });
        body.extend(block(2, i * 500, b"caption", true));
    }
    if crc {
        body = [element(ids::CRC32, &crc32_ieee(&body).to_le_bytes()), body].concat();
    }
    let cluster = element(ids::CLUSTER, &body);
    let mut cues = Vec::new();
    for i in 0..40 {
        if matches!(index, Index::Missing) && i == 20 {
            continue;
        }
        let pos = if matches!(index, Index::Duplicate) && i == 20 {
            positions[19]
        } else if matches!(index, Index::Position) && i == 20 {
            positions[i] + 1
        } else {
            positions[i]
        };
        let time = i as u64 * 500 + u64::from(matches!(index, Index::Time) && i == 20);
        let duration = 400 + u64::from(matches!(index, Index::Duration) && i == 20);
        cues.extend(element(
            ids::CUE_POINT,
            &[
                uint(ids::CUE_TIME, time),
                element(
                    ids::CUE_TRACK_POSITIONS,
                    &[
                        uint(ids::CUE_TRACK, 2),
                        uint(ids::CUE_CLUSTER_POSITION, cluster_position),
                        uint(ids::CUE_RELATIVE_POSITION, pos),
                        uint(ids::CUE_DURATION, duration),
                    ]
                    .concat(),
                ),
            ]
            .concat(),
        ));
    }
    [
        header,
        element(
            ids::SEGMENT,
            &[info, tracks, tags, cluster, element(ids::CUES, &cues)].concat(),
        ),
    ]
    .concat()
}

struct Metered {
    inner: Cursor<Vec<u8>>,
    read: Arc<AtomicU64>,
}
impl Read for Metered {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(out)?;
        self.read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}
impl Seek for Metered {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(from)
    }
}
fn open(bytes: Vec<u8>) -> (MkvDemuxer, Arc<AtomicU64>) {
    let read = Arc::new(AtomicU64::new(0));
    let demuxer = open_typed(
        Box::new(Metered {
            inner: Cursor::new(bytes),
            read: read.clone(),
        }),
        &NullCodecResolver,
    )
    .unwrap();
    (demuxer, read)
}
fn drain(demuxer: &mut MkvDemuxer) -> Vec<Packet> {
    let mut packets = Vec::new();
    loop {
        match demuxer.next_packet() {
            Ok(packet) => packets.push(packet),
            Err(Error::Eof) => return packets,
            Err(error) => panic!("{error}"),
        }
    }
}
fn assert_packets(actual: &[Packet], expected: &[Packet]) {
    assert_eq!(actual.len(), expected.len());
    for (a, b) in actual.iter().zip(expected) {
        assert_eq!(
            (a.stream_index, a.pts, a.duration, &a.data),
            (b.stream_index, b.pts, b.duration, &b.data)
        );
    }
}

#[test]
fn selected_packets_match_full_demux_with_sparse_payload_reads() {
    for crc in [false, true] {
        let bytes = fixture(crc, Index::Valid);
        let (mut full, full_read) = open(bytes.clone());
        let expected = drain(&mut full)
            .into_iter()
            .filter(|p| p.stream_index == 1)
            .collect::<Vec<_>>();
        if !crc {
            assert!(full_read.load(Ordering::Relaxed) < bytes.len() as u64 + 20000);
        } else {
            assert!(full
                .crc_status()
                .iter()
                .any(|s| s.element_id == ids::CLUSTER && s.is_valid()));
        }
        let (mut selected, selected_read) = open(bytes.clone());
        selected.set_active_streams(&[1]);
        assert_packets(&drain(&mut selected), &expected);
        assert!(selected_read.load(Ordering::Relaxed) < bytes.len() as u64 / 50);
        assert!(selected
            .crc_status()
            .iter()
            .all(|s| s.element_id != ids::CLUSTER));
    }
}

#[test]
fn indexed_packets_match_sequential_packets_and_preserve_position() {
    let bytes = fixture(false, Index::Valid);
    let (mut demuxer, _) = open(bytes.clone());
    let first = demuxer.next_packet().unwrap();
    let indexed = demuxer
        .indexed_subtitle_packets(1)
        .unwrap()
        .unwrap_or_else(|| {
            panic!(
                "complete index: {:?}, cues={}",
                demuxer.metadata(),
                demuxer.cue_points().len()
            )
        });
    let mut all = vec![first];
    all.extend(drain(&mut demuxer));
    let subtitles = all
        .iter()
        .filter(|p| p.stream_index == 1)
        .cloned()
        .collect::<Vec<_>>();
    assert_packets(&indexed, &subtitles);
    let (mut indexed_only, _) = open(bytes.clone());
    assert!(indexed_only.indexed_subtitle_packets(0).unwrap().is_none());
    let (mut indexed_only, reads) = open(bytes.clone());
    assert_packets(
        &indexed_only.indexed_subtitle_packets(1).unwrap().unwrap(),
        &subtitles,
    );
    assert!(reads.load(Ordering::Relaxed) < bytes.len() as u64 / 50);
}

#[test]
fn incomplete_or_stale_indexes_preserve_sequential_fallback() {
    for mode in [
        Index::Missing,
        Index::Duplicate,
        Index::Time,
        Index::Duration,
        Index::Position,
        Index::Bytes,
        Index::NoStatistics,
    ] {
        let bytes = fixture(false, mode);
        let (mut demuxer, _) = open(bytes.clone());
        let indexed = demuxer.indexed_subtitle_packets(1);
        assert!(indexed.unwrap().is_none(), "{mode:?}");
        demuxer.set_active_streams(&[1]);
        let actual = drain(&mut demuxer);
        let (mut baseline, _) = open(bytes);
        let expected = drain(&mut baseline)
            .into_iter()
            .filter(|p| p.stream_index == 1)
            .collect::<Vec<_>>();
        assert_packets(&actual, &expected);
        assert_eq!(actual.len(), 40);
    }
}
