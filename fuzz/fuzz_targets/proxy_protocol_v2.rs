//! PROXY protocol v2 headers through `sapflow_core::proxy_protocol::parse`,
//! whole and at an arbitrary split. The first input byte chooses whether the
//! v2 signature is prepended (so mutations reach past it) and where to split;
//! the rest is the connection's bytes.
#![no_main]
use libfuzzer_sys::fuzz_target;
use sapflow_core::proxy_protocol::{FIXED_LEN, MAX_HEADER_LEN, Parsed, SIGNATURE, parse};

fuzz_target!(|data: &[u8]| {
    let Some((&control, rest)) = data.split_first() else {
        return;
    };
    let mut owned = Vec::new();
    let input = if control & 1 == 1 {
        owned.extend_from_slice(&SIGNATURE);
        owned.extend_from_slice(rest);
        owned.as_slice()
    } else {
        rest
    };
    let whole = parse(input);
    if let Ok(Parsed::Complete { header, consumed }) = whole {
        // The consumed length is the declared one, read independently here.
        let declared = input
            .get(14..16)
            .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
            .expect("a header has its fixed part");
        assert_eq!(consumed, FIXED_LEN + declared);
        assert!(consumed <= MAX_HEADER_LEN && consumed <= input.len());
        // The header alone parses to the same result.
        let alone = input.get(..consumed).expect("consumed bytes");
        assert_eq!(parse(alone), Ok(Parsed::Complete { header, consumed }));
        // Walking the TLVs terminates within the header.
        assert!(header.tlvs().count() <= consumed);
    }
    // A prefix gives `NeedMore` or exactly the whole result.
    let split = usize::from(control >> 1) * input.len() / 127;
    let prefix = input.get(..split.min(input.len())).unwrap_or_default();
    match parse(prefix) {
        Ok(Parsed::NeedMore { additional }) => {
            assert!(additional >= 1);
            if let Ok(Parsed::Complete { consumed, .. }) = whole {
                assert!(prefix.len() + additional <= consumed);
            }
        }
        partial => assert_eq!(partial, whole),
    }
});
