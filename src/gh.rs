//! GitHub transport. Like the original Python tool, this delegates auth and
//! API access to the official `gh` CLI rather than handling tokens/HTTP itself.

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::process::Command;

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
        let mut cmd = Command::new(&self.gh_path);
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
