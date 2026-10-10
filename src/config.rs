//! Configuration: finds the file, reads it, validates it, and can EXPLAIN what went wrong
//! and where.
//!
//! Search order -- the first one that exists wins:
//!
//!   1. `--config <path>`
//!   2. `$YT_TRANSCRIBE_CONFIG`
//!   3. `$XDG_CONFIG_HOME/yt-transcribe/config.json` (or `~/.config/yt-transcribe/config.json`)
//!   4. `/etc/yt-transcribe/config.json`
//!
//! Unlike its sibling the rustranslator -- where no file is fatal, because without an endpoint and a
//! model name the tool cannot work at all -- the file here is **optional**: this tool has built-in
//! defaults, so no file anywhere means exactly the behaviour it had before the file existed. What
//! IS an error is asking for one specific file with `--config <path>` and not finding it: that
//! request can only be meant.
//!
//! An unknown key is an error too, naming the file, the line and the column -- a configuration that
//! accepts garbage hides the typo that was made.
//!
//! **The environment comes in as a parameter.** Nothing in this module reads an environment
//! variable on its own: the only reader is [`Environment::real()`], and only the binary calls it.
//! That keeps path resolution pure and testable, and safe under parallel tests -- mutating the
//! process environment inside a test is a trap.

use serde::Deserialize;
use std::fmt;
use std::path::{Path, PathBuf};

pub const APP: &str = "yt-transcribe";
pub const ENV_CONFIG: &str = "YT_TRANSCRIBE_CONFIG";

/// What path resolution needs to know about the world. It always comes from outside.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Environment {
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub env_config: Option<PathBuf>,
}

impl Environment {
    /// Reads the process's REAL environment. This is the only place in the module that touches the
    /// environment -- and nothing in the library calls it; the binary does.
    pub fn real() -> Self {
        let var = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
        Self {
            home: var("HOME"),
            xdg_config_home: var("XDG_CONFIG_HOME"),
            env_config: var(ENV_CONFIG),
        }
    }
}

/// The keys the file accepts, every one of them optional.
///
/// Optional on purpose: "not declared" and "declared with the default value" are different facts,
/// and the resolution below has to tell them apart -- a key that is not in the file must not stop
/// the step below it in the chain from being consulted.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Preferred caption language (`--lang`).
    pub lang: Option<String>,
    /// Output directory (`--output`).
    pub output: Option<PathBuf>,
    /// Model directory (`--model`).
    pub model: Option<PathBuf>,
    /// Netscape cookie file (`--cookies`).
    pub cookies: Option<PathBuf>,
    /// Always transcribe the audio, ignoring existing captions (`--force-transcribe`).
    pub force_transcribe: Option<bool>,
    /// Transcribe from the first block again (`--force`).
    pub force: Option<bool>,
}

