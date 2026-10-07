use crate::error::{CoreError, ErrorClass};
use crate::provider::ProviderErrorKind;

pub(super) fn provider_error(kind: ProviderErrorKind, detail: impl Into<String>) -> CoreError {
    CoreError::new(
        ErrorClass::Provider,
        kind.as_str(),
        detail,
        crate::ExitCode::Provider,
    )
}
