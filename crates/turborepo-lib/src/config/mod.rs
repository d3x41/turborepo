mod env;
mod file;
mod override_env;
mod turbo_json;

use std::{collections::HashMap, ffi::OsString, io};

use camino::{Utf8Path, Utf8PathBuf};
use convert_case::{Case, Casing};
use derive_setters::Setters;
use env::EnvVars;
use file::{AuthFile, ConfigFile};
use merge::Merge;
use miette::{Diagnostic, NamedSource, SourceSpan};
use override_env::OverrideEnvVars;
use serde::Deserialize;
use struct_iterable::Iterable;
use thiserror::Error;
use tracing::debug;
use turbo_json::TurboJsonReader;
use turbopath::{AbsoluteSystemPath, AbsoluteSystemPathBuf};
use turborepo_cache::CacheConfig;
use turborepo_errors::TURBO_SITE;
use turborepo_repository::package_graph::PackageName;

use crate::cli::{EnvMode, LogOrder};
pub use crate::turbo_json::{RawTurboJson, UIMode};

pub const CONFIG_FILE: &str = "turbo.json";
pub const CONFIG_FILE_JSONC: &str = "turbo.jsonc";

#[derive(Debug, Error, Diagnostic)]
#[error("Environment variables should not be prefixed with \"{env_pipeline_delimiter}\"")]
#[diagnostic(
    code(invalid_env_prefix),
    url("{}/messages/{}", TURBO_SITE, self.code().unwrap().to_string().to_case(Case::Kebab))
)]
pub struct InvalidEnvPrefixError {
    pub value: String,
    pub key: String,
    #[source_code]
    pub text: NamedSource<String>,
    #[label("variable with invalid prefix declared here")]
    pub span: Option<SourceSpan>,
    pub env_pipeline_delimiter: &'static str,
}

