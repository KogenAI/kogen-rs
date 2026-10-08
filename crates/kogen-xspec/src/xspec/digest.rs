/// Translate a symbolic claim into a real claim that has the same modeled
/// match result, then let the caller check that result against the real hash.
pub(super) fn modeled_claim(actual_digest: &str, symbolic_claim: &str, matches: bool) -> String {
    if matches {
        return actual_digest
            .get(..symbolic_claim.len())
            .unwrap_or(actual_digest)
            .to_owned();
    }

    if symbolic_claim.is_empty() {
        return "!".to_owned();
    }

    let mut nonmatching = actual_digest.as_bytes().to_vec();
    if let Some(first) = nonmatching.first_mut() {
        *first = if *first == b'0' { b'1' } else { b'0' };
    }
    let nonmatching = String::from_utf8(nonmatching).expect("SHA-256 hex is ASCII");
    nonmatching
        .get(..symbolic_claim.len())
        .unwrap_or(&nonmatching)
        .to_owned()
}
