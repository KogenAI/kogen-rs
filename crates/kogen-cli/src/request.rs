use std::path::PathBuf;

use crate::help::HelpPage;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageError {
    pub message: String,
    pub page: HelpPage,
}

impl UsageError {
    #[must_use]
    pub fn render(&self) -> String {
        format!("{}\n\n{}", self.message, self.page.contents())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectOptions {
    pub project: PathBuf,
    pub origin: Option<PathBuf>,
    pub base: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Status {
        slug: Option<String>,
        watch: bool,
        json: bool,
        project: ProjectOptions,
    },
    IntentShape {
        slug: String,
        request: String,
        project: ProjectOptions,
    },
    IntentApprove {
        slug: String,
        hash: Option<String>,
        by: Option<String>,
        project: ProjectOptions,
    },
    IntentRemove {
        slug: String,
        force: bool,
        project: ProjectOptions,
    },
    QueueStart {
        detach: bool,
        project: ProjectOptions,
    },
    QueueStop {
        project: ProjectOptions,
    },
    ProviderList,
    ProviderLogin {
        provider: String,
    },
    ProviderLogout {
        provider: String,
    },
    ProviderUse {
        provider: String,
        label: String,
        project: Option<PathBuf>,
    },
    Version,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParsedRequest {
    Help(HelpPage),
    Moved(&'static str),
    Usage(UsageError),
    Command(Command),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Route {
    Status,
    IntentShape,
    IntentApprove,
    IntentRemove,
    QueueStart,
    QueueStop,
    ProviderList,
    ProviderLogin,
    ProviderLogout,
    ProviderUse,
    Version,
}

impl Route {
    pub const fn path(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::IntentShape => "intent shape",
            Self::IntentApprove => "intent approve",
            Self::IntentRemove => "intent remove",
            Self::QueueStart => "queue start",
            Self::QueueStop => "queue stop",
            Self::ProviderList => "provider list",
            Self::ProviderLogin => "provider login",
            Self::ProviderLogout => "provider logout",
            Self::ProviderUse => "provider use",
            Self::Version => "version",
        }
    }
}

#[derive(Default)]
pub struct Options {
    pub project: Option<String>,
    pub origin: Option<String>,
    pub base: Option<String>,
    pub by: Option<String>,
    pub as_label: Option<String>,
    pub watch: bool,
    pub json: bool,
    pub force: bool,
    pub detach: bool,
    pub unknown_option: Option<String>,
    pub disallowed_option: Option<String>,
    pub missing_value: Option<String>,
    pub boolean_value: Option<String>,
}