#[derive(Debug, Error, Diagnostic)]
#[diagnostic(
    code(unnecessary_package_task_syntax),
    url("{}/messages/{}", TURBO_SITE, self.code().unwrap().to_string().to_case(Case::Kebab))
)]
#[error("\"{actual}\". Use \"{wanted}\" instead.")]
pub struct UnnecessaryPackageTaskSyntaxError {
    pub actual: String,
    pub wanted: String,
    #[label("unnecessary package syntax found here")]
    pub span: Option<SourceSpan>,
    #[source_code]
    pub text: NamedSource<String>,
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Error, Diagnostic)]
pub enum Error {
    #[error("Authentication error: {0}")]
    Auth(#[from] turborepo_auth::Error),
    #[error("Global config path not found.")]
    NoGlobalConfigPath,
    #[error("Global authentication file path not found.")]
    NoGlobalAuthFilePath,
    #[error("Global config directory not found.")]
    NoGlobalConfigDir,
    #[error(transparent)]
    PackageJson(#[from] turborepo_repository::package_json::Error),
    #[error(
        "Could not find turbo.json or turbo.jsonc.\nFollow directions at https://turborepo.com/docs \
         to create one."
    )]
    NoTurboJSON,
    #[error(
        "Found both turbo.json and turbo.jsonc in the same directory: {directory}\nRemove either \
         turbo.json or turbo.jsonc so there is only one."
    )]
    MultipleTurboConfigs { directory: String },
    #[error(transparent)]
    SerdeJson(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Camino(#[from] camino::FromPathBufError),
    #[error(transparent)]
    Reqwest(#[from] reqwest::Error),
    #[error("Encountered an I/O error while attempting to read {config_path}: {error}")]
    FailedToReadConfig {
        config_path: AbsoluteSystemPathBuf,
        error: io::Error,
    },
    #[error("Encountered an I/O error while attempting to set {config_path}: {error}")]
    FailedToSetConfig {
        config_path: AbsoluteSystemPathBuf,
        error: io::Error,
    },
    #[error(transparent)]
    Cache(#[from] turborepo_cache::config::Error),
    #[error(
        "Package tasks (<package>#<task>) are not allowed in single-package repositories: found \
         {task_id}"
    )]
    #[diagnostic(code(package_task_in_single_package_mode), url("{}/messages/{}", TURBO_SITE, self.code().unwrap().to_string().to_case(Case::Kebab)))]
    PackageTaskInSinglePackageMode {
        task_id: String,
        #[source_code]
        text: NamedSource<String>,
        #[label("package task found here")]
        span: Option<SourceSpan>,
    },
    #[error("Interruptible tasks must be persistent.")]
    InterruptibleButNotPersistent {
        #[source_code]
        text: NamedSource<String>,
        #[label("`interruptible` set here")]
        span: Option<SourceSpan>,
    },
    #[error(transparent)]
    #[diagnostic(transparent)]
    InvalidEnvPrefix(Box<InvalidEnvPrefixError>),
    #[error(transparent)]
    PathError(#[from] turbopath::PathError),
    #[error(transparent)]
    #[diagnostic(transparent)]
    UnnecessaryPackageTaskSyntax(Box<UnnecessaryPackageTaskSyntaxError>),
    #[error("You can only extend from the root of the workspace.")]
    ExtendFromNonRoot {
        #[label("non-root workspace found here")]
        span: Option<SourceSpan>,
        #[source_code]
        text: NamedSource<String>,
    },
    #[error("`{field}` cannot contain an environment variable.")]
    InvalidDependsOnValue {
        field: &'static str,
        #[label("environment variable found here")]
        span: Option<SourceSpan>,
        #[source_code]
        text: NamedSource<String>,
    },
    #[error("`{field}` cannot contain an absolute path.")]
    AbsolutePathInConfig {
        field: &'static str,
        #[label("absolute path found here")]
        span: Option<SourceSpan>,
        #[source_code]
        text: NamedSource<String>,
    },
    #[error("No \"extends\" key found.")]
    NoExtends {
        #[label("add extends key here")]
        span: Option<SourceSpan>,
        #[source_code]
        text: NamedSource<String>,
    },
    #[error("Tasks cannot be marked as interactive and cacheable.")]
    InteractiveNoCacheable {
        #[label("marked interactive here")]
        span: Option<SourceSpan>,
        #[source_code]
        text: NamedSource<String>,
    },
    #[error("Found `pipeline` field instead of `tasks`.")]
    #[diagnostic(help("Changed in 2.0: `pipeline` has been renamed to `tasks`."))]
    PipelineField {
        #[label("Rename `pipeline` field to `tasks`")]
        span: Option<SourceSpan>,
        #[source_code]
        text: NamedSource<String>,
    },
    #[error("Failed to create APIClient: {0}")]
    ApiClient(#[source] turborepo_api_client::Error),
    #[error("{0} is not UTF8.")]
    Encoding(String),
    #[error("TURBO_SIGNATURE should be either 1 or 0.")]
    InvalidSignature,
    #[error("TURBO_REMOTE_CACHE_ENABLED should be either 1 or 0.")]
    InvalidRemoteCacheEnabled,
    #[error("TURBO_REMOTE_CACHE_TIMEOUT: Error parsing timeout.")]
    InvalidRemoteCacheTimeout(#[source] std::num::ParseIntError),
    #[error("TURBO_REMOTE_CACHE_UPLOAD_TIMEOUT: Error parsing timeout.")]
    InvalidUploadTimeout(#[source] std::num::ParseIntError),
    #[error("TURBO_PREFLIGHT should be either 1 or 0.")]
    InvalidPreflight,
    #[error("TURBO_LOG_ORDER should be one of: {0}")]
    InvalidLogOrder(String),
    #[error(transparent)]
    #[diagnostic(transparent)]
    TurboJsonParseError(#[from] crate::turbo_json::parser::Error),
    #[error("found absolute path in `cacheDir`")]
    #[diagnostic(help("If absolute paths are required, use `--cache-dir` or `TURBO_CACHE_DIR`."))]
    AbsoluteCacheDir {
        #[label("Make `cacheDir` value a relative unix path.")]
        span: Option<SourceSpan>,
        #[source_code]
        text: NamedSource<String>,
    },
    #[error("Cannot load turbo.json for {0} in single package mode.")]
    InvalidTurboJsonLoad(PackageName),
    #[error("\"$TURBO_ROOT$\" must be used at the start of glob.")]
    InvalidTurboRootUse {
        #[label("\"$TURBO_ROOT$\" must be used at the start of glob.")]
        span: Option<SourceSpan>,
        #[source_code]
        text: NamedSource<String>,
    },
    #[error("\"$TURBO_ROOT$\" must be followed by a '/'.")]
    InvalidTurboRootNeedsSlash {
        #[label("\"$TURBO_ROOT$\" must be followed by a '/'.")]
        span: Option<SourceSpan>,
        #[source_code]
        text: NamedSource<String>,
    },
    #[error("`with` cannot use dependency relationships.")]
    InvalidTaskWith {
        #[label("Remove `^` from start of task name.")]
        span: Option<SourceSpan>,
        #[source_code]
        text: NamedSource<String>,
    },
    #[error(
        "The \"futureFlags\" key can only be used in the root turbo.json. Please remove it from \
         Package Configurations."
    )]
    FutureFlagsInPackage {
        #[label("futureFlags key found here")]
        span: Option<SourceSpan>,
        #[source_code]
        text: NamedSource<String>,
    },
    #[error(
        "TURBO_TUI_SCROLLBACK_LENGTH: Invalid value. Use a number for how many lines to keep in \
         scrollback."
    )]
    InvalidTuiScrollbackLength(#[source] std::num::ParseIntError),
    #[error("TURBO_SSO_LOGIN_CALLBACK_PORT: Invalid value. Use a number for the callback port.")]
    InvalidSsoLoginCallbackPort(#[source] std::num::ParseIntError),
}

const DEFAULT_API_URL: &str = "https://vercel.com/api";
const DEFAULT_LOGIN_URL: &str = "https://vercel.com";
const DEFAULT_TIMEOUT: u64 = 30;
const DEFAULT_UPLOAD_TIMEOUT: u64 = 60;
const DEFAULT_TUI_SCROLLBACK_LENGTH: u64 = 2048;

// We intentionally don't derive Serialize so that different parts
// of the code that want to display the config can tune how they
// want to display and what fields they want to include.
#[derive(Deserialize, Default, Debug, PartialEq, Eq, Clone, Iterable, Merge, Setters)]
#[serde(rename_all = "camelCase")]
// Generate setters for the builder type that set these values on its override_config field
#[setters(
    prefix = "with_",
    generate_delegates(ty = "TurborepoConfigBuilder", field = "override_config")
)]
pub struct ConfigurationOptions {
    #[serde(alias = "apiurl")]
    #[serde(alias = "ApiUrl")]
    #[serde(alias = "APIURL")]
    pub(crate) api_url: Option<String>,
    #[serde(alias = "loginurl")]
    #[serde(alias = "LoginUrl")]
    #[serde(alias = "LOGINURL")]
    pub(crate) login_url: Option<String>,
    #[serde(alias = "teamslug")]
    #[serde(alias = "TeamSlug")]
    #[serde(alias = "TEAMSLUG")]
    /// corresponds to env var TURBO_TEAM
    pub(crate) team_slug: Option<String>,
    #[serde(alias = "teamid")]
    #[serde(alias = "TeamId")]
    #[serde(alias = "TEAMID")]
    /// corresponds to env var TURBO_TEAMID
    pub(crate) team_id: Option<String>,
    /// corresponds to env var TURBO_TOKEN
    pub(crate) token: Option<String>,
    pub(crate) signature: Option<bool>,
    pub(crate) preflight: Option<bool>,
    pub(crate) timeout: Option<u64>,
    pub(crate) upload_timeout: Option<u64>,
    pub(crate) enabled: Option<bool>,
    #[serde(rename = "ui")]
    pub(crate) ui: Option<UIMode>,
    #[serde(rename = "dangerouslyDisablePackageManagerCheck")]
    pub(crate) allow_no_package_manager: Option<bool>,
    pub(crate) daemon: Option<bool>,
    #[serde(rename = "envMode")]
    pub(crate) env_mode: Option<EnvMode>,
    pub(crate) scm_base: Option<String>,
    pub(crate) scm_head: Option<String>,
    #[serde(rename = "cacheDir")]
    pub(crate) cache_dir: Option<Utf8PathBuf>,
    // This is skipped as we never want this to be stored in a file
    #[serde(skip)]
    pub(crate) root_turbo_json_path: Option<AbsoluteSystemPathBuf>,
    pub(crate) force: Option<bool>,
    pub(crate) log_order: Option<LogOrder>,
    #[serde(skip)]
    pub(crate) cache: Option<CacheConfig>,
    pub(crate) remote_only: Option<bool>,
    pub(crate) remote_cache_read_only: Option<bool>,
    pub(crate) run_summary: Option<bool>,
    pub(crate) allow_no_turbo_json: Option<bool>,
    pub(crate) tui_scrollback_length: Option<u64>,
    pub(crate) concurrency: Option<String>,
    pub(crate) no_update_notifier: Option<bool>,
    pub(crate) sso_login_callback_port: Option<u16>,
}

#[derive(Default)]
pub struct TurborepoConfigBuilder {
    repo_root: AbsoluteSystemPathBuf,
    override_config: ConfigurationOptions,
    global_config_path: Option<AbsoluteSystemPathBuf>,
    environment: Option<HashMap<OsString, OsString>>,
}

// Getters
impl ConfigurationOptions {
    pub fn api_url(&self) -> &str {
        non_empty_str(self.api_url.as_deref()).unwrap_or(DEFAULT_API_URL)
    }

    pub fn login_url(&self) -> &str {
        non_empty_str(self.login_url.as_deref()).unwrap_or(DEFAULT_LOGIN_URL)
    }

    pub fn team_slug(&self) -> Option<&str> {
        self.team_slug
            .as_deref()
            .and_then(|slug| (!slug.is_empty()).then_some(slug))
    }

    pub fn team_id(&self) -> Option<&str> {
        non_empty_str(self.team_id.as_deref())
    }

    pub fn token(&self) -> Option<&str> {
        non_empty_str(self.token.as_deref())
    }

    pub fn signature(&self) -> bool {
        self.signature.unwrap_or_default()
    }

    pub fn enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    pub fn preflight(&self) -> bool {
        self.preflight.unwrap_or_default()
    }

    /// Note: 0 implies no timeout
    pub fn timeout(&self) -> u64 {
        self.timeout.unwrap_or(DEFAULT_TIMEOUT)
    }

    /// Note: 0 implies no timeout
    pub fn upload_timeout(&self) -> u64 {
        self.upload_timeout.unwrap_or(DEFAULT_UPLOAD_TIMEOUT)
    }

    pub fn tui_scrollback_length(&self) -> u64 {
        self.tui_scrollback_length
            .unwrap_or(DEFAULT_TUI_SCROLLBACK_LENGTH)
    }

    pub fn ui(&self) -> UIMode {
        // If we aren't hooked up to a TTY, then do not use TUI
        if !atty::is(atty::Stream::Stdout) {
            return UIMode::Stream;
        }

        self.log_order()
            .compatible_with_tui()
            .then_some(self.ui)
            .flatten()
            .unwrap_or(UIMode::Stream)
    }

    pub fn scm_base(&self) -> Option<&str> {
        non_empty_str(self.scm_base.as_deref())
    }

    pub fn scm_head(&self) -> Option<&str> {
        non_empty_str(self.scm_head.as_deref())
    }

    pub fn allow_no_package_manager(&self) -> bool {
        self.allow_no_package_manager.unwrap_or_default()
    }

    pub fn daemon(&self) -> Option<bool> {
        // hardcode to off in CI
        if turborepo_ci::is_ci() {
            if Some(true) == self.daemon {
                debug!("Ignoring daemon setting and disabling the daemon because we're in CI");
            }

            return Some(false);
        }

        self.daemon
    }

    pub fn env_mode(&self) -> EnvMode {
        self.env_mode.unwrap_or_default()
    }

    pub fn cache_dir(&self) -> &Utf8Path {
        self.cache_dir.as_deref().unwrap_or_else(|| {
            Utf8Path::new(if cfg!(windows) {
                ".turbo\\cache"
            } else {
                ".turbo/cache"
            })
        })
    }

    pub fn cache(&self) -> Option<CacheConfig> {
        self.cache
    }

    pub fn force(&self) -> bool {
        self.force.unwrap_or_default()
    }

    pub fn log_order(&self) -> LogOrder {
        self.log_order.unwrap_or_default()
    }

    pub fn remote_only(&self) -> bool {
        self.remote_only.unwrap_or_default()
    }

    pub fn remote_cache_read_only(&self) -> bool {
        self.remote_cache_read_only.unwrap_or_default()
    }

    pub fn run_summary(&self) -> bool {
        self.run_summary.unwrap_or_default()
    }

    pub fn root_turbo_json_path(
        &self,
        repo_root: &AbsoluteSystemPath,
    ) -> Result<AbsoluteSystemPathBuf, Error> {
        if let Some(path) = &self.root_turbo_json_path {
            return Ok(path.clone());
        }

        resolve_turbo_config_path(repo_root)
    }

    pub fn allow_no_turbo_json(&self) -> bool {
        self.allow_no_turbo_json.unwrap_or_default()
    }

    pub fn no_update_notifier(&self) -> bool {
        self.no_update_notifier.unwrap_or_default()
    }

    pub fn sso_login_callback_port(&self) -> Option<u16> {
        self.sso_login_callback_port
    }
}

// Maps Some("") to None to emulate how Go handles empty strings
fn non_empty_str(s: Option<&str>) -> Option<&str> {
    s.filter(|s| !s.is_empty())
}

trait ResolvedConfigurationOptions {
    fn get_configuration_options(
        &self,
        existing_config: &ConfigurationOptions,
    ) -> Result<ConfigurationOptions, Error>;
}

// Used for global config and local config.
impl<'a> ResolvedConfigurationOptions for &'a ConfigurationOptions {
    fn get_configuration_options(
        &self,
        _existing_config: &ConfigurationOptions,
    ) -> Result<ConfigurationOptions, Error> {
        Ok((*self).clone())
    }
}

fn get_lowercased_env_vars() -> HashMap<OsString, OsString> {
    std::env::vars_os()
        .map(|(k, v)| (k.to_ascii_lowercase(), v))
        .collect()
}

impl TurborepoConfigBuilder {
    pub fn new(repo_root: &AbsoluteSystemPath) -> Self {
        Self {
            repo_root: repo_root.to_owned(),
            override_config: Default::default(),
            global_config_path: None,
            environment: None,
        }
    }

    pub fn with_global_config_path(mut self, path: AbsoluteSystemPathBuf) -> Self {
        self.global_config_path = Some(path);
        self
    }

    fn get_environment(&self) -> HashMap<OsString, OsString> {
        self.environment
            .clone()
            .unwrap_or_else(get_lowercased_env_vars)
    }

    pub fn build(&self) -> Result<ConfigurationOptions, Error> {
        // Priority, from least significant to most significant:
        // - shared configuration (turbo.json)
        // - global configuration (~/.turbo/config.json)
        // - local configuration (<REPO_ROOT>/.turbo/config.json)
        // - environment variables
        // - CLI arguments
        // - builder pattern overrides.

        let turbo_json = TurboJsonReader::new(&self.repo_root);
        let global_config = ConfigFile::global_config(self.global_config_path.clone())?;
        let global_auth = AuthFile::global_auth(self.global_config_path.clone())?;
        let local_config = ConfigFile::local_config(&self.repo_root);
        let env_vars = self.get_environment();
        let env_var_config = EnvVars::new(&env_vars)?;
        let override_env_var_config = OverrideEnvVars::new(&env_vars)?;

        // These are ordered from highest to lowest priority
        let sources: [Box<dyn ResolvedConfigurationOptions>; 7] = [
            Box::new(&self.override_config),
            Box::new(env_var_config),
            Box::new(override_env_var_config),
            Box::new(local_config),
            Box::new(global_auth),
            Box::new(global_config),
            Box::new(turbo_json),
        ];

        let config = sources.into_iter().try_fold(
            ConfigurationOptions::default(),
            |mut acc, current_source| {
                let current_source_config = current_source.get_configuration_options(&acc)?;
                acc.merge(current_source_config);
                Ok(acc)
            },
        );

        // We explicitly do a let and return to help the Rust compiler see that there
        // are no references still held by the folding.
        #[allow(clippy::let_and_return)]
        config
    }
}

/// Given a directory path, determines which turbo.json configuration file to
/// use. Returns an error if both turbo.json and turbo.jsonc exist in the same
/// directory. Returns the path to the config file to use, defaulting to
/// turbo.json if neither exists.
pub fn resolve_turbo_config_path(
    dir_path: &turbopath::AbsoluteSystemPath,
) -> Result<turbopath::AbsoluteSystemPathBuf, crate::config::Error> {
    use crate::config::Error;

    let turbo_json_path = dir_path.join_component(CONFIG_FILE);
    let turbo_jsonc_path = dir_path.join_component(CONFIG_FILE_JSONC);

    let turbo_json_exists = turbo_json_path.try_exists()?;
    let turbo_jsonc_exists = turbo_jsonc_path.try_exists()?;

    match (turbo_json_exists, turbo_jsonc_exists) {
        (true, true) => Err(Error::MultipleTurboConfigs {
            directory: dir_path.to_string(),
        }),
        (true, false) => Ok(turbo_json_path),
        (false, true) => Ok(turbo_jsonc_path),
        // Default to turbo.json if neither exists
        (false, false) => Ok(turbo_json_path),
    }
}

#[cfg(test)]
mod test {
    use std::{collections::HashMap, ffi::OsString};

    use tempfile::TempDir;
    use turbopath::{AbsoluteSystemPath, AbsoluteSystemPathBuf};

    use crate::config::{
        ConfigurationOptions, TurborepoConfigBuilder, CONFIG_FILE, CONFIG_FILE_JSONC,
        DEFAULT_API_URL, DEFAULT_LOGIN_URL, DEFAULT_TIMEOUT,
    };

    #[test]
    fn test_defaults() {
        let defaults: ConfigurationOptions = Default::default();
        assert_eq!(defaults.api_url(), DEFAULT_API_URL);
        assert_eq!(defaults.login_url(), DEFAULT_LOGIN_URL);
        assert_eq!(defaults.team_slug(), None);
        assert_eq!(defaults.team_id(), None);
        assert_eq!(defaults.token(), None);
        assert!(!defaults.signature());
        assert!(defaults.enabled());
        assert!(!defaults.preflight());
        assert_eq!(defaults.timeout(), DEFAULT_TIMEOUT);
        assert!(!defaults.allow_no_package_manager());
        let repo_root = AbsoluteSystemPath::new(if cfg!(windows) {
            "C:\\fake\\repo"
        } else {
            "/fake/repo"
        })
        .unwrap();
        assert_eq!(
            defaults.root_turbo_json_path(repo_root).unwrap(),
            repo_root.join_component("turbo.json")
        )
    }

    #[test]
    fn test_env_layering() {
        let tmp_dir = TempDir::new().unwrap();
        let repo_root = AbsoluteSystemPathBuf::try_from(tmp_dir.path()).unwrap();
        let global_config_path = AbsoluteSystemPathBuf::try_from(
            TempDir::new().unwrap().path().join("nonexistent.json"),
        )
        .unwrap();

        repo_root
            .join_component("turbo.json")
            .create_with_contents(r#"{"experimentalSpaces": {"id": "my-spaces-id"}}"#)
            .unwrap();

        let turbo_teamid = "team_nLlpyC6REAqxydlFKbrMDlud";
        let turbo_token = "abcdef1234567890abcdef";
        let vercel_artifacts_owner = "team_SOMEHASH";
        let vercel_artifacts_token = "correct-horse-battery-staple";

        let mut env: HashMap<OsString, OsString> = HashMap::new();
        env.insert("turbo_teamid".into(), turbo_teamid.into());
        env.insert("turbo_token".into(), turbo_token.into());
        env.insert(
            "vercel_artifacts_token".into(),
            vercel_artifacts_token.into(),
        );
        env.insert(
            "vercel_artifacts_owner".into(),
            vercel_artifacts_owner.into(),
        );

        let builder = TurborepoConfigBuilder {
            repo_root,
            override_config: Default::default(),
            global_config_path: Some(global_config_path),
            environment: Some(env),
        };

        let config = builder.build().unwrap();
        assert_eq!(config.team_id().unwrap(), turbo_teamid);
        assert_eq!(config.token().unwrap(), turbo_token);
    }

    #[test]
    fn test_turbo_json_remote_cache() {
        let tmp_dir = TempDir::new().unwrap();
        let repo_root = AbsoluteSystemPathBuf::try_from(tmp_dir.path()).unwrap();

        let api_url = "url1";
        let login_url = "url2";
        let team_slug = "my-slug";
        let team_id = "an-id";
        let turbo_json_contents = serde_json::to_string_pretty(&serde_json::json!({
            "remoteCache": {
                "enabled": true,
                "apiUrl": api_url,
                "loginUrl": login_url,
                "teamSlug": team_slug,
                "teamId": team_id,
                "signature": true,
                "preflight": false,
                "timeout": 123
            }
        }))
        .unwrap();
        repo_root
            .join_component("turbo.json")
            .create_with_contents(&turbo_json_contents)
            .unwrap();

        let builder = TurborepoConfigBuilder {
            repo_root,
            override_config: ConfigurationOptions::default(),
            global_config_path: None,
            environment: Some(HashMap::default()),
        };

        let config = builder.build().unwrap();
        // Directly accessing field to make sure we're not getting the default value
        assert_eq!(config.enabled, Some(true));
        assert_eq!(config.api_url(), api_url);
        assert_eq!(config.login_url(), login_url);
        assert_eq!(config.team_slug(), Some(team_slug));
        assert_eq!(config.team_id(), Some(team_id));
        assert!(config.signature());
        assert!(!config.preflight());
        assert_eq!(config.timeout(), 123);
    }

    #[test]
    fn test_multiple_turbo_configs() {
        let tmp_dir = TempDir::new().unwrap();
        let repo_root = AbsoluteSystemPath::from_std_path(tmp_dir.path()).unwrap();

        // Create both turbo.json and turbo.jsonc
        let turbo_json_path = repo_root.join_component(CONFIG_FILE);
        let turbo_jsonc_path = repo_root.join_component(CONFIG_FILE_JSONC);

        turbo_json_path.create_with_contents("{}").unwrap();
        turbo_jsonc_path.create_with_contents("{}").unwrap();

        // Test ConfigurationOptions.root_turbo_json_path
        let config = ConfigurationOptions::default();
        let result = config.root_turbo_json_path(repo_root);
        assert!(result.is_err());
    }

    #[test]
    fn test_only_turbo_json() {
        let tmp_dir = TempDir::new().unwrap();
        let repo_root = AbsoluteSystemPath::from_std_path(tmp_dir.path()).unwrap();

        // Create only turbo.json
        let turbo_json_path = repo_root.join_component(CONFIG_FILE);
        turbo_json_path.create_with_contents("{}").unwrap();

        // Test ConfigurationOptions.root_turbo_json_path
        let config = ConfigurationOptions::default();
        let result = config.root_turbo_json_path(repo_root);

        assert_eq!(result.unwrap(), turbo_json_path);
    }

    #[test]
    fn test_only_turbo_jsonc() {
        let tmp_dir = TempDir::new().unwrap();
        let repo_root = AbsoluteSystemPath::from_std_path(tmp_dir.path()).unwrap();

        // Create only turbo.jsonc
        let turbo_jsonc_path = repo_root.join_component(CONFIG_FILE_JSONC);
        turbo_jsonc_path.create_with_contents("{}").unwrap();

        // Test ConfigurationOptions.root_turbo_json_path
        let config = ConfigurationOptions::default();
        let result = config.root_turbo_json_path(repo_root);

        assert_eq!(result.unwrap(), turbo_jsonc_path);
    }

    #[test]
    fn test_no_turbo_config() {
        let tmp_dir = TempDir::new().unwrap();
        let repo_root = AbsoluteSystemPath::from_std_path(tmp_dir.path()).unwrap();

        // Test ConfigurationOptions.root_turbo_json_path
        let config = ConfigurationOptions::default();
        let result = config.root_turbo_json_path(repo_root);

        assert_eq!(result.unwrap(), repo_root.join_component(CONFIG_FILE));
    }
}
