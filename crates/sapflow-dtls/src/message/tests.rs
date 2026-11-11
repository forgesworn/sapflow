use super::*;
use crate::testing::{self, Options, Renegotiation};

fn negotiate(o: &Options) -> Result<Negotiated, Error> {
    ClientHello::parse(&o.hello_body(&[9; 32], &[]))?.negotiate()
}

/// A2.2: every captured ClientHello parses, and negotiation picks the
/// suite's profile the design names: GCM where offered, else CM-80.
#[test]
fn captured_hellos_negotiate() {
    for &(dir, stem, profile) in testing::CAPTURES {
        let body = testing::capture_hello(dir, stem);
        let hello = ClientHello::parse(&body).unwrap_or_else(|e| panic!("{dir}/{stem}: {e}"));
        assert!(hello.offers_suite(SUITE), "{dir}");
        let n = hello
            .negotiate()
            .unwrap_or_else(|e| panic!("{dir}/{stem}: {e}"));
        assert_eq!(n.profile, profile, "{dir}/{stem}");
        assert!(hello.extended_master_secret);
        assert_eq!(hello.use_srtp.map(|s| s.mki_len), Some(0));
    }
}

#[test]
fn captured_hello_details() {
    let chrome = testing::capture_hello("chrome-157-desktop-macos", "clienthello");
    assert_eq!(chrome.len(), 1413);
    let h = ClientHello::parse(&chrome).expect("parse");
    assert_eq!(h.renegotiation_info, Some(0));
    let obs = testing::capture_hello("obs-32.0.4-macos", "clienthello");
    let h = ClientHello::parse(&obs).expect("parse");
    assert_eq!(h.renegotiation_info, None, "OBS signals with the SCSV only");
    assert!(h.offers_suite(RENEGOTIATION_SCSV));
    assert_eq!(
        h.negotiate().expect("ok").profile,
        SrtpProfile::Aes128CmHmacSha1_80
    );
}

#[test]
fn every_prefix_of_a_hello_is_refused_without_panic() {
    let chrome = testing::capture_hello("chrome-157-desktop-macos", "clienthello");
    for n in 0..chrome.len() {
        let r = ClientHello::parse(&chrome[..n]).and_then(|h| h.negotiate());
        assert!(r.is_err(), "prefix {n} accepted");
    }
}

#[test]
fn refusals_carry_the_right_error_and_alert() {
    use Error::*;
    let d = Options::default;
    let cases: Vec<(&str, Options, Error, u8)> = vec![
        (
            "DTLS 1.0 only",
            Options {
                client_version: 0xfeff,
                ..d()
            },
            Version,
            70,
        ),
        (
            "TLS, not DTLS",
            Options {
                client_version: 0x0303,
                ..d()
            },
            Version,
            70,
        ),
        (
            "no suite",
            Options {
                suites: vec![0xc02f, 0x1301],
                ..d()
            },
            NoCipherSuite,
            40,
        ),
        (
            "no P-256",
            Options {
                groups: Some(vec![0x001d, 0x0018]),
                ..d()
            },
            NoGroup,
            40,
        ),
        (
            "no groups",
            Options {
                groups: None,
                ..d()
            },
            NoGroup,
            40,
        ),
        (
            "no ecdsa_secp256r1_sha256",
            Options {
                signature_algorithms: Some(vec![0x0503, 0x0804]),
                ..d()
            },
            NoSignatureScheme,
            40,
        ),
        (
            "no sigalgs",
            Options {
                signature_algorithms: None,
                ..d()
            },
            NoSignatureScheme,
            40,
        ),
        (
            "no EMS",
            Options {
                extended_master_secret: false,
                ..d()
            },
            NoExtendedMasterSecret,
            40,
        ),
        (
            "no RFC 5746 signal",
            Options {
                renegotiation: Renegotiation::None,
                ..d()
            },
            NoRenegotiationSignal,
            40,
        ),
        (
            "non-empty renegotiated_connection",
            Options {
                renegotiation: Renegotiation::NonEmpty,
                ..d()
            },
            RenegotiationInfo,
            40,
        ),
        (
            "no use_srtp",
            Options {
                srtp_profiles: None,
                ..d()
            },
            NoSrtpProfile,
            40,
        ),
        (
            "only unsupported SRTP profiles",
            Options {
                srtp_profiles: Some(vec![0x0002, 0x0008]),
                ..d()
            },
            NoSrtpProfile,
            40,
        ),
        (
            "an MKI",
            Options {
                mki: vec![1, 2],
                ..d()
            },
            SrtpMki,
            40,
        ),
        (
            "compressed points only",
            Options {
                point_formats: Some(vec![1, 2]),
                ..d()
            },
            PointFormat,
            47,
        ),
        (
            "a duplicate extension",
            Options {
                extra: vec![(0x0017, vec![])],
                ..d()
            },
            DuplicateExtension,
            47,
        ),
        (
            "a duplicate unknown extension",
            Options {
                extra: vec![(0x7777, vec![]), (0x7777, vec![1])],
                ..d()
            },
            DuplicateExtension,
            47,
        ),
        (
            "EMS with data",
            Options {
                extended_master_secret: false,
                extra: vec![(0x0017, vec![0])],
                ..d()
            },
            Decode,
            50,
        ),
        (
            "odd use_srtp list",
            Options {
                srtp_profiles: None,
                extra: vec![(14, vec![0, 1, 7, 0])],
                ..d()
            },
            Decode,
            50,
        ),
        (
            "empty groups list",
            Options {
                groups: None,
                extra: vec![(10, vec![0, 0])],
                ..d()
            },
            Decode,
            50,
        ),
    ];
    for (what, o, err, desc) in cases {
        assert_eq!(negotiate(&o), Err(err), "{what}");
        assert_eq!(err.alert(), Some(desc), "{what}");
    }
}

