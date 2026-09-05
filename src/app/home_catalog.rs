use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use serde::Deserialize;

use crate::detect::Agent;

pub(crate) const DEFAULT_MODEL: &str = "default";
pub(crate) const AUTO_EFFORT: &str = "auto";
pub(crate) const DEFAULT_CONTEXT_WINDOW: &str = "200k";
pub(crate) const LARGE_CONTEXT_WINDOW: &str = "1M";
const CATALOG_TIMEOUT: Duration = Duration::from_millis(750);
const CATALOG_OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
const CLAUDE_HELP_CACHE_FILE: &str = "herdr-claude-help.txt";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClaudeContextWindowForm {
    ModelAlias,
    Flag(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeModelCatalogEntry {
    pub(crate) id: String,
    pub(crate) display_name: String,
    pub(crate) efforts: Vec<String>,
    pub(crate) supports_large_context: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeProviderCatalog {
    pub(crate) agent: Agent,
    pub(crate) models: Vec<HomeModelCatalogEntry>,
    pub(crate) claude_context_form: Option<ClaudeContextWindowForm>,
}

impl HomeProviderCatalog {
    pub(crate) fn model(&self, id: &str) -> Option<&HomeModelCatalogEntry> {
        self.models.iter().find(|model| model.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeCatalog {
    providers: Vec<HomeProviderCatalog>,
}

impl Default for HomeCatalog {
    fn default() -> Self {
        Self::fallback()
    }
}

impl HomeCatalog {
    pub(crate) fn fallback() -> Self {
        Self {
            providers: vec![claude_catalog(None), default_codex_catalog()],
        }
    }

    #[cfg(test)]
    pub(crate) fn with_codex(codex: HomeProviderCatalog) -> Self {
        debug_assert_eq!(codex.agent, Agent::Codex);
        Self {
            providers: vec![claude_catalog(None), codex],
        }
    }

    #[cfg(test)]
    pub(crate) fn with_claude(claude: HomeProviderCatalog) -> Self {
        debug_assert_eq!(claude.agent, Agent::Claude);
        Self {
            providers: vec![claude, default_codex_catalog()],
        }
    }

    fn with_claude_and_codex(claude: HomeProviderCatalog, codex: HomeProviderCatalog) -> Self {
        debug_assert_eq!(claude.agent, Agent::Claude);
        debug_assert_eq!(codex.agent, Agent::Codex);
        Self {
            providers: vec![claude, codex],
        }
    }

    pub(crate) fn provider(&self, agent: Agent) -> Option<&HomeProviderCatalog> {
        self.providers
            .iter()
            .find(|provider| provider.agent == agent)
    }

    pub(crate) fn replace(&mut self, provider: HomeProviderCatalog) {
        if let Some(existing) = self
            .providers
            .iter_mut()
            .find(|existing| existing.agent == provider.agent)
        {
            *existing = provider;
        } else {
            self.providers.push(provider);
        }
    }
}

fn entry(id: &str, efforts: &[&str]) -> HomeModelCatalogEntry {
    HomeModelCatalogEntry {
        id: id.into(),
        display_name: id.into(),
        efforts: efforts.iter().map(|effort| (*effort).into()).collect(),
        supports_large_context: false,
    }
}

fn claude_catalog(help: Option<&str>) -> HomeProviderCatalog {
    let efforts = claude_efforts_from_help(help.unwrap_or_default());
    let model =
        |display_name: &str, id: &str, supports_large_context: bool| HomeModelCatalogEntry {
            id: id.into(),
            display_name: display_name.into(),
            efforts: efforts.clone(),
            supports_large_context,
        };
    HomeProviderCatalog {
        agent: Agent::Claude,
        models: vec![
            model(DEFAULT_MODEL, DEFAULT_MODEL, false),
            model("Claude Fable 5.1", "claude-fable-5-1", true),
            model("Claude Opus 5", "claude-opus-5", true),
            model("Claude Sonnet 5", "claude-sonnet-5", true),
            model("Claude Haiku 4.5", "claude-haiku-4-5-20251001", false),
        ],
        claude_context_form: Some(claude_context_form_from_help(help.unwrap_or_default())),
    }
}

pub(crate) fn parse_claude_help(help: &str) -> HomeProviderCatalog {
    claude_catalog(Some(help))
}

fn option_block(help: &str, option: &str) -> Option<String> {
    let mut block = String::new();
    let mut found = false;
    for line in help.lines() {
        let trimmed = line.trim_start();
        if !found {
            if trimmed.starts_with('-') && trimmed.contains(option) {
                found = true;
                block.push_str(trimmed);
            }
            continue;
        }
        if trimmed.starts_with('-') {
            break;
        }
        block.push(' ');
        block.push_str(trimmed);
    }
    found.then_some(block)
}

fn parse_choice_list(raw: &str) -> Vec<String> {
    let raw = raw
        .trim()
        .strip_prefix("choices:")
        .map(str::trim)
        .unwrap_or_else(|| raw.trim());
    let separator = if raw.contains(',') { ',' } else { '|' };
    let values = raw
        .split(separator)
        .map(|value| value.trim().trim_matches(['\'', '"']))
        .filter(|value| {
            !value.is_empty()
                && value
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        })
        .map(str::to_string)
        .collect::<Vec<_>>();
    if values.len() > 1 {
        values
    } else {
        Vec::new()
    }
}

pub(crate) fn claude_efforts_from_help(help: &str) -> Vec<String> {
    let discovered = option_block(help, "--effort")
        .and_then(|block| {
            if let Some(open) = block.find('<') {
                if let Some(close) = block[open + 1..].find('>').map(|close| close + open + 1) {
                    let choices = parse_choice_list(&block[open + 1..close]);
                    if !choices.is_empty() {
                        return Some(choices);
                    }
                }
            }
            let open = block.find('(')?;
            let close = block[open + 1..].find(')')? + open + 1;
            Some(parse_choice_list(&block[open + 1..close]))
        })
        .filter(|values| !values.is_empty())
        .unwrap_or_else(|| ["low", "medium", "high"].map(str::to_string).to_vec());
    let mut efforts = vec![AUTO_EFFORT.to_string()];
    for effort in discovered {
        if effort != AUTO_EFFORT && !efforts.contains(&effort) {
            efforts.push(effort);
        }
    }
    efforts
}

fn context_flag_from_help(help: &str) -> Option<String> {
    let lines = help.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('-') {
            continue;
        }
        let Some(raw_flag) = trimmed
            .split_whitespace()
            .find(|word| word.starts_with("--") && word.to_ascii_lowercase().contains("context"))
        else {
            continue;
        };
        let flag = raw_flag.trim_end_matches(',');
        let flag = flag.split(['=', '<']).next().unwrap_or(flag);
        if flag == "--autocompact" {
            continue;
        }
        let mut block = trimmed.to_ascii_lowercase();
        for continuation in lines.iter().skip(index + 1) {
            let continuation = continuation.trim_start();
            if continuation.starts_with('-') {
                break;
            }
            block.push(' ');
            block.push_str(&continuation.to_ascii_lowercase());
        }
        if block.contains("1m") {
            return Some(flag.to_string());
        }
    }
    None
}

pub(crate) fn claude_context_form_from_help(help: &str) -> ClaudeContextWindowForm {
    if let Some(flag) = context_flag_from_help(help) {
        ClaudeContextWindowForm::Flag(flag)
    } else {
        ClaudeContextWindowForm::ModelAlias
    }
}

fn default_codex_catalog() -> HomeProviderCatalog {
    HomeProviderCatalog {
        agent: Agent::Codex,
        models: vec![entry(DEFAULT_MODEL, &[AUTO_EFFORT])],
        claude_context_form: None,
    }
}

#[derive(Debug, Deserialize)]
struct RawCatalog {
    models: Vec<RawModel>,
}

#[derive(Debug, Deserialize)]
struct RawModel {
    slug: String,
    visibility: String,
    priority: Option<u64>,
    #[serde(default)]
    supported_reasoning_levels: Vec<RawEffort>,
}

#[derive(Debug, Deserialize)]
struct RawEffort {
    effort: String,
}

pub(crate) fn parse_codex_catalog(bytes: &[u8]) -> Option<HomeProviderCatalog> {
    let raw: RawCatalog = serde_json::from_slice(bytes).ok()?;
    let mut visible: Vec<_> = raw
        .models
        .into_iter()
        .enumerate()
        .filter(|(_, model)| model.visibility == "list" && !model.slug.trim().is_empty())
        .collect();
    visible
        .sort_by_key(|(source_order, model)| (model.priority.unwrap_or(u64::MAX), *source_order));

    let mut catalog = default_codex_catalog();
    for (_, model) in visible {
        if catalog.models.iter().any(|entry| entry.id == model.slug) {
            continue;
        }
        let mut efforts = vec![AUTO_EFFORT.to_string()];
        for effort in model.supported_reasoning_levels {
            let effort = effort.effort.trim();
            if !effort.is_empty() && !efforts.iter().any(|known| known == effort) {
                efforts.push(effort.into());
            }
        }
        catalog.models.push(HomeModelCatalogEntry {
            id: model.slug.clone(),
            display_name: model.slug,
            efforts,
            supports_large_context: false,
        });
    }
    (catalog.models.len() > 1).then_some(catalog)
}

pub(crate) fn codex_catalog_paths() -> Option<(PathBuf, PathBuf)> {
    let root = crate::integration::codex_dir().ok()?;
    Some((root.join("models_cache.json"), root.join("config.toml")))
}

pub(crate) fn load_codex_catalog_cache(path: &Path) -> Option<HomeProviderCatalog> {
    let bytes = std::fs::read(path).ok()?;
    (bytes.len() <= CATALOG_OUTPUT_LIMIT)
        .then(|| parse_codex_catalog(&bytes))
        .flatten()
}

#[derive(Default)]
struct CodexCatalogConfig {
    model: Option<String>,
    model_catalog_json: Option<PathBuf>,
}

fn codex_catalog_config(config_path: &Path) -> CodexCatalogConfig {
    let Some(contents) = std::fs::read_to_string(config_path).ok() else {
        return CodexCatalogConfig::default();
    };
    let Some(value) = toml::from_str::<toml::Value>(&contents).ok() else {
        return CodexCatalogConfig::default();
    };
    let model = value
        .get("model")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned);
    let model_catalog_json = value
        .get("model_catalog_json")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .and_then(|path| crate::integration::expand_tilde_path(path).ok())
        .map(|catalog_path| {
            if catalog_path.is_absolute() {
                catalog_path
            } else {
                config_path
                    .parent()
                    .unwrap_or_else(|| Path::new(""))
                    .join(catalog_path)
            }
        });
    CodexCatalogConfig {
        model,
        model_catalog_json,
    }
}

pub(crate) fn cached_home_catalog(cache_path: &Path, config_path: &Path) -> HomeCatalog {
    let config = codex_catalog_config(config_path);
    let configured_catalog = config
        .model_catalog_json
        .as_deref()
        .and_then(load_codex_catalog_cache);
    let codex = configured_catalog
        .or_else(|| load_codex_catalog_cache(cache_path))
        .unwrap_or_else(|| {
            let mut catalog = default_codex_catalog();
            if let Some(model) = config.model {
                if model != DEFAULT_MODEL {
                    catalog.models.push(HomeModelCatalogEntry {
                        display_name: model.clone(),
                        id: model,
                        efforts: vec![AUTO_EFFORT.into()],
                        supports_large_context: false,
                    });
                }
            }
            catalog
        });
    let claude_cache_path = cache_path.with_file_name(CLAUDE_HELP_CACHE_FILE);
    let claude =
        load_claude_catalog_cache(&claude_cache_path).unwrap_or_else(|| claude_catalog(None));
    HomeCatalog::with_claude_and_codex(claude, codex)
}

pub(crate) fn cached_home_catalog_for_current_profile() -> HomeCatalog {
    codex_catalog_paths()
        .map(|(cache, config)| cached_home_catalog(&cache, &config))
        .unwrap_or_default()
}

pub(crate) fn refresh_codex_catalog_with(
    program: impl AsRef<OsStr>,
    timeout: Duration,
) -> Option<HomeProviderCatalog> {
    let mut command = crate::noninteractive_process::command(program);
    command
        .args(["debug", "models"])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let output = crate::noninteractive_process::output_with_deadline_limited(
        command,
        Instant::now() + timeout,
        CATALOG_OUTPUT_LIMIT,
    )
    .ok()?;
    output
        .status
        .success()
        .then(|| parse_codex_catalog(&output.stdout))
        .flatten()
}

pub(crate) fn refresh_codex_catalog() -> Option<HomeProviderCatalog> {
    refresh_codex_catalog_with("codex", CATALOG_TIMEOUT)
}

pub(crate) fn load_claude_catalog_cache(path: &Path) -> Option<HomeProviderCatalog> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > CATALOG_OUTPUT_LIMIT {
        return None;
    }
    let help = std::str::from_utf8(&bytes).ok()?;
    Some(parse_claude_help(help))
}

pub(crate) fn refresh_claude_catalog_with(
    program: impl AsRef<OsStr>,
    cache_path: &Path,
    timeout: Duration,
) -> Option<HomeProviderCatalog> {
    let mut command = crate::noninteractive_process::command(program);
    command
        .arg("--help")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let output = crate::noninteractive_process::output_with_deadline_limited(
        command,
        Instant::now() + timeout,
        CATALOG_OUTPUT_LIMIT,
    )
    .ok()?;
    if !output.status.success() {
        return None;
    }
    let help = String::from_utf8(output.stdout).ok()?;
    let cache_result = cache_path
        .parent()
        .map(std::fs::create_dir_all)
        .transpose()
        .and_then(|_| std::fs::write(cache_path, help.as_bytes()));
    if let Err(error) = cache_result {
        tracing::debug!(path = %cache_path.display(), %error, "failed to cache Claude help");
    }
    Some(parse_claude_help(&help))
}

pub(crate) fn refresh_claude_catalog() -> Option<HomeProviderCatalog> {
    let (codex_cache, _) = codex_catalog_paths()?;
    refresh_claude_catalog_with(
        "claude",
        &codex_cache.with_file_name(CLAUDE_HELP_CACHE_FILE),
        CATALOG_TIMEOUT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODELS: &str = r#"{
      "models": [
        {"slug":"hidden","visibility":"hide","priority":0,"supported_reasoning_levels":[{"effort":"ultra"}]},
        {"slug":"later","visibility":"list","priority":20,"supported_reasoning_levels":[{"effort":"low"}]},
        {"slug":"first","visibility":"list","priority":10,"supported_reasoning_levels":[{"effort":"low"},{"effort":"ultra"}]},
        {"slug":"duplicate","visibility":"list","priority":30,"supported_reasoning_levels":[]},
        {"slug":"duplicate","visibility":"list","priority":40,"supported_reasoning_levels":[{"effort":"high"}]}
      ]
    }"#;

    #[test]
    fn parser_keeps_visible_models_in_priority_order_with_model_efforts() {
        let catalog = parse_codex_catalog(MODELS.as_bytes()).expect("catalog");
        assert_eq!(
            catalog
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            [DEFAULT_MODEL, "first", "later", "duplicate"]
        );
        assert_eq!(
            catalog.model("first").expect("first model").efforts,
            [AUTO_EFFORT, "low", "ultra"]
        );
        assert_eq!(
            catalog.model("later").expect("later model").efforts,
            [AUTO_EFFORT, "low"]
        );
    }

    #[test]
    fn claude_catalog_has_exact_names_ids_efforts_and_context_support() {
        const HELP: &str = "  --effort <level>  Effort level\n                    (low, medium, high, xhigh, max)\n";
        let claude = claude_catalog(Some(HELP));

        assert_eq!(
            claude
                .models
                .iter()
                .map(|model| (model.display_name.as_str(), model.id.as_str()))
                .collect::<Vec<_>>(),
            [
                ("default", "default"),
                ("Claude Fable 5.1", "claude-fable-5-1"),
                ("Claude Opus 5", "claude-opus-5"),
                ("Claude Sonnet 5", "claude-sonnet-5"),
                ("Claude Haiku 4.5", "claude-haiku-4-5-20251001"),
            ]
        );
        assert!(claude
            .models
            .iter()
            .all(|model| model.efforts == [AUTO_EFFORT, "low", "medium", "high", "xhigh", "max"]));
        assert_eq!(
            claude
                .models
                .iter()
                .filter(|model| model.supports_large_context)
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["claude-fable-5-1", "claude-opus-5", "claude-sonnet-5"]
        );
    }

    #[test]
    fn claude_effort_discovery_uses_help_choices_and_bounded_fallback() {
        assert_eq!(
            claude_efforts_from_help(
                "  --effort <level>  Current effort\n                    (low, medium, high, xhigh, max)\n  --model <model>"
            ),
            [AUTO_EFFORT, "low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            claude_efforts_from_help("--effort <level> undocumented values"),
            [AUTO_EFFORT, "low", "medium", "high"]
        );
        assert_eq!(
            claude_efforts_from_help("no effort option"),
            [AUTO_EFFORT, "low", "medium", "high"]
        );
    }

    #[test]
    fn claude_context_discovery_uses_a_documented_flag_or_model_alias() {
        assert_eq!(
            claude_context_form_from_help("  --context-window <size>  Context window (200k, 1m)\n"),
            ClaudeContextWindowForm::Flag("--context-window".into())
        );
        assert_eq!(
            claude_context_form_from_help("  --model <model>  Append [1m] for 1M context\n"),
            ClaudeContextWindowForm::ModelAlias
        );
        assert_eq!(
            claude_context_form_from_help("  --model <model>  Model id\n"),
            ClaudeContextWindowForm::ModelAlias
        );
    }

    #[test]
    fn malformed_or_empty_catalog_is_rejected() {
        assert!(parse_codex_catalog(b"not json").is_none());
        assert!(parse_codex_catalog(br#"{"models":[]}"#).is_none());
        assert!(
            parse_codex_catalog(br#"{"models":[{"slug":"hidden","visibility":"hide"}]}"#).is_none()
        );
    }

    #[test]
    fn cache_then_config_then_default_fallback_always_keeps_default_usable() {
        let root = std::env::temp_dir().join(format!("herdr-home-catalog-{}", std::process::id()));
        let cache = root.join("models_cache.json");
        let config = root.join("config.toml");
        let configured_catalog = root.join("configured-models.json");
        std::fs::create_dir_all(&root).expect("create fixture directory");

        std::fs::write(&cache, MODELS).expect("write cache");
        std::fs::write(
            root.join(CLAUDE_HELP_CACHE_FILE),
            "--effort <level> (low, medium, high, xhigh, max)\n",
        )
        .expect("write Claude help cache");
        std::fs::write(
            &configured_catalog,
            MODELS.replace("first", "configured-first"),
        )
        .expect("write configured catalog");
        std::fs::write(
            &config,
            "model = 'configured-model'\nmodel_catalog_json = 'configured-models.json'\n",
        )
        .expect("write config");
        let from_cache = cached_home_catalog(&cache, &config);
        assert!(from_cache
            .provider(Agent::Codex)
            .expect("Codex")
            .model("configured-first")
            .is_some());
        assert!(from_cache
            .provider(Agent::Codex)
            .expect("Codex")
            .model("first")
            .is_none());
        assert_eq!(
            from_cache
                .provider(Agent::Claude)
                .expect("Claude")
                .model(DEFAULT_MODEL)
                .expect("default")
                .efforts,
            [AUTO_EFFORT, "low", "medium", "high", "xhigh", "max"]
        );

        std::fs::write(&configured_catalog, "malformed").expect("replace configured catalog");
        let from_default_cache = cached_home_catalog(&cache, &config);
        assert!(from_default_cache
            .provider(Agent::Codex)
            .expect("Codex")
            .model("first")
            .is_some());

        std::fs::write(&cache, "malformed").expect("replace cache");
        let from_config = cached_home_catalog(&cache, &config);
        assert!(from_config
            .provider(Agent::Codex)
            .expect("Codex")
            .model("configured-model")
            .is_some());

        std::fs::write(&config, "malformed = [").expect("replace config");
        let fallback = cached_home_catalog(&cache, &config);
        assert_eq!(
            fallback.provider(Agent::Codex).expect("Codex").models,
            [entry(DEFAULT_MODEL, &[AUTO_EFFORT])]
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn expired_refresh_deadline_returns_without_replacing_the_fallback() {
        let refreshed = refresh_codex_catalog_with("codex", Duration::ZERO);
        assert!(refreshed.is_none());
        assert!(HomeCatalog::fallback()
            .provider(Agent::Codex)
            .expect("Codex fallback")
            .model(DEFAULT_MODEL)
            .is_some());
    }

    #[test]
    fn expired_claude_refresh_deadline_does_not_replace_the_cache() {
        let root = std::env::temp_dir().join(format!(
            "herdr-home-claude-catalog-timeout-{}",
            std::process::id()
        ));
        let cache = root.join(CLAUDE_HELP_CACHE_FILE);
        std::fs::create_dir_all(&root).expect("create fixture directory");
        std::fs::write(&cache, "cached help").expect("write fixture cache");

        let refreshed = refresh_claude_catalog_with("claude", &cache, Duration::ZERO);

        assert!(refreshed.is_none());
        assert_eq!(
            std::fs::read_to_string(&cache).expect("read fixture cache"),
            "cached help"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn claude_refresh_runs_help_once_and_caches_its_output() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "herdr-home-claude-catalog-refresh-{}",
            std::process::id()
        ));
        let program = root.join("fixture-claude");
        let count = root.join("calls");
        let cache = root.join(CLAUDE_HELP_CACHE_FILE);
        std::fs::create_dir_all(&root).expect("create fixture directory");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\n[ \"$1\" = \"--help\" ] || exit 2\nprintf x >> '{}'\nprintf '%s\\n' '--effort <level> (low, medium, high, xhigh)'\n",
                count.display()
            ),
        )
        .expect("write fixture command");
        let mut permissions = std::fs::metadata(&program)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&program, permissions).expect("make fixture executable");

        let catalog = refresh_claude_catalog_with(&program, &cache, Duration::from_secs(1))
            .expect("refresh catalog");

        assert_eq!(std::fs::read_to_string(&count).expect("call count"), "x");
        assert_eq!(
            catalog.model(DEFAULT_MODEL).expect("default").efforts,
            [AUTO_EFFORT, "low", "medium", "high", "xhigh"]
        );
        assert_eq!(
            std::fs::read_to_string(&cache).expect("cached help"),
            "--effort <level> (low, medium, high, xhigh)\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn refresh_kills_a_catalog_command_that_exceeds_its_deadline() {
        use std::os::unix::fs::PermissionsExt;

        let root =
            std::env::temp_dir().join(format!("herdr-home-catalog-timeout-{}", std::process::id()));
        let program = root.join("slow-codex");
        std::fs::create_dir_all(&root).expect("create fixture directory");
        std::fs::write(&program, "#!/bin/sh\nsleep 5\n").expect("write fixture command");
        let mut permissions = std::fs::metadata(&program)
            .expect("fixture metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&program, permissions).expect("make fixture executable");

        let started = Instant::now();
        let refreshed = refresh_codex_catalog_with(&program, Duration::from_millis(25));

        assert!(refreshed.is_none());
        assert!(started.elapsed() < Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(root);
    }
}
