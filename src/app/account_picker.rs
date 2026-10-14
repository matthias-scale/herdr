//! Client-local provider/account/model selection. Launch identity is independent of labels.
use std::path::PathBuf;

use super::{
    home::{HomePicker, HomeState},
    launch_profiles::LaunchProfile,
};
use crate::{
    detect::Agent,
    provider_usage::{AccountUsage, ProviderUsageSnapshot, QuotaProvider, UsageUnavailable},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct AccountSelection {
    pub(crate) profile: String,
    pub(crate) provider: QuotaProvider,
    pub(crate) account: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum PickerFocus {
    Accounts,
    #[default]
    Models,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountRow {
    pub(crate) profile: String,
    pub(crate) label: String,
    pub(crate) selection: Option<AccountSelection>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AccountPickerState {
    pub(crate) active: bool,
    pub(crate) focus: PickerFocus,
    pub(crate) account_cursor: usize,
    pub(crate) model_cursor: usize,
    pub(crate) query: String,
    pub(crate) rows: Vec<AccountRow>,
    pub(crate) bindings: Vec<(QuotaProvider, String, PathBuf)>,
    pub(crate) claude_bindings: Vec<crate::claude_account_launch::ClaudeTokenBinding>,
    pub(crate) claude_helper_available: bool,
    pub(crate) ambient_overrides: Vec<QuotaProvider>,
    pub(crate) ambient_accounts: Vec<(QuotaProvider, Option<String>)>,
    pub(crate) catalogs: Vec<(
        QuotaProvider,
        String,
        super::home_catalog::HomeProviderCatalog,
    )>,
    pub(crate) inherited_catalog: Option<super::home_catalog::HomeCatalog>,
}

impl HomeState {
    /// Record only presence, never values, of ambient authentication overrides.
    pub(crate) fn prepare_account_environment(&mut self) {
        self.account_picker.ambient_overrides.clear();
        self.account_picker.claude_bindings.clear();
        self.account_picker.claude_helper_available = false;
        if let Some((root, helper)) = crate::claude_account_launch::local_paths() {
            self.account_picker.claude_bindings =
                crate::claude_account_launch::discover_bindings(&root);
            self.account_picker.claude_helper_available = helper.is_file();
        }
        self.account_picker.ambient_accounts = [QuotaProvider::Claude, QuotaProvider::Codex]
            .into_iter()
            .map(|provider| {
                (
                    provider,
                    crate::provider_usage::inherited_account_id(
                        provider,
                        &self.account_picker.bindings,
                    ),
                )
            })
            .collect();
        for (provider, names) in [
            (
                QuotaProvider::Claude,
                crate::claude_account_launch::AUTH_OVERRIDES,
            ),
            (
                QuotaProvider::Codex,
                &["OPENAI_API_KEY", "CODEX_API_KEY", "OPENAI_BASE_URL"][..],
            ),
        ] {
            if names.iter().any(|name| std::env::var_os(name).is_some()) {
                self.account_picker.ambient_overrides.push(provider);
            }
        }
    }

    /// Read local, non-authentication catalog metadata only at composer creation.
    pub(crate) fn prepare_account_catalogs(&mut self) {
        self.account_picker.catalogs = self
            .account_picker
            .bindings
            .iter()
            .filter(|(provider, _, _)| *provider == QuotaProvider::Codex)
            .filter_map(|(provider, id, path)| {
                let catalog = super::home_catalog::cached_home_catalog(
                    &path.join("models_cache.json"),
                    &path.join("config.toml"),
                );
                catalog
                    .provider(Agent::Codex)
                    .cloned()
                    .map(|catalog| (*provider, id.clone(), catalog))
            })
            .collect();
    }

    pub(crate) fn refresh_account_rows(&mut self, snapshot: &ProviderUsageSnapshot) {
        let mut rows = Vec::new();
        for profile in self.profiles() {
            rows.push(AccountRow {
                profile: profile.id.clone(),
                label: if profile
                    .id
                    .eq_ignore_ascii_case(crate::detect::agent_label(profile.agent))
                    && profile
                        .label
                        .eq_ignore_ascii_case(crate::detect::agent_label(profile.agent))
                {
                    "inherited".into()
                } else {
                    format!("{} / inherited", profile.label)
                },
                selection: None,
            });
            if let Some(quota) = profile.quota {
                for account in snapshot
                    .accounts
                    .iter()
                    .filter(|account| account.provider == quota.provider())
                {
                    rows.push(AccountRow {
                        profile: profile.id.clone(),
                        label: {
                            let identity = if snapshot
                                .accounts
                                .iter()
                                .filter(|other| {
                                    other.provider == account.provider
                                        && other.label == account.label
                                })
                                .count()
                                > 1
                            {
                                format!("{} ({})", account.label, account.profile_id)
                            } else {
                                account.label.clone()
                            };
                            if profile
                                .id
                                .eq_ignore_ascii_case(crate::detect::agent_label(profile.agent))
                                && profile
                                    .label
                                    .eq_ignore_ascii_case(crate::detect::agent_label(profile.agent))
                            {
                                identity
                            } else {
                                format!("{} / {identity}", profile.label)
                            }
                        },
                        selection: Some(AccountSelection {
                            profile: profile.id.clone(),
                            provider: account.provider,
                            account: account.profile_id.clone(),
                        }),
                    });
                }
            }
        }
        self.account_picker.rows = rows;
        self.account_picker.account_cursor = self
            .account_picker
            .rows
            .iter()
            .position(|row| {
                row.profile == self.profile_id() && row.selection == self.account_selection
            })
            .unwrap_or(0);
    }

    pub(crate) fn open_account_picker(&mut self, picker: HomePicker) {
        self.picker = Some(picker);
        self.account_picker.active = true;
        self.account_picker.focus = if picker == HomePicker::Agent {
            PickerFocus::Accounts
        } else {
            PickerFocus::Models
        };
        self.account_picker.query.clear();
        self.account_picker.model_cursor = self
            .model_options()
            .iter()
            .position(|model| model.id == self.model)
            .unwrap_or(0);
        self.dispatch_error = None;
    }

    pub(crate) fn close_account_picker(&mut self) {
        self.account_picker.active = false;
        self.picker = None;
    }

    pub(crate) fn filtered_account_models(&self) -> Vec<usize> {
        if self.profile().owns_its_flags() {
            return Vec::new();
        }
        self.model_options()
            .iter()
            .enumerate()
            .filter_map(|(index, model)| {
                super::command_palette::fuzzy_score(
                    &format!("{} {}", model.id, model.display_name),
                    &self.account_picker.query,
                )
                .map(|_| index)
            })
            .collect()
    }

    pub(crate) fn select_account_row(&mut self, index: usize) {
        let Some(row) = self.account_picker.rows.get(index).cloned() else {
            return;
        };
        self.set_profile(&row.profile);
        self.restore_account_selection(row.selection);
        self.account_picker.account_cursor = index;
        self.account_picker.model_cursor = 0;
        self.dispatch_error = self.account_launch_env().err();
    }

    pub(crate) fn restore_account_selection(&mut self, selection: Option<AccountSelection>) {
        self.account_selection = selection;
        if let Some(selection) = &self.account_selection {
            if let Some((_, _, catalog)) =
                self.account_picker
                    .catalogs
                    .iter()
                    .find(|(provider, id, _)| {
                        *provider == selection.provider && *id == selection.account
                    })
            {
                self.replace_provider_catalog(catalog.clone());
            }
        }
        self.dispatch_error = self.account_launch_env().err();
    }

    pub(crate) fn select_account_model(&mut self, index: usize) -> bool {
        let Some(model_index) = self.filtered_account_models().get(index).copied() else {
            return false;
        };
        let id = self.model_options()[model_index].id.clone();
        self.set_model(&id);
        self.account_picker.model_cursor = index;
        self.close_account_picker();
        true
    }

    pub(crate) fn account_picker_key(&mut self, event: KeyEvent) {
        let plain = !event
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER);
        match event.code {
            KeyCode::Esc => self.close_account_picker(),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                self.account_picker.focus = match self.account_picker.focus {
                    PickerFocus::Accounts => PickerFocus::Models,
                    PickerFocus::Models => PickerFocus::Accounts,
                };
            }
            KeyCode::Up | KeyCode::Down if plain => {
                let delta: isize = if event.code == KeyCode::Up { -1 } else { 1 };
                match self.account_picker.focus {
                    PickerFocus::Accounts => {
                        let len = self.account_picker.rows.len();
                        let next = self
                            .account_picker
                            .account_cursor
                            .saturating_add_signed(delta)
                            .min(len.saturating_sub(1));
                        self.select_account_row(next);
                    }
                    PickerFocus::Models => {
                        let len = self.filtered_account_models().len();
                        self.account_picker.model_cursor = self
                            .account_picker
                            .model_cursor
                            .saturating_add_signed(delta)
                            .min(len.saturating_sub(1));
                    }
                }
            }
            KeyCode::Enter if plain => match self.account_picker.focus {
                PickerFocus::Accounts => {
                    self.select_account_row(self.account_picker.account_cursor);
                    self.account_picker.focus = PickerFocus::Models;
                    if self.profile().owns_its_flags() && self.account_selection.is_none() {
                        self.close_account_picker();
                    }
                }
                PickerFocus::Models => {
                    self.select_account_model(self.account_picker.model_cursor);
                }
            },
            KeyCode::Backspace if plain => {
                self.account_picker.query.pop();
                self.account_picker.model_cursor = 0;
            }
            KeyCode::Char(ch) if plain && !ch.is_control() => {
                self.account_picker.query.push(ch);
                self.account_picker.model_cursor = 0;
                self.account_picker.focus = PickerFocus::Models;
            }
            _ => {}
        }
    }

    pub(crate) fn account_launch_env(&self) -> Result<Vec<(String, String)>, String> {
        let profile = self.profile();
        let Some(selection) = &self.account_selection else {
            return Ok(profile.env.clone());
        };
        if selection.account == "default" {
            return Err(
                "account unavailable: default cache has no discovered profile binding".into(),
            );
        }
        if self
            .account_picker
            .ambient_overrides
            .contains(&selection.provider)
        {
            return Err("account unavailable: ambient authentication/provider override".into());
        }
        if self.machine().is_some_and(|machine| !machine.is_local()) {
            return Err("account unavailable: no verified binding on remote host".into());
        }
        if self.agent != profile.agent {
            return Err("account unavailable: provider changed".into());
        }
        if selection.profile != profile.id {
            return Err("account unavailable: launch profile changed".into());
        }
        if profile.owns_its_flags() {
            return Err("account unavailable: wrapper command owns its environment".into());
        }
        if selection.provider == QuotaProvider::Claude {
            self.claude_subscription_binding(selection, profile)?;
            return Ok(profile.env.clone());
        }
        let key = binding_key(profile, selection.provider)?;
        let path = self
            .account_picker
            .bindings
            .iter()
            .find(|(provider, id, path)| {
                *provider == selection.provider && *id == selection.account && path.is_absolute()
            })
            .map(|(_, _, path)| path)
            .ok_or_else(|| "account unavailable: no discovered profile binding".to_string())?;
        let mut env = profile.env.clone();
        env.retain(|(name, _)| name != key);
        env.push((key.into(), path.to_string_lossy().into_owned()));
        Ok(env)
    }

    fn claude_subscription_binding<'a>(
        &'a self,
        selection: &AccountSelection,
        profile: &LaunchProfile,
    ) -> Result<&'a crate::claude_account_launch::ClaudeTokenBinding, String> {
        if !crate::platform::claude_account_launch_supported() {
            return Err(
                "Claude unavailable: subscription-token launcher unsupported on this platform"
                    .into(),
            );
        }
        binding_key(profile, selection.provider)?;
        if profile.env.iter().any(|(name, _)| {
            // Configured roots/tool lookup or secret-get controls could redirect
            // the child away from the independently verified local secret owner.
            matches!(name.as_str(), "HOME" | "USERPROFILE" | "PATH")
                || crate::claude_account_launch::HELPER_CONTROL_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
                || crate::claude_account_launch::AUTH_OVERRIDES.contains(&name.as_str())
        }) {
            return Err(
                "Claude unavailable: configured authentication/provider/config override".into(),
            );
        }
        if !self.account_picker.claude_helper_available {
            return Err("Claude unavailable: canonical secret-get helper missing".into());
        }
        let key = crate::claude_account_launch::token_key(&selection.account)?;
        self.account_picker.claude_bindings.iter().find(|binding| {
            binding.account == selection.account && binding.secret_name == key
        }).ok_or_else(|| "Claude unavailable: no unique verified subscription-token mapping (metadata missing or key collision)".into())
    }

    /// Compose only non-secret identity; the child resolves auth after IPC/persistence.
    pub(crate) fn account_launch_argv(&self, argv: Vec<String>) -> Result<Vec<String>, String> {
        let Some(selection) = self
            .account_selection
            .as_ref()
            .filter(|s| s.provider == QuotaProvider::Claude)
        else {
            return Ok(argv);
        };
        self.account_launch_env()?;
        let binding = self.claude_subscription_binding(selection, self.profile())?;
        crate::claude_account_launch::compose_argv(binding, argv)
    }

    pub(crate) fn selected_account_usage<'a>(
        &self,
        snapshot: &'a ProviderUsageSnapshot,
    ) -> Option<&'a AccountUsage> {
        let profile = self.profile();
        if let Some(selection) = &self.account_selection {
            return snapshot
                .accounts
                .iter()
                .find(|account| {
                    account.provider == selection.provider
                        && account.profile_id == selection.account
                })
                .map(|account| &account.usage);
        }
        let provider = profile.quota?.provider();
        if self.account_picker.ambient_overrides.contains(&provider) {
            return None;
        }
        let key = binding_key(profile, provider).ok()?;
        if let Some((_, value)) = profile.env.iter().find(|(name, _)| name == key) {
            let id = self
                .account_picker
                .bindings
                .iter()
                .find(|(p, _, path)| *p == provider && path == &PathBuf::from(value))
                .map(|(_, id, _)| id)?;
            return snapshot
                .accounts
                .iter()
                .find(|account| account.provider == provider && &account.profile_id == id)
                .map(|account| &account.usage);
        }
        if let Some((_, id)) = self
            .account_picker
            .ambient_accounts
            .iter()
            .find(|(p, _)| *p == provider)
        {
            return id
                .as_ref()
                .and_then(|id| {
                    snapshot
                        .accounts
                        .iter()
                        .find(|account| account.provider == provider && &account.profile_id == id)
                })
                .map(|account| &account.usage);
        }
        Some(profile.quota?.usage(snapshot))
    }

    pub(crate) fn account_row_unavailable(&self, row: &AccountRow) -> bool {
        let Some(selection) = &row.selection else {
            return false;
        };
        if selection.account == "default" {
            return true;
        }
        let Some(profile) = self
            .profiles()
            .iter()
            .find(|profile| profile.id == row.profile)
        else {
            return true;
        };
        self.machine().is_some_and(|machine| !machine.is_local())
            || self
                .account_picker
                .ambient_overrides
                .contains(&selection.provider)
            || profile.owns_its_flags()
            || binding_key(profile, selection.provider).is_err()
            || (selection.provider == QuotaProvider::Claude
                && self
                    .claude_subscription_binding(selection, profile)
                    .is_err())
            || (selection.provider != QuotaProvider::Claude
                && !self
                    .account_picker
                    .bindings
                    .iter()
                    .any(|(provider, id, path)| {
                        *provider == selection.provider
                            && *id == selection.account
                            && path.is_absolute()
                    }))
    }

    pub(crate) fn account_label(&self) -> String {
        if self.account_selection.is_none() {
            return self.profile().label.clone();
        }
        self.account_picker
            .rows
            .iter()
            .find(|row| row.profile == self.profile_id() && row.selection == self.account_selection)
            .map(|row| row.label.clone())
            .unwrap_or_else(|| {
                self.account_selection
                    .as_ref()
                    .map(|selection| format!("{} / {}", selection.profile, selection.account))
                    .unwrap_or_else(|| self.profile().label.clone())
            })
    }
}

