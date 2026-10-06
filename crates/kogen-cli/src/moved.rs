const PREFIX_MOVES: &[(&[&str], &str)] = &[
    (&["build", "show"], "kogen status <slug>"),
    (
        &["build"],
        "kogen queue start (approved Intents build from the queue)",
    ),
    (&["report"], "kogen status <slug>"),
    (&["approve"], "kogen intent approve <slug> <hash>"),
    (
        &["reconcile"],
        "kogen status (crash recovery is automatic in status and queue start)",
    ),
    (
        &["intent", "check"],
        "kogen intent approve <slug> (prints the review card and check results)",
    ),
    (&["intent", "close"], "kogen intent remove <slug>"),
    (&["--version"], "kogen version"),
];

const FLAG_MOVES: &[(&str, &str, bool)] = &[
    ("--task-file", "kogen intent shape <slug> <file>", false),
    ("--yes", "kogen intent approve <slug> <hash>", false),
    (
        "--borrow",
        "kogen provider login chatgpt for a Kogen-owned login",
        false,
    ),
    ("--recipe", "build.recipe in .kogen/project.yaml", false),
    (
        "--model",
        "build.roles.builder.model in .kogen/project.yaml",
        false,
    ),
    (
        "--effort",
        "build.roles.builder.effort in .kogen/project.yaml",
        false,
    ),
    (
        "--as",
        "kogen provider use chatgpt --as <label> --project <checkout>",
        true,
    ),
];

/// Apply the moved-form table before tree and option parsing.
#[must_use]
pub fn find_moved_form(args: &[String]) -> Option<&'static str> {
    for (prefix, message) in PREFIX_MOVES {
        if args.len() >= prefix.len()
            && args
                .iter()
                .take(prefix.len())
                .map(String::as_str)
                .eq(prefix.iter().copied())
        {
            return Some(message);
        }
    }

    let is_provider_command = args.first().is_some_and(|word| word == "provider");
    for (flag, message, outside_provider_only) in FLAG_MOVES {
        if *outside_provider_only && is_provider_command {
            continue;
        }
        if args.iter().any(|arg| {
            arg == flag
                || arg
                    .strip_prefix(flag)
                    .is_some_and(|suffix| suffix.starts_with('='))
        }) {
            return Some(message);
        }
    }
    None
}
