use super::{classify_grok_http_error, normalize_failure, size_limit_failure};
use crate::provider::http::wire::ResponseMode;
use crate::provider::{ProviderErrorKind, ProviderFailure};

#[test]
fn grok_http_failures_follow_the_provider_contract() {
    let cases = [
        (
            401,
            "",
            ProviderErrorKind::Login,
            "Grok rejected this session; run `kogen provider login grok`.",
        ),
        (
            403,
            "",
            ProviderErrorKind::Login,
            "This Grok account cannot access the requested model.",
        ),
        (
            429,
            "",
            ProviderErrorKind::UsageLimit,
            "Grok subscription usage limit reached.",
        ),
        (
            400,
            "quota exceeded",
            ProviderErrorKind::UsageLimit,
            "Grok subscription usage limit reached.",
        ),
        (
            400,
            "server_is_overloaded",
            ProviderErrorKind::Overload,
            "Grok service is temporarily overloaded.",
        ),
        (
            503,
            "",
            ProviderErrorKind::Overload,
            "Grok service is temporarily overloaded.",
        ),
        (
            400,
            "bad request",
            ProviderErrorKind::Malformed,
            "Grok rejected the request (HTTP 400).",
        ),
    ];
    for (status, body, kind, message) in cases {
        let failure = classify_grok_http_error(status, body);
        assert_eq!(failure.kind, kind);
        assert_eq!(failure.message, message);
    }
    assert_eq!(
        size_limit_failure().message,
        "Grok response exceeded the size limit."
    );
    let malformed = normalize_failure(
        ProviderFailure::new(ProviderErrorKind::Malformed, "anything"),
        ResponseMode::Grok,
    );
    assert_eq!(
        malformed.message,
        "Grok returned a malformed response stream."
    );
}
