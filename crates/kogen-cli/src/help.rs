#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HelpPage {
    Top,
    Status,
    Intent,
    IntentShape,
    IntentApprove,
    IntentRemove,
    Queue,
    QueueStart,
    QueueStop,
    Provider,
    ProviderList,
    ProviderLogin,
    ProviderLogout,
    ProviderUse,
    Version,
}

impl HelpPage {
    #[must_use]
    pub const fn contents(self) -> &'static str {
        match self {
            Self::Top => include_str!("../data/help/kogen.txt"),
            Self::Status => include_str!("../data/help/kogen-status.txt"),
            Self::Intent => include_str!("../data/help/kogen-intent.txt"),
            Self::IntentShape => include_str!("../data/help/kogen-intent-shape.txt"),
            Self::IntentApprove => include_str!("../data/help/kogen-intent-approve.txt"),
            Self::IntentRemove => include_str!("../data/help/kogen-intent-remove.txt"),
            Self::Queue => include_str!("../data/help/kogen-queue.txt"),
            Self::QueueStart => include_str!("../data/help/kogen-queue-start.txt"),
            Self::QueueStop => include_str!("../data/help/kogen-queue-stop.txt"),
            Self::Provider => include_str!("../data/help/kogen-provider.txt"),
            Self::ProviderList => include_str!("../data/help/kogen-provider-list.txt"),
            Self::ProviderLogin => include_str!("../data/help/kogen-provider-login.txt"),
            Self::ProviderLogout => include_str!("../data/help/kogen-provider-logout.txt"),
            Self::ProviderUse => include_str!("../data/help/kogen-provider-use.txt"),
            Self::Version => include_str!("../data/help/kogen-version.txt"),
        }
    }
}
