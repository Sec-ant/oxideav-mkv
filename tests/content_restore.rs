use oxideav_core::{Demuxer, NullCodecResolver};
use oxideav_mkv::{
    demux::open_typed,
    ebml::{write_element_id, write_vint},
    ids,
};
use std::io::{Cursor, Write};
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
fn compressed(data: &[u8]) -> Vec<u8> {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(data).unwrap();
    z.finish().unwrap()
}
fn encoding(order: u64, scope: u64, algo: u64, settings: &[u8]) -> Vec<u8> {
    element(
        ids::CONTENT_ENCODING,
        &[
            uint(ids::CONTENT_ENCODING_ORDER, order),
            uint(ids::CONTENT_ENCODING_SCOPE, scope),
            element(
                ids::CONTENT_COMPRESSION,
                &[
                    uint(ids::CONTENT_COMP_ALGO, algo),
                    element(ids::CONTENT_COMP_SETTINGS, settings),
                ]
                .concat(),
            ),
        ]
        .concat(),
    )
}
fn header() -> Vec<u8> {
    element(
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
    )
}
fn block(time: i16, data: &[u8]) -> Vec<u8> {
    element(
        ids::SIMPLE_BLOCK,
        &[
            vec![0x81],
            time.to_be_bytes().to_vec(),
            vec![0x80],
            data.to_vec(),
        ]
        .concat(),
    )
}
fn fixture(
    enc: &[u8],
    private: &[u8],
    payloads: &[(i16, Vec<u8>)],
    index: Option<&str>,
) -> Vec<u8> {
    let info = element(ids::INFO, &uint(ids::TIMECODE_SCALE, 1_000_000));
    let tracks = element(
        ids::TRACKS,
        &element(
            ids::TRACK_ENTRY,
            &[
                uint(ids::TRACK_NUMBER, 1),
                uint(ids::TRACK_UID, 1),
                uint(ids::TRACK_TYPE, ids::TRACK_TYPE_SUBTITLE),
                element(ids::CODEC_ID, b"S_HDMV/PGS"),
                element(ids::CODEC_PRIVATE, private),
                if enc.is_empty() {
                    vec![]
                } else {
                    element(ids::CONTENT_ENCODINGS, enc)
                },
            ]
            .concat(),
        ),
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
    let tags = if index.is_some() {
        element(
            ids::TAGS,
            &element(
                ids::TAG,
                &[
                    element(ids::TARGETS, &uint(ids::TAG_TRACK_UID, 1)),
                    tag(
                        "NUMBER_OF_FRAMES",
                        if index == Some("count") { "3" } else { "2" },
                    ),
                    tag(
                        "NUMBER_OF_BYTES",
                        if index == Some("bytes") { "12" } else { "11" },
                    ),
                ]
                .concat(),
            ),
        )
    } else {
        vec![]
    };
    let cluster_position = (info.len() + tracks.len() + tags.len()) as u64;
    let mut body = uint(ids::TIMECODE, 0);
    let mut cues = Vec::new();
    for (i, (time, payload)) in payloads.iter().enumerate() {
        let relative = body.len() as u64;
        body.extend(block(*time, payload));
        if index == Some("missing") && i == 1 {
            continue;
        }
        cues.extend(element(
            ids::CUE_POINT,
            &[
                uint(
                    ids::CUE_TIME,
                    (*time as u64) + u64::from(index == Some("time") && i == 1),
                ),
                element(
                    ids::CUE_TRACK_POSITIONS,
                    &[
                        uint(ids::CUE_TRACK, 1),
                        uint(ids::CUE_CLUSTER_POSITION, cluster_position),
                        uint(
                            ids::CUE_RELATIVE_POSITION,
                            relative + u64::from(index == Some("position") && i == 1),
                        ),
                        if index == Some("duration") || index == Some("duration-overflow") {
                            uint(
                                ids::CUE_DURATION,
                                if index == Some("duration-overflow") {
                                    u64::MAX
                                } else {
                                    500
                                },
                            )
                        } else {
                            vec![]
                        },
                    ]
                    .concat(),
                ),
            ]
            .concat(),
        ));
    }
    [
        header(),
        element(
            ids::SEGMENT,
            &[
                info,
                tracks,
                tags,
                element(ids::CLUSTER, &body),
                if index.is_some() {
                    element(ids::CUES, &cues)
                } else {
                    vec![]
                },
            ]
            .concat(),
        ),
    ]
    .concat()
}
#[test]
fn zlib_header_mixed_chains_and_private_restore_once_in_decode_order() {
    let payload = b"prefix original data";
    let private = b"config original";
    for (enc, data, config) in [
        (
            encoding(0, 3, 0, &[]),
            compressed(payload),
            compressed(private),
        ),
        (
            encoding(0, 1, 3, b"prefix "),
            payload[7..].to_vec(),
            private.to_vec(),
        ),
        (
            [encoding(0, 1, 3, b"prefix "), encoding(1, 1, 0, &[])].concat(),
            compressed(&payload[7..]),
            private.to_vec(),
        ),
        (
            encoding(0, 2, 3, b"config "),
            payload.to_vec(),
            private[7..].to_vec(),
        ),
    ] {
        let bytes = fixture(&enc, &config, &[(1000, data)], None);
        let mut d = open_typed(Box::new(Cursor::new(bytes)), &NullCodecResolver).unwrap();
        assert_eq!(d.streams()[0].params.extradata, private);
        assert_eq!(d.next_packet().unwrap().data, payload);
    }
}
#[test]
fn zlib_content_rejects_truncation_trailing_data_bad_checksum_and_expansion() {
    let expected: Vec<u8> = (0..100_000).map(|i| (i % 251) as u8).collect();
    let good = compressed(&expected);
    let enc = encoding(0, 1, 0, &[]);
    let mut d = open_typed(
        Box::new(Cursor::new(fixture(&enc, &[], &[(0, good.clone())], None))),
        &NullCodecResolver,
    )
    .unwrap();
    assert_eq!(d.next_packet().unwrap().data, expected);
    let mut checksum = good.clone();
    *checksum.last_mut().unwrap() ^= 1;
    let mut trailing = good.clone();
    trailing.push(0);
    for (payload, reason) in [
        (good[..good.len() - 2].to_vec(), "truncated"),
        (checksum, "decompressing"),
        (trailing, "trailing"),
        (compressed(&vec![0; 16 * 1024 * 1024 + 1]), "16 MiB"),
    ] {
        let mut d = open_typed(
            Box::new(Cursor::new(fixture(&enc, &[], &[(0, payload)], None))),
            &NullCodecResolver,
        )
        .unwrap();
        assert!(
            d.next_packet().unwrap_err().to_string().contains(reason),
            "{reason}"
        );
    }
}
#[test]
fn unsupported_transforms_fail_when_consumed_without_hiding_track_metadata() {
    for (scope, algo, reason) in [
        (4, 0, "scope"),
        (8, 0, "scope"),
        (1, 1, "Bzlib"),
        (1, 2, "Lzo1x"),
        (1, 99, "Other"),
    ] {
        let enc = encoding(0, scope, algo, &[]);
        let mut d = open_typed(
            Box::new(Cursor::new(fixture(&enc, &[], &[(0, vec![1])], None))),
            &NullCodecResolver,
        )
        .unwrap();
        assert_eq!(d.streams().len(), 1);
        assert!(
            d.next_packet().unwrap_err().to_string().contains(reason),
            "{reason}"
        );
    }
    let enc = element(
        ids::CONTENT_ENCODING,
        &[
            uint(ids::CONTENT_ENCODING_TYPE, 1),
            element(ids::CONTENT_ENCRYPTION, &uint(ids::CONTENT_ENC_ALGO, 5)),
        ]
        .concat(),
    );
    let mut d = open_typed(
        Box::new(Cursor::new(fixture(&enc, &[], &[(0, vec![1])], None))),
        &NullCodecResolver,
    )
    .unwrap();
    assert!(d
        .next_packet()
        .unwrap_err()
        .to_string()
        .contains("encrypted"));
}
#[test]
fn pgs_index_validates_all_restored_bytes_and_every_durationless_state_change() {
    let enc = encoding(0, 1, 0, &[]);
    let packets = [(1000, compressed(b"display")), (3000, compressed(b"hide"))];
    for index in [
        "valid",
        "count",
        "bytes",
        "missing",
        "time",
        "position",
        "duration",
        "duration-overflow",
    ] {
        let mut d = open_typed(
            Box::new(Cursor::new(fixture(&enc, &[], &packets, Some(index)))),
            &NullCodecResolver,
        )
        .unwrap();
        let got = d.indexed_subtitle_packets(0).unwrap();
        if index == "valid" {
            let got = got.unwrap();
            assert_eq!(got.len(), 2);
            assert_eq!(got[0].data, b"display");
            assert_eq!(got[1].data, b"hide");
            assert_eq!(got[0].pts, Some(1000));
            assert_eq!(got[1].pts, Some(3000));
            assert!(got.iter().all(|p| p.duration.is_none()));
        } else {
            assert!(got.is_none(), "{index}");
        }
        assert_eq!(
            d.next_packet().unwrap().data,
            b"display",
            "index validation must restore sequential reader state"
        );
    }
}
