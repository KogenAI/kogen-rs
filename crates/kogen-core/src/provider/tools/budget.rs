use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;

use super::ToolError;

const NOTICE_RESERVE: usize = 320;

pub(super) fn bound_result(run_dir: &Path, result: &str, tokens: u64) -> Result<String, ToolError> {
    if !(128..=100_000).contains(&tokens) {
        return Err(ToolError::InvalidArguments);
    }
    let redacted = redact(result);
    let cap = usize::try_from(tokens.saturating_mul(4)).unwrap_or(usize::MAX);
    if redacted.len() <= cap {
        return Ok(redacted);
    }
    let handle = store_full_result(run_dir, redacted.as_bytes())?;
    let mut prefix_len = cap.saturating_sub(NOTICE_RESERVE) / 2;
    let mut suffix_len = prefix_len;
    let total = redacted.len();
    let mut output = String::new();
    for _ in 0..8 {
        prefix_len = floor_boundary(&redacted, prefix_len);
        suffix_len = floor_boundary_from_end(&redacted, suffix_len);
        let shown = format!(
            "0..{prefix_len}, {}..{total}",
            total.saturating_sub(suffix_len)
        );
        let notice = truncation_notice(total, &shown, &handle);
        let available = cap.saturating_sub(notice.len());
        let next_prefix = floor_boundary(&redacted, available / 2);
        let next_suffix = floor_boundary_from_end(&redacted, available - available / 2);
        output = format!(
            "{}{}{}",
            &redacted[..prefix_len],
            notice,
            &redacted[total - suffix_len..]
        );
        if output.len() <= cap && prefix_len == next_prefix && suffix_len == next_suffix {
            return Ok(output);
        }
        prefix_len = next_prefix;
        suffix_len = next_suffix;
    }
    Ok(output)
}

pub(super) fn read_output(
    run_dir: &Path,
    handle: &str,
    offset: u64,
    limit: Option<u64>,
) -> Result<String, ToolError> {
    if handle.len() != 64
        || !handle
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ToolError::UnknownOutputHandle);
    }
    let path = logs_dir(run_dir)?.join(format!("tool-result-{handle}.log"));
    let metadata = fs::symlink_metadata(&path).map_err(|_| ToolError::UnknownOutputHandle)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(ToolError::UnknownOutputHandle);
    }
    let bytes = fs::read(path).map_err(|_| ToolError::UnknownOutputHandle)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| ToolError::UnknownOutputHandle)?;
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    let end = limit
        .and_then(|limit| usize::try_from(offset.saturating_add(limit)).ok())
        .unwrap_or(bytes.len())
        .min(bytes.len());
    let start = ceil_boundary(text, start.min(end));
    let end = floor_boundary(text, end.max(start));
    Ok(text[start..end].to_owned())
}

fn store_full_result(run_dir: &Path, bytes: &[u8]) -> Result<String, ToolError> {
    let handle = format!("{:x}", Sha256::digest(bytes));
    let path = logs_dir(run_dir)?.join(format!("tool-result-{handle}.log"));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            file.write_all(bytes)
                .map_err(|error| ToolError::Io(error.to_string()))?;
            file.sync_all()
                .map_err(|error| ToolError::Io(error.to_string()))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(ToolError::Io(error.to_string())),
    }
    Ok(handle)
}

fn logs_dir(run_dir: &Path) -> Result<std::path::PathBuf, ToolError> {
    let path = run_dir.join("logs");
    fs::create_dir_all(&path).map_err(|error| ToolError::Io(error.to_string()))?;
    let metadata = fs::symlink_metadata(&path).map_err(|error| ToolError::Io(error.to_string()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ToolError::UnknownOutputHandle);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .map_err(|error| ToolError::Io(error.to_string()))?;
    }
    Ok(path)
}

fn truncation_notice(total: usize, shown: &str, handle: &str) -> String {
    format!(
        "\n[truncated/range: {total} bytes; shown byte ranges {shown}; retrieve with tool_output handle={handle}, output_offset and output_limit]\n"
    )
}

fn floor_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset = offset.saturating_sub(1);
    }
    offset
}

fn ceil_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

fn floor_boundary_from_end(text: &str, length: usize) -> usize {
    let mut start = text.len().saturating_sub(length.min(text.len()));
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text.len() - start
}

fn redact(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut offset = 0;
    while offset < text.len() {
        let word_end = next_whitespace(text, offset).unwrap_or(text.len());
        let word = &text[offset..word_end];
        let space_end = next_non_whitespace(text, word_end).unwrap_or(text.len());
        let spacing = &text[word_end..space_end];
        if word.eq_ignore_ascii_case("bearer") {
            output.push_str(word);
            output.push_str(spacing);
            offset = space_end;
            if offset < text.len() {
                let secret_end = next_whitespace(text, offset).unwrap_or(text.len());
                let secret_space_end = next_non_whitespace(text, secret_end).unwrap_or(text.len());
                output.push_str("[REDACTED]");
                output.push_str(&text[secret_end..secret_space_end]);
                offset = secret_space_end;
            }
        } else if looks_like_jwt(word) || word.starts_with("sk-") {
            output.push_str("[REDACTED]");
            output.push_str(spacing);
            offset = space_end;
        } else {
            output.push_str(word);
            output.push_str(spacing);
            offset = space_end;
        }
    }
    output
}

fn next_whitespace(text: &str, offset: usize) -> Option<usize> {
    text[offset..]
        .char_indices()
        .find(|(_, character)| character.is_whitespace())
        .map(|(index, _)| offset + index)
}

fn next_non_whitespace(text: &str, offset: usize) -> Option<usize> {
    text[offset..]
        .char_indices()
        .find(|(_, character)| !character.is_whitespace())
        .map(|(index, _)| offset + index)
}

fn looks_like_jwt(token: &str) -> bool {
    let mut parts = token.split('.');
    parts.next().is_some_and(|part| !part.is_empty())
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::bound_result;

    #[test]
    fn large_results_keep_utf8_boundaries_and_store_a_retrievable_copy() {
        let root = std::env::temp_dir().join(format!("kogen-tool-budget-{}", std::process::id()));
        let run = root.join("run");
        std::fs::create_dir_all(&run).unwrap();
        let source = "é".repeat(2000);
        let shown = bound_result(&run, &source, 128).unwrap();
        assert!(shown.len() <= 512);
        assert!(shown.starts_with("é"));
        assert!(shown.contains("truncated/range"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