fn binding_key(profile: &LaunchProfile, provider: QuotaProvider) -> Result<&'static str, String> {
    if profile.env.iter().any(|(key, _)| {
        matches!(
            key.as_str(),
            "ANTHROPIC_BASE_URL"
                | "OPENAI_BASE_URL"
                | "ANTHROPIC_AUTH_TOKEN"
                | "ANTHROPIC_API_KEY"
                | "OPENAI_API_KEY"
                | "CODEX_API_KEY"
                | "CLAUDE_CODE_USE_BEDROCK"
                | "CLAUDE_CODE_USE_VERTEX"
                | "CLAUDE_CODE_USE_FOUNDRY"
        )
    }) {
        return Err(
            "account unavailable: configured provider environment overrides native authentication"
                .into(),
        );
    }
    match (profile.agent, provider) {
        (Agent::Claude, QuotaProvider::Claude) => Ok("CLAUDE_CONFIG_DIR"),
        (Agent::Codex, QuotaProvider::Codex) => Ok("CODEX_HOME"),
        _ => Err("account unavailable: provider has no supported per-process binding".into()),
    }
}

/// Truthful text, shared by both composers and the picker. Unknown never becomes zero.
pub(crate) fn quota_lines(usage: Option<&AccountUsage>, now: Option<i64>) -> Vec<String> {
    let Some(usage) = usage else {
        return vec!["usage unknown".into()];
    };
    let mut lines = Vec::new();
    if let Some(reason) = usage.unavailable {
        lines.push(
            match reason {
                UsageUnavailable::AuthExpired => "login required",
                UsageUnavailable::HelperMissing => "helper missing",
                UsageUnavailable::HelperTimedOut => "helper timed out",
                UsageUnavailable::HelperFailed => "helper failed",
                UsageUnavailable::HelperUnreadable => "helper unreadable",
                _ => "usage unknown",
            }
            .into(),
        );
    }
    for (name, window) in [("5h", usage.five_hour), ("week", usage.seven_day)] {
        if let Some(window) = window {
            let remaining = 100u8.saturating_sub(window.used_percent);
            let state = if remaining == 0 {
                "exhausted"
            } else if remaining <= 10 {
                "low"
            } else {
                ""
            };
            let reset = window
                .resets_at
                .map(|at| {
                    now.and_then(|now| crate::provider_usage::reset_label(at, now))
                        .unwrap_or_else(|| format!("reset {at}"))
                })
                .unwrap_or_else(|| "reset unknown".into());
            lines.push(format!("{name} {remaining}% left {state} · {reset}"));
        }
    }
    if usage.is_empty() && lines.is_empty() {
        lines.push("usage unknown".into());
    }
    lines.push(if usage.stale {
        "stale · last-known".into()
    } else {
        usage
            .last_refresh_unix
            .zip(now)
            .map(|(at, now)| format!("updated {}s ago", now.saturating_sub(at).max(0)))
            .unwrap_or_else(|| "freshness unknown".into())
    });
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_usage::{ProviderAccountUsage, QuotaWindow};
    fn fixture() -> HomeState {
        let mut home = HomeState::default();
        home.set_profiles(vec![LaunchProfile::builtin(Agent::Codex)]);
        home.set_profile("codex");
        home.prompt = "test".into();
        home.account_picker.bindings = vec![(
            QuotaProvider::Codex,
            "stable-id".into(),
            PathBuf::from("/fixture/profile"),
        )];
        let mut snapshot = ProviderUsageSnapshot::default();
        snapshot.accounts.push(ProviderAccountUsage {
            provider: QuotaProvider::Codex,
            profile_id: "stable-id".into(),
            label: "a display label that is not a directory".into(),
            usage: Default::default(),
        });
        home.refresh_account_rows(&snapshot);
        home
    }
    #[cfg(unix)]
    fn claude_token_fixture() -> HomeState {
        let mut home = HomeState::default();
        home.set_profiles(vec![LaunchProfile::builtin(Agent::Claude)]);
        home.prompt = "native prompt".into();
        home.account_picker.claude_helper_available = true;
        home.account_picker.claude_bindings =
            vec![crate::claude_account_launch::ClaudeTokenBinding {
                account: "scalable-so".into(),
                secret_name: "CLAUDE_T3_TOKEN_SCALABLE_SO".into(),
            }];
        let mut snapshot = ProviderUsageSnapshot::default();
        snapshot.accounts.push(ProviderAccountUsage {
            provider: QuotaProvider::Claude,
            profile_id: "scalable-so".into(),
            label: "untrusted ; $(label)".into(),
            usage: Default::default(),
        });
        home.refresh_account_rows(&snapshot);
        home
    }

    #[test]
    fn account_picker_short_model_labels_preserve_full_id_and_prior_name_search() {
        let mut home = HomeState::default();
        home.open_account_picker(HomePicker::Model);
        for (id, previous, short) in [
            ("claude-fable-5-1", "Claude Fable 5.1", "Fable 5.1"),
            ("claude-opus-5-5", "Claude Opus 5.5", "Opus 5.5"),
            ("claude-sonnet-5-5", "Claude Sonnet 5.5", "Sonnet 5.5"),
            ("claude-haiku-5-5", "Claude Haiku 5.5", "Haiku 5.5"),
            ("claude-opus-5", "Claude Opus 5", "Opus 5"),
            ("claude-haiku-4-5-20251001", "Claude Haiku 4.5", "Haiku 4.5"),
        ] {
            for query in [id, previous, short] {
                home.account_picker.query = query.into();
                assert!(
                    home.filtered_account_models()
                        .iter()
                        .any(|index| home.model_options()[*index].id == id),
                    "query {query}"
                );
            }
            home.set_model(id);
            assert_eq!(home.model, id, "explicit ID is never silently upgraded");
            assert_eq!(home.model_display_name(), short);
        }
    }

    #[cfg(unix)]
    #[test]
    fn account_picker_claude_token_plan_and_spawn_draft_hold_only_non_secret_identity() {
        let mut home = claude_token_fixture();
        let native = home.dispatch_plan().expect("inherited native");
        assert_eq!(native.argv[0], "claude");
        assert!(!home.account_row_unavailable(&home.account_picker.rows[1]));
        home.select_account_row(1);
        let plan = home.dispatch_plan().expect("token adapter");
        assert_eq!(
            &plan.argv[1..5],
            &[
                crate::claude_account_launch::CHILD_COMMAND,
                "scalable-so",
                "CLAUDE_T3_TOKEN_SCALABLE_SO",
                "--"
            ]
        );
        assert_eq!(
            &plan.argv[5..],
            native.argv.as_slice(),
            "same native model/effort/prompt flags"
        );
        assert_eq!(plan.env, native.env);
        assert_eq!(plan.model, native.model);
        assert_eq!(plan.effort, native.effort);
        let request = serde_json::json!({"argv": plan.argv, "env": plan.env}).to_string();
        assert!(request.contains("CLAUDE_T3_TOKEN_SCALABLE_SO"));
        assert!(!request.contains("dummy-subscription-value"));
        assert!(!request.contains("CLAUDE_CODE_OAUTH_TOKEN"));
        assert!(!request.contains("untrusted ; $(label)"));
        let dock = super::super::spawn_dock::SpawnDockState::new(home.clone());
        let encoded = serde_json::to_string(&dock.draft()).expect("local draft");
        assert!(!encoded.contains("dummy-subscription-value"));
        assert!(!encoded.contains("CLAUDE_CODE_OAUTH_TOKEN"));
        let restored = super::super::spawn_dock::SpawnDockState::from_draft(
            home.clone(),
            serde_json::from_str(&encoded).expect("draft"),
        );
        assert_eq!(
            restored.home.dispatch_plan().expect("Spawn same contract"),
            plan
        );
        home.select_account_row(0);
        assert_eq!(home.dispatch_plan().expect("inherited unchanged"), native);
    }

    #[cfg(unix)]
    #[test]
    fn account_picker_claude_token_mapping_helper_override_remote_and_wrapper_fail_closed() {
        let mut selected = claude_token_fixture();
        selected.select_account_row(1);
        let mut home = selected.clone();
        home.account_picker.claude_bindings.clear();
        assert!(home
            .dispatch_plan()
            .expect_err("unverified")
            .contains("no unique verified"));
        let mut home = selected.clone();
        home.account_picker.claude_helper_available = false;
        assert!(home
            .dispatch_plan()
            .expect_err("missing helper")
            .contains("helper missing"));
        for name in crate::claude_account_launch::AUTH_OVERRIDES
            .iter()
            .copied()
            .chain([
                "HOME",
                "USERPROFILE",
                "PATH",
                "SECRET_GET_PERSONAL_PROJECTS",
                "SECRET_GET_PERSONAL_CONFIG",
                "SECRET_GET_PROFILE_TIMEOUT",
                "SECRET_GET_RETRIES",
                "SECRET_GET_RETRY_DELAY",
                "DOPPLER_TOKEN",
                "DOPPLER_TOKEN_PERSONAL",
                "DOPPLER_TOKEN_SCALABLE",
                "DOPPLER_TOKEN_SCALABLEV2",
                "DOPPLER_CONFIG_DIR",
            ])
        {
            let mut home = selected.clone();
            let mut profile = LaunchProfile::builtin(Agent::Claude);
            profile.env = vec![(name.to_string(), "dummy-conflict".into())];
            home.set_profiles(vec![profile]);
            assert!(home.dispatch_plan().is_err(), "override {name} must refuse");
            home.select_account_row(0);
            assert!(
                home.dispatch_plan().is_ok(),
                "inherited compatibility for {name}"
            );
        }
        let mut home = selected.clone();
        home.account_picker
            .ambient_overrides
            .push(QuotaProvider::Claude);
        assert!(home
            .dispatch_plan()
            .expect_err("ambient")
            .contains("ambient authentication"));
        let mut home = selected.clone();
        home.set_machines(vec![super::super::machines::Machine {
            name: "remote".into(),
            target: Some("fixture-host".into()),
            icon: None,
            socket: None,
        }]);
        home.set_machine("remote");
        assert!(home
            .dispatch_plan()
            .expect_err("remote")
            .contains("remote host"));
        let mut home = selected;
        let mut profile = LaunchProfile::builtin(Agent::Claude);
        profile.command = vec!["wrapper".into()];
        home.set_profiles(vec![profile]);
        assert!(home
            .dispatch_plan()
            .expect_err("wrapper")
            .contains("wrapper"));
    }

    #[test]
    fn account_picker_claude_refuses_capture_without_token_mapping_and_keeps_usage_and_inherited_launch(
    ) {
        let mut home = HomeState::default();
        let mut profile = LaunchProfile::builtin(Agent::Claude);
        profile.env = vec![("UNRELATED".into(), "keep".into())];
        home.set_profiles(vec![profile]);
        home.prompt = "task".into();
        home.account_picker.claude_helper_available = true;
        let inherited = home
            .dispatch_plan()
            .expect("inherited Claude remains dispatchable");
        let mut snapshot = ProviderUsageSnapshot::default();
        let usage = AccountUsage {
            five_hour: Some(QuotaWindow {
                used_percent: 37,
                resets_at: Some(200),
            }),
            stale: true,
            ..Default::default()
        };
        snapshot.accounts.push(ProviderAccountUsage {
            provider: QuotaProvider::Claude,
            profile_id: "capture-id".into(),
            label: "capture account".into(),
            usage: usage.clone(),
        });
        home.refresh_account_rows(&snapshot);
        // Even an old draft or injected native-looking binding cannot authorize Claude.
        home.account_picker.bindings = vec![(
            QuotaProvider::Claude,
            "capture-id".into(),
            "/fixture/native-looking".into(),
        )];
        assert!(home.account_row_unavailable(&home.account_picker.rows[1]));
        home.select_account_row(1);
        assert_eq!(home.selected_account_usage(&snapshot), Some(&usage));
        let reason = home
            .dispatch_plan()
            .expect_err("explicit Claude must never fallback");
        if crate::platform::claude_account_launch_supported() {
            assert!(reason.contains("no verified subscription-token mapping"));
        } else {
            assert!(reason.contains("unsupported on this platform"));
        }
        assert_eq!(home.dispatch_error.as_deref(), Some(reason.as_str()));
        let draft = super::super::spawn_dock::SpawnDockState::new(home.clone()).draft();
        let reopened = super::super::spawn_dock::SpawnDockState::from_draft(home.clone(), draft);
        assert_eq!(
            reopened
                .home
                .dispatch_plan()
                .expect_err("Spawn refuses too"),
            reason
        );
        home.select_account_row(0);
        assert_eq!(
            home.dispatch_plan()
                .expect("exact inherited environment restored"),
            inherited
        );
    }

    #[test]
    fn account_picker_config_refresh_preserves_identity_or_refuses_removed_profile() {
        let mut home = fixture();
        home.select_account_row(1);
        let selection = home.account_selection.clone();
        home.set_profiles(home.profiles().to_vec());
        assert_eq!(home.account_selection, selection);
        assert!(home.dispatch_plan().is_ok());
        home.set_profiles(vec![LaunchProfile::builtin(Agent::Claude)]);
        assert_eq!(home.account_selection, selection);
        assert!(home.dispatch_plan().is_err());
        assert!(home.account_label().contains("stable-id"));
    }

    #[test]
    fn account_picker_external_inherited_profile_never_borrows_default_quota() {
        let mut home = fixture();
        home.set_profiles(vec![LaunchProfile::builtin(Agent::Claude)]);
        home.set_profile("claude");
        home.account_picker.ambient_accounts = vec![(QuotaProvider::Claude, None)];
        let snapshot = ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage {
                five_hour: Some(QuotaWindow {
                    used_percent: 25,
                    resets_at: None,
                }),
                ..Default::default()
            },
            Default::default(),
            Default::default(),
        );
        assert!(home.selected_account_usage(&snapshot).is_none());
        assert!(
            home.dispatch_plan().is_ok(),
            "unknown quota does not alter inherited launch"
        );
    }

    #[test]
    fn account_picker_refuses_ambient_authentication_override_only_for_explicit_selection() {
        let mut home = fixture();
        home.account_picker
            .ambient_overrides
            .push(QuotaProvider::Codex);
        assert!(
            home.dispatch_plan().is_ok(),
            "inherited launch is unchanged"
        );
        home.select_account_row(1);
        assert!(home
            .dispatch_plan()
            .expect_err("wrong authentication must not win")
            .contains("ambient authentication"));
    }

    #[test]
    fn account_picker_switching_account_reconciles_real_catalog_and_restores_inherited_catalog() {
        let mut home = fixture();
        home.account_picker.inherited_catalog =
            Some(super::super::home_catalog::HomeCatalog::fallback());
        home.account_picker.catalogs.push((
            QuotaProvider::Codex,
            "stable-id".into(),
            super::super::home_catalog::HomeProviderCatalog {
                agent: Agent::Codex,
                models: vec![super::super::home_catalog::HomeModelCatalogEntry {
                    id: "profile-only-model".into(),
                    display_name: "profile-only-model".into(),
                    efforts: vec!["auto".into()],
                    supports_large_context: false,
                }],
                claude_context_form: None,
            },
        ));
        let inherited = home.model.clone();
        home.select_account_row(1);
        assert_eq!(home.model, "profile-only-model");
        assert_eq!(home.effort.as_deref(), Some("auto"));
        assert_eq!(home.context_window, None);
        home.select_account_row(0);
        assert_eq!(home.model, inherited);
    }

    #[test]
    fn account_picker_preserves_custom_launches_and_refuses_explicit_unsupported_provider() {
        let mut home = HomeState::default();
        let mut lane = LaunchProfile::builtin(Agent::Claude);
        lane.id = "custom".into();
        lane.label = "custom provider".into();
        lane.command = vec!["bash".into(), "-lc".into(), "wrapper".into()];
        lane.env = vec![("CUSTOM_ENV".into(), "preserved".into())];
        lane.quota = Some(super::super::launch_profiles::QuotaSource::Kimi);
        home.set_profiles(vec![lane]);
        home.set_profile("custom");
        home.prompt = "task".into();
        let before = home.dispatch_plan().expect("custom inherited launch");
        home.open_account_picker(HomePicker::Model);
        home.close_account_picker();
        assert_eq!(home.dispatch_plan().expect("unchanged"), before);
        home.account_selection = Some(AccountSelection {
            profile: "custom".into(),
            provider: QuotaProvider::Kimi,
            account: "known-label".into(),
        });
        assert!(home
            .dispatch_plan()
            .expect_err("no silent fallback")
            .contains("wrapper"));
    }

    #[test]
    fn account_picker_codex_override_preserves_other_lane_environment_and_model_flags() {
        let mut home = HomeState::default();
        let mut profile = LaunchProfile::builtin(Agent::Codex);
        profile.env = vec![
            ("CODEX_HOME".into(), "/old/profile".into()),
            ("UNRELATED".into(), "keep".into()),
        ];
        home.set_profiles(vec![profile]);
        home.set_profile("codex");
        home.prompt = "task".into();
        home.account_picker.bindings = vec![(
            QuotaProvider::Codex,
            "actual-id".into(),
            "/actual/profile".into(),
        )];
        home.account_selection = Some(AccountSelection {
            profile: "codex".into(),
            provider: QuotaProvider::Codex,
            account: "actual-id".into(),
        });
        let plan = home.dispatch_plan().expect("safe override");
        assert_eq!(
            plan.env,
            [
                ("UNRELATED".into(), "keep".into()),
                ("CODEX_HOME".into(), "/actual/profile".into())
            ]
        );
        assert_eq!(plan.model, home.model);
        assert_eq!(plan.effort, home.effort);
        home.account_selection.as_mut().expect("selection").account = "default".into();
        assert!(home
            .dispatch_plan()
            .expect_err("cache default is not a binding")
            .contains("no discovered"));
    }

    #[test]
    fn account_picker_binds_exact_metadata_identity_and_preserves_inherited_environment() {
        let mut home = fixture();
        let inherited = home.dispatch_plan().expect("inherited");
        assert!(inherited.env.is_empty());
        home.select_account_row(1);
        assert_eq!(
            home.dispatch_plan().expect("explicit").env,
            [("CODEX_HOME".into(), "/fixture/profile".into())]
        );
        home.select_account_row(0);
        assert_eq!(home.dispatch_plan().expect("inherited again"), inherited);
    }
    #[test]
    fn account_picker_refuses_missing_remote_and_wrapper_bindings() {
        let mut home = fixture();
        home.select_account_row(1);
        home.account_picker.bindings.clear();
        assert!(home
            .dispatch_plan()
            .expect_err("missing")
            .contains("no discovered"));
        home.account_picker.bindings = vec![(
            QuotaProvider::Codex,
            "stable-id".into(),
            "/fixture/profile".into(),
        )];
        home.set_machines(vec![super::super::machines::Machine {
            name: "remote".into(),
            target: Some("fixture-host".into()),
            icon: None,
            socket: None,
        }]);
        home.set_machine("remote");
        assert!(home
            .dispatch_plan()
            .expect_err("remote")
            .contains("remote host"));
        let mut profile = LaunchProfile::builtin(Agent::Claude);
        profile.command = vec!["custom-wrapper".into()];
        assert!(binding_key(&profile, QuotaProvider::Kimi).is_err());
    }
    #[test]
    fn account_picker_search_navigation_reconciles_model_and_effort() {
        let mut home = fixture();
        home.open_account_picker(HomePicker::Agent);
        home.account_picker_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(home.account_selection.is_some());
        home.account_picker_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert!(home.filtered_account_models().is_empty());
        assert!(!home.select_account_model(0));
        assert!(home.account_picker.active);
        home.account_picker_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(home.select_account_model(0));
        assert!(!home.account_picker.active);
        assert!(home
            .effort_options()
            .iter()
            .any(|effort| Some(effort) == home.effort.as_ref()));
    }
    #[test]
    fn account_picker_quota_states_do_not_invent_readiness() {
        assert_eq!(quota_lines(None, Some(100)), ["usage unknown"]);
        let mut usage = AccountUsage {
            five_hour: Some(QuotaWindow {
                used_percent: 100,
                resets_at: None,
            }),
            stale: true,
            ..Default::default()
        };
        let text = quota_lines(Some(&usage), Some(100)).join(" ");
        assert!(
            text.contains("exhausted") && text.contains("stale") && text.contains("reset unknown")
        );
        usage.unavailable = Some(UsageUnavailable::AuthExpired);
        assert!(quota_lines(Some(&usage), None)[0].contains("login required"));
        usage.unavailable = Some(UsageUnavailable::HelperMissing);
        assert_eq!(quota_lines(Some(&usage), None)[0], "helper missing");
        assert!(quota_lines(Some(&AccountUsage::default()), None)
            .join(" ")
            .contains("unknown"));
    }
}
