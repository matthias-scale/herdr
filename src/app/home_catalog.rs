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
const CODEX_CATALOG_TIMEOUT: Duration = Duration::from_millis(750);
const CODEX_CATALOG_OUTPUT_LIMIT: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeModelCatalogEntry {
    pub(crate) id: String,
    pub(crate) efforts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HomeProviderCatalog {
    pub(crate) agent: Agent,
    pub(crate) models: Vec<HomeModelCatalogEntry>,
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
            providers: vec![claude_catalog(), default_codex_catalog()],
        }
    }

    pub(crate) fn with_codex(codex: HomeProviderCatalog) -> Self {
        debug_assert_eq!(codex.agent, Agent::Codex);
        Self {
            providers: vec![claude_catalog(), codex],
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
        efforts: efforts.iter().map(|effort| (*effort).into()).collect(),
    }
}

fn claude_catalog() -> HomeProviderCatalog {
    const AUTO_ONLY: &[&str] = &[AUTO_EFFORT];
    const ADAPTIVE_EFFORTS: &[&str] = &[AUTO_EFFORT, "low", "medium", "high", "xhigh", "max"];
    HomeProviderCatalog {
        agent: Agent::Claude,
        models: vec![
            entry(DEFAULT_MODEL, AUTO_ONLY),
            entry("fable", ADAPTIVE_EFFORTS),
            entry("opus", ADAPTIVE_EFFORTS),
            entry("sonnet", ADAPTIVE_EFFORTS),
            entry("haiku", AUTO_ONLY),
        ],
    }
}

fn default_codex_catalog() -> HomeProviderCatalog {
    HomeProviderCatalog {
        agent: Agent::Codex,
        models: vec![entry(DEFAULT_MODEL, &[AUTO_EFFORT])],
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
            id: model.slug,
            efforts,
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
    (bytes.len() <= CODEX_CATALOG_OUTPUT_LIMIT)
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
                        id: model,
                        efforts: vec![AUTO_EFFORT.into()],
                    });
                }
            }
            catalog
        });
    HomeCatalog::with_codex(codex)
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
        CODEX_CATALOG_OUTPUT_LIMIT,
    )
    .ok()?;
    output
        .status
        .success()
        .then(|| parse_codex_catalog(&output.stdout))
        .flatten()
}

pub(crate) fn refresh_codex_catalog() -> Option<HomeProviderCatalog> {
    refresh_codex_catalog_with("codex", CODEX_CATALOG_TIMEOUT)
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
    fn claude_effort_options_follow_the_selected_model() {
        let catalog = HomeCatalog::fallback();
        let claude = catalog.provider(Agent::Claude).expect("Claude catalog");

        assert_eq!(
            claude.model(DEFAULT_MODEL).expect("Default").efforts,
            [AUTO_EFFORT]
        );
        assert_eq!(claude.model("haiku").expect("Haiku").efforts, [AUTO_EFFORT]);
        assert_eq!(
            claude.model("fable").expect("Fable").efforts,
            [AUTO_EFFORT, "low", "medium", "high", "xhigh", "max"]
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