#[derive(Debug)]
pub enum ConfigError {
    /// `--config <path>` was given and there is nothing there. A file nobody asked for is not an
    /// error; a file somebody named is.
    MissingExplicit { path: PathBuf },
    Read { path: PathBuf, source: std::io::Error },
    /// Invalid JSON, or a key this build does not know. Names the path, the line and the column.
    Json {
        path: PathBuf,
        line: usize,
        column: usize,
        msg: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::MissingExplicit { path } => write!(
                f,
                "no configuration file at {} -- it was asked for by name (--config), so it has to \
                 be there",
                path.display()
            ),
            ConfigError::Read { path, source } => {
                write!(f, "could not read {}: {source}", path.display())
            }
            ConfigError::Json { path, line, column, msg } => write!(
                f,
                "{} is invalid at line {line}, column {column}: {msg}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

/// `$XDG_CONFIG_HOME/yt-transcribe/config.json`, falling back to `~/.config/...`.
pub fn user_config_path(env: &Environment) -> Option<PathBuf> {
    let base = env
        .xdg_config_home
        .clone()
        .or_else(|| env.home.as_ref().map(|h| h.join(".config")))?;
    Some(base.join(APP).join("config.json"))
}

pub fn system_config_path() -> PathBuf {
    PathBuf::from("/etc").join(APP).join("config.json")
}

/// The paths to try, in order. The environment one only enters if it is defined.
pub fn candidates(explicit: Option<&Path>, env: &Environment) -> Vec<PathBuf> {
    explicit
        .map(Path::to_path_buf)
        .into_iter()
        .chain(env.env_config.clone())
        .chain(user_config_path(env))
        .chain(std::iter::once(system_config_path()))
        .collect()
}

/// The configuration in force: the file found and what it holds.
///
/// `Ok(None)` is not a failure -- it means no file exists anywhere in the chain, and the tool runs
/// on its built-in defaults, exactly as it did before this module existed. `Err(MissingExplicit)`
/// is the one case where a missing file is a failure.
pub fn resolve(
    explicit: Option<&Path>,
    env: &Environment,
) -> Result<Option<(PathBuf, Config)>, ConfigError> {
    // A file named on the command line is THE file: measured 2026-10-10, without this the search
    // walked on and silently loaded the user's own file when the named one was missing -- the
    // opposite of what naming a file means, and the one case where the missing file IS an error.
    if let Some(path) = explicit {
        if !path.is_file() {
            return Err(ConfigError::MissingExplicit {
                path: path.to_path_buf(),
            });
        }
    }

    let tried = candidates(explicit, env);
    let Some(path) = tried.iter().find(|p| p.is_file()).cloned() else {
        return Ok(None);
    };

    let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
        path: path.clone(),
        source,
    })?;

    let cfg: Config = serde_json::from_str(&text).map_err(|e| ConfigError::Json {
        path: path.clone(),
        line: e.line(),
        column: e.column(),
        msg: e.to_string(),
    })?;

    Ok(Some((path, cfg)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake environment, complete and deterministic. No test here touches the real one.
    fn fake_env() -> Environment {
        Environment {
            home: Some(PathBuf::from("/home/fake")),
            xdg_config_home: None,
            env_config: None,
        }
    }

    fn test_file(name: &str, content: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yt-transcribe-config-test-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("config.json");
        std::fs::write(&file, content).unwrap();
        file
    }

    #[test]
    fn the_user_config_path_prefers_xdg() {
        let env = Environment {
            home: Some(PathBuf::from("/home")),
            xdg_config_home: Some(PathBuf::from("/etc/xdg-of-the-user")),
            env_config: None,
        };
        assert_eq!(
            user_config_path(&env).unwrap(),
            PathBuf::from("/etc/xdg-of-the-user/yt-transcribe/config.json")
        );
        assert_eq!(
            user_config_path(&fake_env()).unwrap(),
            PathBuf::from("/home/fake/.config/yt-transcribe/config.json")
        );
    }

    #[test]
    fn candidate_order_without_env() {
        let c = candidates(Some(Path::new("/tmp/explicit.json")), &fake_env());
        assert_eq!(
            c,
            vec![
                PathBuf::from("/tmp/explicit.json"),
                PathBuf::from("/home/fake/.config/yt-transcribe/config.json"),
                PathBuf::from("/etc/yt-transcribe/config.json"),
            ]
        );
    }

    #[test]
    fn candidate_order_with_env() {
        let env = Environment {
            env_config: Some(PathBuf::from("/tmp/from-env.json")),
            ..fake_env()
        };
        let c = candidates(None, &env);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0], PathBuf::from("/tmp/from-env.json"));
        assert_eq!(c.last().unwrap(), &system_config_path());
    }

    #[test]
    fn a_file_nobody_asked_for_is_not_an_error() {
        // The whole point of the difference from the rustranslator: no file, no failure.
        let env = Environment {
            env_config: Some(PathBuf::from("/tmp/definitely-not-here-yt.json")),
            ..fake_env()
        };
        assert!(resolve(None, &env).unwrap().is_none());
    }

    #[test]
    fn a_file_asked_for_by_name_has_to_be_there() {
        let e = resolve(Some(Path::new("/tmp/named-but-absent-yt.json")), &fake_env()).unwrap_err();
        let text = e.to_string();
        assert!(text.contains("/tmp/named-but-absent-yt.json"), "text: {text}");
        assert!(text.contains("--config"), "text: {text}");
    }

    #[test]
    fn a_named_file_that_is_absent_is_an_error_even_when_another_one_exists() {
        // Measured 2026-10-10: without this, `--config /no/such/file.json` loaded the user's own
        // file instead and the run went on happily -- the opposite of naming a file.
        let present = test_file("present-but-not-named", r#"{ "lang": "en" }"#);
        let env = Environment {
            env_config: Some(present),
            ..fake_env()
        };
        let e = resolve(Some(Path::new("/tmp/named-absent-yt.json")), &env).unwrap_err();
        assert!(e.to_string().contains("/tmp/named-absent-yt.json"), "{e}");
    }

    #[test]
    fn an_empty_file_declares_nothing() {
        let file = test_file("empty", "{}");
        let (path, cfg) = resolve(Some(&file), &fake_env()).unwrap().unwrap();
        assert_eq!(path, file);
        assert_eq!(cfg, Config::default());
        // Every key stays `None`, which is what lets the step below be consulted.
        assert!(cfg.lang.is_none() && cfg.output.is_none() && cfg.model.is_none());
    }

    #[test]
    fn reads_the_keys_the_cli_accepts() {
        let file = test_file(
            "full",
            r#"{
  "lang": "pt",
  "output": "/tmp/out",
  "model": "/tmp/tdt",
  "cookies": "/tmp/cookies.txt",
  "force_transcribe": true,
  "force": false
}"#,
        );
        let (_, cfg) = resolve(Some(&file), &fake_env()).unwrap().unwrap();
        assert_eq!(cfg.lang.as_deref(), Some("pt"));
        assert_eq!(cfg.output, Some(PathBuf::from("/tmp/out")));
        assert_eq!(cfg.model, Some(PathBuf::from("/tmp/tdt")));
        assert_eq!(cfg.cookies, Some(PathBuf::from("/tmp/cookies.txt")));
        assert_eq!(cfg.force_transcribe, Some(true));
        assert_eq!(cfg.force, Some(false));
    }

    #[test]
    fn an_unknown_key_errors_and_names_the_line() {
        let file = test_file(
            "typo",
            r#"{
  "output": "/tmp/out",
  "modell": "/tmp/tdt"
}"#,
        );
        let msg = resolve(Some(&file), &fake_env()).unwrap_err().to_string();
        assert!(msg.contains("config.json"), "msg: {msg}");
        assert!(msg.contains("line 3"), "msg: {msg}");
        assert!(msg.contains("modell"), "msg: {msg}");
    }

    #[test]
    fn a_wrong_type_errors_and_names_the_position() {
        let file = test_file("type", r#"{ "force": "yes" }"#);
        let msg = resolve(Some(&file), &fake_env()).unwrap_err().to_string();
        assert!(msg.contains("line 1"), "msg: {msg}");
    }

    #[test]
    fn broken_json_errors_with_its_position() {
        let file = test_file("broken", "{ \"output\": }");
        let msg = resolve(Some(&file), &fake_env()).unwrap_err().to_string();
        assert!(msg.contains("line 1"), "msg: {msg}");
    }

    #[test]
    fn the_explicit_file_beats_the_environment_one() {
        let explicit = test_file("prec-explicit", r#"{ "lang": "pt" }"#);
        let from_env = test_file("prec-env", r#"{ "lang": "en" }"#);
        let env = Environment {
            env_config: Some(from_env),
            ..fake_env()
        };
        let (path, cfg) = resolve(Some(&explicit), &env).unwrap().unwrap();
        assert_eq!(path, explicit);
        assert_eq!(cfg.lang.as_deref(), Some("pt"));
    }
}
