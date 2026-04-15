//! Project sandbox: file-system tools must stay within the project root.
//!
//! Call `init()` once at startup.  Tools call:
//!  - `check_read(path)`  — lexical check; symlink reads allowed (knowledge base)
//!  - `check_write(path)` — also canonicalises nearest existing parent to block
//!                          symlink escapes (e.g. writing through `.yggdra/knowledge`)
//!
//! Both return `Ok(())` when uninitialised (test context).

use anyhow::{anyhow, Result};
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

static PROJECT_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Initialise the sandbox with the project root.  Must be called once at
/// startup before any tool execution.
pub fn init(root: PathBuf) {
    let canonical = root.canonicalize().unwrap_or(root);
    let _ = PROJECT_ROOT.set(canonical);
}

/// Return the canonical project root, or `None` if uninitialised.
pub fn project_root() -> Option<&'static Path> {
    PROJECT_ROOT.get().map(|p| p.as_path())
}

// ── Path helpers ─────────────────────────────────────────────────────────────

/// Lexically normalise a path: resolve `.` and `..` without touching the FS.
/// Works for paths that don't exist yet (writefile targets).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => { out.pop(); }
            Component::CurDir    => {}
            c                    => out.push(c),
        }
    }
    out
}

/// Expand a leading `~/` to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("~"));
    }
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(&path[2..]);
        }
    }
    PathBuf::from(path)
}

/// Resolve a user-supplied path string to an absolute, normalised `PathBuf`.
/// Relative paths are anchored to the project root (or `.` if uninitialised).
pub fn resolve(path: &str) -> PathBuf {
    let expanded = expand_tilde(path);
    if expanded.is_absolute() {
        normalize(&expanded)
    } else {
        let base = PROJECT_ROOT
            .get()
            .map(|p| p.as_path())
            .unwrap_or_else(|| Path::new("."));
        normalize(&base.join(expanded))
    }
}

/// Walk up `path` until an existing ancestor is found, canonicalise it, then
/// re-append the non-existent suffix.  Used to detect symlink escapes for
/// paths that don't exist yet.
fn canonicalize_nearest_parent(path: &Path) -> PathBuf {
    let mut suffix: Vec<&OsStr> = Vec::new();
    let mut current = path;

    loop {
        if current.exists() {
            let canon = current.canonicalize().unwrap_or_else(|_| current.to_path_buf());
            return suffix.iter().rev().fold(canon, |acc, part| acc.join(part));
        }
        match current.file_name() {
            Some(name) => {
                suffix.push(name);
                match current.parent() {
                    Some(parent) => current = parent,
                    None         => break,
                }
            }
            None => break,
        }
    }
    path.to_path_buf()
}

// ── Public API ────────────────────────────────────────────────────────────────

fn outside_root_error(path: &str, root: &Path) -> anyhow::Error {
    anyhow!(
        "Path '{}' is outside the project root.\n\
         ► All files must live inside: {}\n\
         ► Use relative paths (e.g. src/foo.rs) or absolute paths under that directory.\n\
         ► Do NOT write to parent directories, other repositories, or system paths.",
        path,
        root.display()
    )
}

/// Check that `path` is readable (lexical containment — symlink reads allowed
/// so the knowledge base at `.yggdra/knowledge` is accessible).
///
/// Returns `Ok(resolved_path)` on success so callers can use the canonical form.
pub fn check_read(path: &str) -> Result<PathBuf> {
    let root = match PROJECT_ROOT.get() {
        Some(r) => r,
        None    => return Ok(resolve(path)),
    };
    let resolved = resolve(path);
    if resolved.starts_with(root) {
        Ok(resolved)
    } else {
        Err(outside_root_error(path, root))
    }
}

/// Check that `path` is writable.  Beyond the lexical check, this
/// canonicalises the nearest existing parent to block symlink escapes
/// (e.g. writing through `.yggdra/knowledge` into the offline docs).
///
/// Returns `Ok(resolved_path)` on success.
pub fn check_write(path: &str) -> Result<PathBuf> {
    let root = match PROJECT_ROOT.get() {
        Some(r) => r,
        None    => return Ok(resolve(path)),
    };
    let resolved = resolve(path);
    if !resolved.starts_with(root) {
        return Err(outside_root_error(path, root));
    }
    // Detect symlink escapes by canonicalising the nearest existing parent.
    let canon = canonicalize_nearest_parent(&resolved);
    if !canon.starts_with(root) {
        return Err(anyhow!(
            "Path '{}' escapes the project root through a symlink.\n\
             ► Files must stay inside: {}",
            path,
            root.display()
        ));
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_dotdot() {
        assert_eq!(normalize(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
    }

    #[test]
    fn test_normalize_dot() {
        assert_eq!(normalize(Path::new("/a/./b/./c")), PathBuf::from("/a/b/c"));
    }

    #[test]
    fn test_resolve_absolute_passthrough() {
        let r = resolve("/absolute/path/foo.rs");
        assert_eq!(r, PathBuf::from("/absolute/path/foo.rs"));
    }

    #[test]
    fn test_resolve_strips_dotdot() {
        // Relative path anchored to "." when uninitialised; no ".." in result
        let r = resolve("src/../../etc/passwd");
        assert!(!r.to_string_lossy().contains(".."));
    }

    #[test]
    fn test_check_read_uninitialised_allows_anything() {
        // If sandbox not initialised, check_read passes through
        // (OnceLock may already be set in the test binary; this exercises the
        // logic path via resolve() directly)
        let r = resolve("/any/arbitrary/path");
        assert_eq!(r, PathBuf::from("/any/arbitrary/path"));
    }

    #[test]
    fn test_check_write_uninitialised_allows_anything() {
        assert!(check_write("/any/path/foo.rs").is_ok());
    }

    #[test]
    fn test_check_read_uninitialised_allows_read() {
        assert!(check_read("/any/path/foo.rs").is_ok());
    }
}
