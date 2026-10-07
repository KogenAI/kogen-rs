use kogen_cache_replay::execute::{ReceiptLedger, execute_plan, unadmitted_ledger};
use kogen_cache_replay::{
    CHATGPT_BACKEND_ENDPOINT, FixtureManifest, MAX_POSTS, MAX_TOTAL_TOKENS, ReplayPlan,
    adapter_source_sha256, build_plan, dry_run_rows, plan_digest, sha256_hex, verify_plan,
};
use kogen_core::provider::RunAccount;
use kogen_core::provider::auth::{self, RequestCredential};
use kogen_core::provider::http::ReqwestPort;
use serde_json::json;
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    if let Err(error) = run() {
        eprintln!("kogen-cache-replay: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let Some(command) = args.next() else {
        return Err(usage());
    };
    if command == "--help" || command == "help" {
        println!("{}", usage());
        return Ok(());
    }
    let options = parse_options(args.collect())?;
    reject_ambient_overrides(&options)?;
    match command.as_str() {
        "plan" => run_plan(&options),
        "dry-run" => run_dry_run(&options),
        "execute" => run_execute(&options),
        _ => Err(format!("unsupported mode {command:?}\n{}", usage())),
    }
}

fn run_plan(options: &BTreeMap<String, String>) -> Result<(), String> {
    reject_unknown_options(options, &["fixtures", "seed", "out"])?;
    let fixture_path = required(options, "fixtures")?;
    let seed = required(options, "seed")?;
    let out = PathBuf::from(required(options, "out")?);
    let fixture_bytes = fs::read(fixture_path)
        .map_err(|_| format!("could not read sanitized fixture manifest {}", fixture_path))?;
    let manifest: FixtureManifest = serde_json::from_slice(&fixture_bytes)
        .map_err(|error| format!("fixture manifest is invalid JSON: {error}"))?;
    let mut plan = build_plan(&manifest, &fixture_bytes, seed)?;
    plan.source_revision = source_revision();
    plan.binary_sha256 = current_binary_sha256()?;
    plan.adapter_source_sha256 = adapter_source_sha256();
    plan.plan_sha256 = plan_digest(&plan)?;
    let bytes = serde_json::to_vec_pretty(&plan).map_err(|error| error.to_string())?;
    write_external_file(&out, &bytes)?;
    println!(
        "{}",
        json!({
            "plan_sha256": plan.plan_sha256,
            "request_count": plan.scheduled_posts,
            "scheduled_posts": plan.scheduled_posts,
            "scheduled_input_reservation": plan.scheduled_input_reservation,
            "scheduled_output_reservation": plan.scheduled_output_reservation,
            "scheduled_total_reservation": plan.scheduled_total_reservation,
            "worst_case_token_estimate": plan.scheduled_total_reservation,
            "admitted": plan.admission.admitted,
            "blockers": plan.admission.blockers,
            "admission_policy": plan.admission.policy,
            "plan_path": out
        })
    );
    Ok(())
}

fn run_dry_run(options: &BTreeMap<String, String>) -> Result<(), String> {
    reject_unknown_options(options, &["plan"])?;
    let plan_path = required(options, "plan")?;
    let plan = read_plan(plan_path)?;
    let output = json!({
        "plan_sha256": plan.plan_sha256,
        "source_revision": plan.source_revision,
        "binary_sha256": plan.binary_sha256,
        "adapter_source_sha256": plan.adapter_source_sha256,
        "admitted": plan.admission.admitted,
        "blockers": plan.admission.blockers,
        "admission_policy": plan.admission.policy,
        "request_count": plan.scheduled_posts,
        "scheduled_posts": plan.scheduled_posts,
        "scheduled_input_reservation": plan.scheduled_input_reservation,
        "scheduled_output_reservation": plan.scheduled_output_reservation,
        "scheduled_total_reservation": plan.scheduled_total_reservation,
        "worst_case_token_estimate": plan.scheduled_total_reservation,
        "requests": dry_run_rows(&plan)
    });
    serde_json::to_writer_pretty(std::io::stdout().lock(), &output)
        .map_err(|error| format!("could not print dry-run: {error}"))?;
    println!();
    Ok(())
}