#[test]
fn accepted_variants() {
    let d = Options::default;
    let ok = |o: Options| negotiate(&o).expect("accepted");
    assert_eq!(ok(d()).profile, SrtpProfile::AeadAes128Gcm);
    assert_eq!(
        ok(Options {
            srtp_profiles: Some(vec![1]),
            ..d()
        })
        .profile,
        SrtpProfile::Aes128CmHmacSha1_80
    );
    assert_eq!(
        ok(Options {
            srtp_profiles: Some(vec![7, 1]),
            ..d()
        })
        .profile,
        SrtpProfile::AeadAes128Gcm
    );
    ok(Options {
        renegotiation: Renegotiation::Scsv,
        ..d()
    });
    ok(Options {
        offer_dtls13: true,
        ..d()
    });
    ok(Options {
        client_version: 0xfefc,
        ..d()
    });
    // Unknown extensions, with any contents, are skipped.
    ok(Options {
        extra: vec![(0x1234, vec![1, 2, 3]), (0xfe0d, vec![])],
        ..d()
    });
    assert!(
        !ok(Options {
            point_formats: None,
            ..d()
        })
        .point_formats
    );
    assert!(ok(d()).point_formats);
}

#[test]
fn hello_structure_is_checked() {
    let good = Options::default().hello_body(&[1; 32], &[]);
    assert!(ClientHello::parse(&good).is_ok());
    let mut trailing = good.clone();
    trailing.push(0);
    assert_eq!(ClientHello::parse(&trailing), Err(Error::Decode));
    // A 33-byte session id.
    let mut b = good.clone();
    b[34] = 33;
    assert_eq!(ClientHello::parse(&b), Err(Error::Decode));
    // Without extensions at all it parses, and fails negotiation.
    let mut bare = Vec::new();
    put_u16(&mut bare, DTLS_1_2);
    bare.extend_from_slice(&[0; 32]);
    bare.extend_from_slice(&[0, 0, 0, 2, 0xc0, 0x2b, 1, 0]);
    let h = ClientHello::parse(&bare).expect("parses");
    assert_eq!(h.negotiate(), Err(Error::NoRenegotiationSignal));
}

