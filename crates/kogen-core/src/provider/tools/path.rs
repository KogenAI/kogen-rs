use std::fs;
use std::path::{Path, PathBuf};

use super::ToolError;

pub(super) fn resolve(
    root: &Path,
    requested: &str,
    must_exist: bool,
) -> Result<PathBuf, ToolError> {
    let candidate = if Path::new(requested).is_absolute() {
        PathBuf::from(requested)
    } else {
        root.join(requested)
    };
    if must_exist {
        let resolved = fs::canonicalize(candidate).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                ToolError::MissingFile
            } else {
                ToolError::Io(error.to_string())
            }
        })?;
        return if resolved.starts_with(root) {
            Ok(resolved)
        } else {
            Err(ToolError::PathEscape)
        };
    }
    match fs::symlink_metadata(&candidate) {
        Ok(_) => {
            let resolved = fs::canonicalize(candidate).map_err(|_| ToolError::PathEscape)?;
            return if resolved.starts_with(root) {
                Ok(resolved)
            } else {
                Err(ToolError::PathEscape)
            };
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(ToolError::Io(error.to_string())),
    }
    let parent = candidate.parent().ok_or(ToolError::PathEscape)?;
    let name = candidate.file_name().ok_or(ToolError::PathEscape)?;
    let parent = fs::canonicalize(parent).map_err(|error| ToolError::Io(error.to_string()))?;
    if !parent.starts_with(root) {
        return Err(ToolError::PathEscape);
    }
    Ok(parent.join(name))
}

pub(super) fn display_relative(root: &Path, path: &Path) -> String {
    let relative = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");
    if relative.is_empty() {
        ".".to_owned()
    } else {
        relative
    }
}

#[cfg(test)]
mod tests {
    use super::display_relative;
    use std::path::Path;

    #[test]
    fn workspace_root_is_searchable_as_dot() {
        let root = Path::new("/workspace");
        assert_eq!(display_relative(root, root), ".");
        assert_eq!(
            display_relative(root, &root.join("lib/greet.txt")),
            "lib/greet.txt"
        );
    }
}