fn run_execute(options: &BTreeMap<String, String>) -> Result<(), String> {
    reject_unknown_options(
        options,
        &["plan", "max-posts", "max-total-tokens", "auth-path", "out"],
    )?;
    let plan_path = required(options, "plan")?;
    let max_posts = parse_cap(required(options, "max-posts")?, "--max-posts")?;
    let max_total_tokens = parse_cap(required(options, "max-total-tokens")?, "--max-total-tokens")?;
    let out = PathBuf::from(required(options, "out")?);
    let plan = read_plan(plan_path)?;
    if max_posts > MAX_POSTS || max_posts > plan.maximum_posts {
        return Err(format!("--max-posts must be at most {MAX_POSTS}"));
    }
    if max_total_tokens > MAX_TOTAL_TOKENS || max_total_tokens > plan.maximum_total_tokens {
        return Err(format!(
            "--max-total-tokens must be at most {MAX_TOTAL_TOKENS}"
        ));
    }
    if plan.binary_sha256 != current_binary_sha256()? {
        return Err("running binary SHA-256 differs from the frozen plan".to_owned());
    }
    prepare_external_directory(&out)?;
    let attempts_path = out.join("attempts.jsonl");
    let run_path = out.join("run.json");
    let attempts_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&attempts_path)
        .map_err(|error| format!("could not create {}: {error}", attempts_path.display()))?;
    let mut attempt_writer = BufWriter::new(attempts_file);
    let explicit_auth_path = options.get("auth-path").map(PathBuf::from);
    let auth_source = if explicit_auth_path.is_some() {
        "explicit_injected_path"
    } else {
        "kogen_owned_account_selection"
    };
    if !plan.admission.admitted {
        let reason = format!(
            "plan is not admitted; no login was loaded and no request was sent: {}",
            plan.admission.blockers.join("; ")
        );
        let ledger = unadmitted_ledger(&plan, max_posts, max_total_tokens, auth_source, &reason)?;
        for receipt in &ledger.attempts {
            serde_json::to_writer(&mut attempt_writer, receipt)
                .map_err(|error| format!("could not journal attempt: {error}"))?;
            attempt_writer
                .write_all(b"\n")
                .and_then(|()| attempt_writer.flush())
                .map_err(|error| format!("could not flush attempt journal: {error}"))?;
        }
        write_ledger(&run_path, &ledger)?;
        println!(
            "{}",
            json!({
                "plan_sha256": ledger.plan_sha256,
                "admitted": false,
                "admission_policy": ledger.admission.policy,
                "posts_sent": ledger.posts_sent,
                "tokens_charged_or_reserved": ledger.tokens_charged_or_reserved,
                "aborted_reason": ledger.aborted_reason,
                "attempts_path": attempts_path,
                "run_path": run_path
            })
        );
        return Err(reason);
    }
    let http = ReqwestPort::new_without_redirects().map_err(|failure| failure.message)?;
    let home = kogen_home()?;
    let account = selected_chatgpt_account(&home, explicit_auth_path.is_some())?;
    let mut load_credential =
        || load_kogen_credential(&home, &account, explicit_auth_path.as_deref());
    let ledger = execute_plan(
        &plan,
        max_posts,
        max_total_tokens,
        auth_source,
        &http,
        &mut load_credential,
        |receipt| {
            serde_json::to_writer(&mut attempt_writer, receipt)
                .map_err(|error| format!("could not journal attempt: {error}"))?;
            attempt_writer
                .write_all(b"\n")
                .and_then(|()| attempt_writer.flush())
                .map_err(|error| format!("could not flush attempt journal: {error}"))
        },
    )?;
    write_ledger(&run_path, &ledger)?;
    println!(
        "{}",
        json!({
            "plan_sha256": ledger.plan_sha256,
            "admitted": ledger.admission.admitted,
            "admission_policy": ledger.admission.policy,
            "posts_sent": ledger.posts_sent,
            "tokens_charged_or_reserved": ledger.tokens_charged_or_reserved,
            "aborted_reason": ledger.aborted_reason,
            "attempts_path": attempts_path,
            "run_path": run_path
        })
    );
    match ledger.aborted_reason {
        Some(reason) => Err(format!("execution stopped: {reason}")),
        None => Ok(()),
    }
}

fn kogen_home() -> Result<PathBuf, String> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set; Kogen cannot select a saved ChatGPT login".to_owned())
}

fn selected_chatgpt_account(home: &Path, injected: bool) -> Result<RunAccount, String> {
    let project = env::current_dir().map_err(|error| error.to_string())?;
    let requested_account = env::var("KOGEN_BENCH_ACCOUNT").ok();
    let account = kogen_core::provider::resolve_run_account(
        home,
        &project,
        None,
        Some("chatgpt"),
        requested_account.as_deref(),
        injected,
    )
    .map_err(|error| error.reason)?;
    Ok(account)
}

fn load_kogen_credential(
    home: &Path,
    account: &RunAccount,
    auth_path: Option<&Path>,
) -> Result<RequestCredential, String> {
    let credential = auth::credential_for_request_with_injected_path(home, account, auth_path)
        .map_err(|error| error.reason)?;
    match (auth_path.is_some(), credential) {
        (true, credential @ RequestCredential::Injected(_)) => Ok(credential),
        (false, credential @ RequestCredential::Owned(_)) => Ok(credential),
        (true, _) => Err(
            "explicit auth path did not resolve to Kogen injected ChatGPT credentials".to_owned(),
        ),
        (false, _) => {
            Err("selected account did not resolve to Kogen Owned ChatGPT credentials".to_owned())
        }
    }
}

