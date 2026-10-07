use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Returns a Git command with global and system configuration isolated.
#[must_use]
pub fn git_command() -> Command {
    let mut command = Command::new("git");
    configure_git_command(&mut command);
    command
}

/// Isolates a Git subprocess from the developer's global and system config.
pub fn configure_git_command(command: &mut Command) {
    command
        .env("GIT_CONFIG_GLOBAL", empty_global_config())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS");
}

/// Writes an explicit identity to a test repository's local Git config.
pub fn set_identity(repository: &Path, name: &str, email: &str) -> Result<(), String> {
    for (key, value) in [("user.name", name), ("user.email", email)] {
        let output = git_command()
            .arg("-C")
            .arg(repository)
            .args(["config", "--local", key, value])
            .output()
            .map_err(|error| format!("start git config {key}: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "git config {key} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }
    Ok(())
}

fn empty_global_config() -> &'static Path {
    static CONFIG: OnceLock<PathBuf> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is after the Unix epoch")
                .as_nanos();
            let directory = std::env::temp_dir().join(format!(
                "kogen-git-test-config-{}-{stamp}",
                std::process::id()
            ));
            std::fs::create_dir(&directory).expect("create isolated Git config directory");
            let config = directory.join("config");
            std::fs::write(&config, "").expect("write empty Git global config");
            config
        })
        .as_path()
}