#[test]
fn certificate_message_bounds() {
    let mut one = Vec::new();
    put_vec24(&mut one, b"leaf").expect("ok");
    put_vec24(&mut one, b"ca").expect("ok");
    let mut body = Vec::new();
    put_vec24(&mut body, &one).expect("ok");
    assert_eq!(certificate_leaf(&body, 4), Ok(&b"leaf"[..]));
    assert_eq!(
        certificate_leaf(&body, 1),
        Err(Error::Limit(crate::Limit::Chain))
    );
    assert_eq!(certificate_leaf(&[0, 0, 0], 4), Err(Error::NoCertificate));
    assert_eq!(
        certificate_leaf(&[0, 0, 3, 0, 0, 0], 4),
        Err(Error::Decode),
        "empty cert"
    );
    assert_eq!(
        certificate_leaf(&[0, 0, 4, 0, 0, 5, 1], 4),
        Err(Error::Decode)
    );
    let mut trailing = body.clone();
    trailing.push(0);
    assert_eq!(certificate_leaf(&trailing, 4), Err(Error::Decode));
}

#[test]
fn key_exchange_verify_and_finished_bounds() {
    let mut point = vec![65, 4];
    point.extend_from_slice(&[1; 64]);
    assert!(client_key_exchange(&point).is_ok());
    let mut compressed = vec![33, 2];
    compressed.extend_from_slice(&[1; 32]);
    assert_eq!(client_key_exchange(&compressed), Err(Error::BadKeyShare));
    assert_eq!(client_key_exchange(&[0]), Err(Error::Decode));
    assert_eq!(client_key_exchange(&[2, 4]), Err(Error::Decode));
    assert_eq!(certificate_verify(&[4, 3, 0, 1, 9]), Ok(&[9][..]));
    assert_eq!(
        certificate_verify(&[5, 3, 0, 1, 9]),
        Err(Error::VerifyScheme)
    );
    assert_eq!(
        Error::VerifyScheme.alert(),
        Some(crate::alert::ILLEGAL_PARAMETER)
    );
    assert_eq!(certificate_verify(&[4, 3, 0, 0]), Err(Error::Decode));
    assert_eq!(certificate_verify(&[4, 3, 0, 2, 9]), Err(Error::Decode));
    assert_eq!(finished(&[0; 12]), Ok([0; 12]));
    assert_eq!(finished(&[0; 13]), Err(Error::Decode));
}

#[test]
fn server_hello_answers_only_what_was_offered() {
    let n = Negotiated {
        profile: SrtpProfile::Aes128CmHmacSha1_80,
        point_formats: false,
    };
    let sh = server_hello(&[7; 32], &n).expect("build");
    let mut r = Reader::new(&sh);
    assert_eq!(r.u16(), Some(DTLS_1_2));
    assert_eq!(r.array::<32>(), Some([7; 32]));
    assert_eq!(r.vec8(), Some(&[][..]));
    assert_eq!(r.u16(), Some(SUITE));
    assert_eq!(r.u8(), Some(0));
    let mut e = Reader::new(r.vec16().expect("extensions"));
    let mut kinds = Vec::new();
    while !e.is_empty() {
        let k = e.u16().expect("type");
        let data = e.vec16().expect("data");
        if k == ext::USE_SRTP {
            assert_eq!(data, [0, 2, 0, 1, 0]);
        }
        kinds.push(k);
    }
    assert_eq!(
        kinds,
        [
            ext::EXTENDED_MASTER_SECRET,
            ext::RENEGOTIATION_INFO,
            ext::USE_SRTP
        ]
    );
    let with = server_hello(
        &[7; 32],
        &Negotiated {
            point_formats: true,
            ..n
        },
    )
    .expect("build");
    assert!(with.ends_with(&[0, 11, 0, 2, 1, 0]));
}

#[test]
fn fragments_stop_at_inconsistent_lengths() {
    // offset + fragment length > message length.
    let bad = [1, 0, 0, 4, 0, 0, 0, 0, 3, 0, 0, 2, 1, 2];
    assert_eq!(fragments(&bad).count(), 0);
    let ok = [1, 0, 0, 4, 0, 0, 0, 0, 2, 0, 0, 2, 1, 2];
    let f: Vec<_> = fragments(&ok).collect();
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].data, [1, 2]);
    assert_eq!(fragments(&ok[..13]).count(), 0, "truncated body");
}
