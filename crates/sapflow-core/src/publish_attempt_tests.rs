use super::*;

const ATTEMPT: &str = "0123456789abcdef0123456789abcdef";
fn stream(attribute: &str) -> String {
    format!("#!::m=publish,r=live/event-attempt,u=fixture,s=credential,{attribute}")
}

#[test]
fn typed_attempt_id_is_exactly_bounded_lowercase_hex() {
    assert_eq!(
        ConnectionAttemptId::parse(ATTEMPT)
            .expect("valid ID")
            .as_str(),
        ATTEMPT
    );
    for invalid in [
        "",
        "0123456789abcdef",
        "0123456789ABCDEF0123456789abcdef",
        "0123456789abcdef0123456789abcdeg",
        "0123456789abcdef0123456789abcdef0",
        "0123456789abcdef0123456789abcde\n",
    ] {
        assert!(ConnectionAttemptId::parse(invalid).is_none(), "{invalid:?}");
    }
}

#[test]
fn access_control_attempt_is_correlation_only_and_old_publishers_stay_unbound() {
    let request =
        PublishRequest::parse_srt_stream_id(&stream(&format!("forgesworn_attempt={ATTEMPT}")))
            .expect("attempt");
    assert_eq!(
        request.connection_attempt_id.as_ref().expect("ID").as_str(),
        ATTEMPT
    );
    assert_eq!(request.path.as_path(), "live/event-attempt");
    assert_eq!(
        request.authorise(&Publisher::new("fixture", "credential")),
        Ok(())
    );
    assert_eq!(
        request.authorise(&Publisher::new("fixture", "different")),
        Err(PublishError::Unauthorized)
    );
    for legacy in [
        "publish:live/event-attempt",
        "publish:live/event-attempt:fixture:credential",
        "#!::m=publish,r=live/event-attempt,u=fixture,s=credential,other_extension=ignored",
    ] {
        assert!(
            PublishRequest::parse_srt_stream_id(legacy)
                .expect("legacy")
                .connection_attempt_id
                .is_none()
        );
    }
    assert!(
        PublishRequest::parse_rtmp(
            "live",
            &format!("event-attempt?user=fixture&pass=credential&forgesworn_attempt={ATTEMPT}")
        )
        .expect("RTMP compatibility")
        .connection_attempt_id
        .is_none()
    );
}

#[test]
fn duplicate_or_malformed_reserved_attempt_attribute_fails_without_last_value_wins() {
    for attribute in [
        format!("forgesworn_attempt={ATTEMPT},forgesworn_attempt={ATTEMPT}"),
        format!("forgesworn_attempt=bad,forgesworn_attempt={ATTEMPT}"),
        format!("forgesworn_attempt={ATTEMPT},forgesworn_attempt=bad"),
        "forgesworn_attempt=".into(),
        "forgesworn_attempt".into(),
        "forgesworn_attempt=0123456789ABCDEF0123456789abcdef".into(),
        "forgesworn_attempt=0123456789abcdef0123456789abcdef0".into(),
        "forgesworn_attempt=%300123456789abcdef0123456789abcde".into(),
        format!("FORGESWORN_ATTEMPT={ATTEMPT}"),
        format!(" forgesworn_attempt={ATTEMPT}"),
    ] {
        assert_eq!(
            PublishRequest::parse_srt_stream_id(&stream(&attribute)),
            Err(PublishError::BadRequest),
            "{attribute}"
        );
    }
}

#[test]
fn whole_stream_id_remains_byte_bounded_with_attempt_metadata() {
    let mut exact = stream(&format!("forgesworn_attempt={ATTEMPT},forgesworn_padding="));
    exact.push_str(&"a".repeat(512 - exact.len()));
    assert_eq!(exact.len(), 512);
    assert!(
        PublishRequest::parse_srt_stream_id(&exact)
            .expect("exact bound")
            .connection_attempt_id
            .is_some()
    );
    exact.push('a');
    assert_eq!(
        PublishRequest::parse_srt_stream_id(&exact),
        Err(PublishError::BadRequest)
    );
}