fn read_plan(path: &str) -> Result<ReplayPlan, String> {
    let bytes = fs::read(path).map_err(|_| format!("could not read replay plan {path}"))?;
    let plan: ReplayPlan = serde_json::from_slice(&bytes)
        .map_err(|error| format!("replay plan is invalid JSON: {error}"))?;
    verify_plan(&plan)?;
    Ok(plan)
}

fn write_ledger(path: &Path, ledger: &ReceiptLedger) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(ledger).map_err(|error| error.to_string())?;
    write_new_file(path, &bytes)
}

fn current_binary_sha256() -> Result<String, String> {
    let path = env::current_exe().map_err(|error| error.to_string())?;
    let bytes = fs::read(path).map_err(|error| format!("could not hash replay binary: {error}"))?;
    Ok(sha256_hex(&bytes))
}

fn source_revision() -> String {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|revision| revision.trim().to_owned())
        .unwrap_or_else(|| "unresolved".to_owned())
}

fn write_external_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    reject_repository_path(path)?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create output directory: {error}"))?;
    write_new_file(path, bytes)
}

fn prepare_external_directory(path: &Path) -> Result<(), String> {
    reject_repository_path(path)?;
    fs::create_dir_all(path)
        .map_err(|error| format!("could not create receipt directory: {error}"))?;
    Ok(())
}

fn reject_repository_path(path: &Path) -> Result<(), String> {
    let root = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|root| PathBuf::from(root.trim()));
    let Some(root) = root.and_then(|root| fs::canonicalize(root).ok()) else {
        return Ok(());
    };
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map_err(|error| error.to_string())?
            .join(path)
    };
    let canonical = if candidate.exists() {
        fs::canonicalize(&candidate).map_err(|error| error.to_string())?
    } else {
        let mut ancestor = candidate.as_path();
        while !ancestor.exists() {
            ancestor = ancestor
                .parent()
                .ok_or_else(|| "output path has no existing parent".to_owned())?;
        }
        let canonical_ancestor = fs::canonicalize(ancestor).map_err(|error| error.to_string())?;
        let remainder = candidate
            .strip_prefix(ancestor)
            .map_err(|error| error.to_string())?;
        canonical_ancestor.join(remainder)
    };
    if canonical.starts_with(&root) {
        return Err("plan and receipt outputs must be outside the repository".to_owned());
    }
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("could not create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("could not sync {}: {error}", path.display()))
}

fn parse_options(args: Vec<String>) -> Result<BTreeMap<String, String>, String> {
    let mut options = BTreeMap::new();
    let mut index = 0;
    while index < args.len() {
        let key = &args[index];
        if !key.starts_with("--") {
            return Err(format!("unexpected argument {key:?}"));
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("missing value for {key}"))?;
        if value.starts_with("--") {
            return Err(format!("missing value for {key}"));
        }
        if options
            .insert(key[2..].to_owned(), value.to_owned())
            .is_some()
        {
            return Err(format!("duplicate option {key}"));
        }
        index += 2;
    }
    Ok(options)
}

fn required<'a>(options: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, String> {
    options
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("missing --{key}"))
}

fn reject_unknown_options(
    options: &BTreeMap<String, String>,
    allowed: &[&str],
) -> Result<(), String> {
    if let Some(unknown) = options.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(format!("unknown option --{unknown}"));
    }
    Ok(())
}

fn parse_cap(value: &str, name: &str) -> Result<u64, String> {
    value
        .parse::<u64>()
        .map_err(|_| format!("{name} must be an unsigned integer"))
}

fn reject_ambient_overrides(options: &BTreeMap<String, String>) -> Result<(), String> {
    if env::var_os("KOGEN_PROVIDER_URL").is_some() {
        return Err(
            "KOGEN_PROVIDER_URL is not allowed; replay endpoints are fixed and allowlisted"
                .to_owned(),
        );
    }
    if env::var_os("KOGEN_AUTH_PATH").is_some() && !options.contains_key("auth-path") {
        return Err(
            "KOGEN_AUTH_PATH is not allowed; pass --auth-path explicitly to execute for injected auth"
                .to_owned(),
        );
    }
    Ok(())
}

fn usage() -> String {
    format!(
        "Usage:\n  kogen-cache-replay plan --fixtures <sanitized-fixtures.json> --seed <seed> --out <plan.json>\n  kogen-cache-replay dry-run --plan <plan.json>\n  kogen-cache-replay execute --plan <plan.json> --max-posts <n> --max-total-tokens <n> --out <receipt-dir> [--auth-path <path>]\n\nThe replay allocation is capped at {MAX_POSTS} POSTs and {MAX_TOTAL_TOKENS} tokens. {CHATGPT_BACKEND_ENDPOINT} currently has no verified output-cap contract, so its plans are not admitted for execution."
    )
}
