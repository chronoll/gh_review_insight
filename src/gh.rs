//! GitHub transport. Like the original Python tool, this delegates auth and
//! API access to the official `gh` CLI rather than handling tokens/HTTP itself.

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::path::Path;
use std::process::Command;

/// Directories commonly holding `gh` that are missing from the minimal PATH a
/// GUI launch (Dock / Spotlight / login item) inherits.
const GH_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"];

/// Resolve the `gh` binary. When launched from the terminal, PATH already has
/// it; from a `.app` it usually does not, so fall back to common locations.
fn resolve_gh(preferred: &str) -> String {
    let candidates: Vec<String> = GH_DIRS.iter().map(|dir| format!("{dir}/gh")).collect();
    resolve_gh_in(preferred, &candidates)
}

fn resolve_gh_in(preferred: &str, candidates: &[String]) -> String {
    // An explicit path that exists wins.
    if preferred.contains('/') && Path::new(preferred).exists() {
        return preferred.to_string();
    }
    // Otherwise probe common install locations.
    for candidate in candidates {
        if Path::new(candidate).exists() {
            return candidate.clone();
        }
    }
    // Last resort: let the OS resolve it via PATH.
    preferred.to_string()
}

/// PATH augmented with the common bin dirs, for any tools `gh` itself spawns.
fn augmented_path() -> String {
    let existing = std::env::var("PATH").unwrap_or_default();
    format!("{}:{existing}", GH_DIRS.join(":"))
}

const VIEWER_QUERY: &str = r#"
query {
  viewer {
    login
  }
}
"#;

/// A GraphQL variable value. `gh api graphql` distinguishes raw (`-F`, used for
/// numbers/bools) from string (`-f`) arguments.
pub enum GqlVar {
    Str(String),
    Int(i64),
}

/// Thin wrapper around `gh api graphql`.
pub struct GhClient {
    pub gh_path: String,
}

impl GhClient {
    pub fn new(gh_path: impl Into<String>) -> Self {
        Self {
            gh_path: gh_path.into(),
        }
    }

    /// Run a GraphQL query through `gh` and return the parsed JSON response.
    pub fn graphql(&self, query: &str, vars: &[(&str, GqlVar)]) -> Result<Value> {
        let mut cmd = Command::new(resolve_gh(&self.gh_path));
        cmd.env("PATH", augmented_path());
        cmd.arg("api").arg("graphql").arg("-f").arg(format!("query={query}"));
        for (key, value) in vars {
            match value {
                GqlVar::Str(s) => {
                    cmd.arg("-f").arg(format!("{key}={s}"));
                }
                GqlVar::Int(n) => {
                    cmd.arg("-F").arg(format!("{key}={n}"));
                }
            }
        }

        let output = cmd.output().map_err(|err| {
            anyhow!(
                "GitHub CLI `gh` を実行できませんでした（先に `gh auth login` まで済ませてください）: {err}"
            )
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let trimmed = stderr.trim();
            return Err(anyhow!(if trimmed.is_empty() {
                "`gh api graphql` の実行に失敗しました。".to_string()
            } else {
                trimmed.to_string()
            }));
        }

        serde_json::from_slice(&output.stdout)
            .context("`gh` のJSON応答を解析できませんでした")
    }

    /// The login of the currently authenticated user.
    pub fn viewer_login(&self) -> Result<String> {
        let data = self.graphql(VIEWER_QUERY, &[])?;
        data["data"]["viewer"]["login"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| anyhow!("GitHub viewer login を取得できませんでした。"))
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_gh_in;

    #[test]
    fn explicit_existing_path_wins() {
        let exe = std::env::current_exe().unwrap();
        let path = exe.to_string_lossy().to_string();
        assert_eq!(resolve_gh_in(&path, &["/nonexistent/gh".to_string()]), path);
    }

    #[test]
    fn bare_name_uses_existing_candidate() {
        let exe = std::env::current_exe().unwrap();
        let path = exe.to_string_lossy().to_string();
        assert_eq!(resolve_gh_in("gh", &[path.clone()]), path);
    }

    #[test]
    fn falls_back_to_preferred_when_nothing_found() {
        assert_eq!(resolve_gh_in("gh", &["/nope/gh".to_string()]), "gh");
    }
}
