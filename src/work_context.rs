use std::collections::HashSet;
use std::sync::OnceLock;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

pub const MAX_PREVIEW_URLS: usize = 8;
pub const MAX_MISSIVE_URLS: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneWorkRole {
    Baseline,
    Repair,
    Ship,
    Review,
}

impl PaneWorkRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Repair => "repair",
            Self::Ship => "ship",
            Self::Review => "review",
        }
    }
}

impl std::str::FromStr for PaneWorkRole {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "baseline" => Ok(Self::Baseline),
            "repair" => Ok(Self::Repair),
            "ship" => Ok(Self::Ship),
            "review" => Ok(Self::Review),
            _ => Err(format!(
                "invalid work role {value}: expected baseline, repair, ship, or review"
            )),
        }
    }
}

/// Missive conversations are always served from this single host.
const MISSIVE_HOST: &str = "mail.missiveapp.com";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorkLinkKind {
    Ticket,
    PullRequest,
    Preview,
    Missive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorkLinkCandidate {
    pub kind: WorkLinkKind,
    pub label: String,
    pub url: String,
    /// The value copied by a link-row click. Tickets retain their compact ID;
    /// URL-backed links copy their canonical URL.
    pub copy_value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct PaneWorkContext {
    pub ticket_ids: Vec<String>,
    pub pr_urls: Vec<String>,
    #[serde(default)]
    pub preview_urls: Vec<String>,
    #[serde(default)]
    pub missive_urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Canonical `owner/repo` of the repository this pane actually works on.
    ///
    /// Deliberately NOT derived from prose: a repository named in a prompt is
    /// ambient noise, not evidence of the work. It is either declared (manual
    /// or spawn binding) or observed from the checkout's `origin` remote, and
    /// the declaration always outranks the observation — see `recompute`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_title: Option<String>,
    /// Name the agent gave this session (Claude's `ai-title`). It is an
    /// explicit identity for the work, not a derivation from the prompt or the
    /// checkout path, so display surfaces rank it above every derived title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    /// Role of this run in the lifecycle of its primary pull request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<PaneWorkRole>,
    /// Whether this pane currently owns its pull request. Other attached runs
    /// remain visible as history.
    #[serde(default, skip_serializing_if = "is_false")]
    pub active_owner: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Build the canonical, stable-order links shared by the picker and info panel.
/// Invalid values are omitted defensively even though effective contexts are
/// normally normalized at their producer boundary.
pub(crate) fn work_link_candidates(context: &PaneWorkContext) -> Vec<WorkLinkCandidate> {
    let mut seen_urls = HashSet::new();
    let mut candidates = Vec::new();

    for ticket in &context.ticket_ids {
        let Some(ticket) = normalize_ticket_id(ticket).ok() else {
            continue;
        };
        let Some(url) = linear_ticket_url(&ticket) else {
            continue;
        };
        if seen_urls.insert(url.clone()) {
            candidates.push(WorkLinkCandidate {
                kind: WorkLinkKind::Ticket,
                label: ticket.clone(),
                url,
                copy_value: ticket,
            });
        }
    }

    for raw_url in &context.pr_urls {
        let Some(url) = normalize_pr_url(raw_url).ok() else {
            continue;
        };
        if seen_urls.insert(url.clone()) {
            candidates.push(WorkLinkCandidate {
                kind: WorkLinkKind::PullRequest,
                label: url.clone(),
                copy_value: url.clone(),
                url,
            });
        }
    }

    for raw_url in &context.preview_urls {
        let Some(url) = normalize_preview_url(raw_url).ok() else {
            continue;
        };
        if seen_urls.insert(url.clone()) {
            candidates.push(WorkLinkCandidate {
                kind: WorkLinkKind::Preview,
                label: url.clone(),
                copy_value: url.clone(),
                url,
            });
        }
    }

    for raw_url in &context.missive_urls {
        let Some(url) = normalize_missive_url(raw_url).ok() else {
            continue;
        };
        if seen_urls.insert(url.clone()) {
            candidates.push(WorkLinkCandidate {
                kind: WorkLinkKind::Missive,
                label: missive_link_label(&url),
                copy_value: url.clone(),
                url,
            });
        }
    }

    candidates
}

/// Missive URLs are long and repetitive, so the panel shows the conversation
/// segment rather than the whole hash route.
fn missive_link_label(url: &str) -> String {
    url.rsplit('/')
        .find(|segment| !segment.is_empty())
        .map(|segment| format!("missive/{segment}"))
        .unwrap_or_else(|| url.to_string())
}

impl PaneWorkContext {
    pub(crate) fn normalized(self) -> Result<Self, String> {
        let normalized = Self {
            ticket_ids: normalize_ticket_ids(self.ticket_ids)?,
            pr_urls: normalize_pr_urls(self.pr_urls)?,
            preview_urls: normalize_preview_urls(self.preview_urls)?,
            missive_urls: normalize_missive_urls(self.missive_urls)?,
            branch: normalize_optional_text("branch", self.branch)?,
            repo: normalize_optional_repo(self.repo)?,
            work_title: normalize_optional_text("work title", self.work_title)?,
            session_name: normalize_optional_text("session name", self.session_name)?,
            role: self.role,
            active_owner: self.active_owner,
        };
        if normalized.active_owner && normalized.role.is_none() {
            return Err("active pull-request ownership requires a work role".into());
        }
        if normalized.role.is_some() && normalized.pr_urls.is_empty() {
            return Err("a work role requires a pull request".into());
        }
        Ok(normalized)
    }

    /// Fill an absent repository from an explicitly bound pull request.
    ///
    /// Applied only to declaration tiers. A pull request URL a human or agent
    /// deliberately bound to the pane names the repository unambiguously,
    /// whereas one merely scraped out of prompt text does not, so the hook tier
    /// never gets this treatment.
    fn with_repo_implied_by_pull_request(mut self) -> Self {
        if self.repo.is_none() {
            self.repo = self
                .pr_urls
                .first()
                .and_then(|url| repo_slug_from_pr_url(url));
        }
        self
    }

    pub(crate) fn normalized_spawn_binding(self) -> Result<Self, String> {
        let normalized = self.normalized()?.with_repo_implied_by_pull_request();
        if normalized.pr_urls.len() > 1 {
            return Err("a spawn-time binding accepts one pull request".into());
        }
        if !normalized.pr_urls.is_empty() && normalized.role.is_none() {
            return Err("a spawn-time pull-request binding requires --role".into());
        }
        Ok(normalized)
    }

    pub(crate) fn binding_display(&self, pr_has_active_owner: bool) -> Option<String> {
        self.role.map(|role| {
            let run = format!(
                "{}/{}",
                if self.active_owner {
                    "owner"
                } else {
                    "history"
                },
                role.as_str()
            );
            if !self.active_owner && !pr_has_active_owner {
                format!("{run} · un-owned")
            } else {
                run
            }
        })
    }

    // PR-1 establishes these shared projections for later API/UI consumers.
    #[allow(dead_code)]
    pub fn primary_ticket(&self) -> Option<&str> {
        self.ticket_ids.first().map(String::as_str)
    }

    #[allow(dead_code)]
    pub fn primary_pr(&self) -> Option<&str> {
        self.pr_urls.first().map(String::as_str)
    }

    pub(crate) fn is_active_owner_of(&self, pr_url: &str) -> bool {
        self.active_owner
            && self
                .primary_pr()
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(pr_url))
    }

    #[allow(dead_code)]
    pub fn primary_action_url(&self) -> Option<String> {
        self.primary_ticket()
            .and_then(linear_ticket_url)
            .or_else(|| self.primary_pr().map(str::to_string))
    }
}

/// Persisted per-tier work context, preserving source provenance across restarts.
///
/// `restored_fallback` carries values whose source tier is unknown (legacy flat
/// snapshots); live hook/git observations supersede it while `manual` stays
/// authoritative.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PaneWorkContextTiers {
    pub manual: PaneWorkContext,
    pub hook_turn: PaneWorkContext,
    pub git_observation: PaneWorkContext,
    pub restored_fallback: PaneWorkContext,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneWorkContextField {
    TicketIds,
    PrUrls,
    Branch,
    Repo,
    WorkTitle,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PaneWorkContextPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ticket_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_urls: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clear_fields: Vec<PaneWorkContextField>,
}

impl PaneWorkContextPatch {
    pub fn is_empty(&self) -> bool {
        self.ticket_ids.is_none()
            && self.pr_urls.is_none()
            && self.branch.is_none()
            && self.repo.is_none()
            && self.work_title.is_none()
            && self.clear_fields.is_empty()
    }

    fn validate_collisions(&self) -> Result<(), String> {
        let cleared: HashSet<_> = self.clear_fields.iter().copied().collect();
        for (field, supplied) in [
            (PaneWorkContextField::TicketIds, self.ticket_ids.is_some()),
            (PaneWorkContextField::PrUrls, self.pr_urls.is_some()),
            (PaneWorkContextField::Branch, self.branch.is_some()),
            (PaneWorkContextField::Repo, self.repo.is_some()),
            (PaneWorkContextField::WorkTitle, self.work_title.is_some()),
        ] {
            if supplied && cleared.contains(&field) {
                return Err(format!(
                    "cannot set and clear work-context field {}",
                    field.as_str()
                ));
            }
        }
        Ok(())
    }
}

impl PaneWorkContextField {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TicketIds => "ticket_ids",
            Self::PrUrls => "pr_urls",
            Self::Branch => "branch",
            Self::Repo => "repo",
            Self::WorkTitle => "work_title",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneWorkContextState {
    manual: PaneWorkContext,
    hook_turn: PaneWorkContext,
    git_observation: PaneWorkContext,
    /// Legacy restored values with unknown source provenance. Below every live
    /// tier in precedence; superseded by later hook/git observations.
    restored_fallback: PaneWorkContext,
    effective: PaneWorkContext,
}

impl PaneWorkContextState {
    /// Restore persisted context. When per-tier provenance was persisted it is
    /// reinstalled tier-by-tier; otherwise the legacy flat value becomes a
    /// restored fallback that later live observations supersede — it is never
    /// promoted to a manual pin.
    pub fn from_restored(context: PaneWorkContext) -> Result<Self, String> {
        let mut state = Self {
            restored_fallback: context.normalized()?,
            ..Self::default()
        };
        state.recompute();
        Ok(state)
    }

    pub fn from_restored_with_tiers(
        flat: PaneWorkContext,
        tiers: Option<PaneWorkContextTiers>,
    ) -> Result<Self, String> {
        let Some(tiers) = tiers else {
            return Self::from_restored(flat);
        };
        let mut manual = tiers.manual.normalized()?;
        manual.preview_urls.clear();
        manual.missive_urls.clear();
        let hook_turn = tiers.hook_turn.normalized()?;
        let git_observation = tiers.git_observation.normalized()?;
        let restored_fallback = tiers.restored_fallback.normalized()?;
        let mut state = Self {
            manual,
            hook_turn,
            git_observation,
            restored_fallback,
            effective: PaneWorkContext::default(),
        };
        state.recompute();
        Ok(state)
    }

    pub fn snapshot_tiers(&self) -> PaneWorkContextTiers {
        PaneWorkContextTiers {
            manual: self.manual.clone(),
            hook_turn: self.hook_turn.clone(),
            git_observation: self.git_observation.clone(),
            restored_fallback: self.restored_fallback.clone(),
        }
    }

    pub fn effective(&self) -> &PaneWorkContext {
        &self.effective
    }

    pub fn apply_manual_patch(&mut self, patch: PaneWorkContextPatch) -> Result<bool, String> {
        if patch.is_empty() {
            return Err("missing work-context field to set or clear".into());
        }
        patch.validate_collisions()?;
        let pr_binding_replaced =
            patch.pr_urls.is_some() || patch.clear_fields.contains(&PaneWorkContextField::PrUrls);
        // A pull request implies its repository only when this patch actually
        // binds one, and never when the same patch clears the repository. A
        // patch that merely touches an unrelated field must not re-derive it,
        // or an explicit `--clear repo` would silently come back and an
        // unrelated `--title` edit could relocate the pane.
        let imply_repo_from_pull_request =
            patch.pr_urls.is_some() && !patch.clear_fields.contains(&PaneWorkContextField::Repo);

        let mut candidate = self.manual.clone();
        if let Some(ticket_ids) = patch.ticket_ids {
            candidate.ticket_ids = ticket_ids;
        }
        if let Some(pr_urls) = patch.pr_urls {
            candidate.pr_urls = pr_urls;
        }
        if let Some(branch) = patch.branch {
            candidate.branch = Some(branch);
        }
        if let Some(repo) = patch.repo {
            candidate.repo = Some(repo);
        }
        if let Some(work_title) = patch.work_title {
            candidate.work_title = Some(work_title);
        }
        for field in patch.clear_fields {
            match field {
                PaneWorkContextField::TicketIds => candidate.ticket_ids.clear(),
                PaneWorkContextField::PrUrls => candidate.pr_urls.clear(),
                PaneWorkContextField::Branch => candidate.branch = None,
                PaneWorkContextField::Repo => candidate.repo = None,
                PaneWorkContextField::WorkTitle => candidate.work_title = None,
            }
        }
        if pr_binding_replaced || candidate.pr_urls.is_empty() {
            candidate.role = None;
            candidate.active_owner = false;
        }
        let mut candidate = candidate.normalized()?;
        if imply_repo_from_pull_request {
            candidate = candidate.with_repo_implied_by_pull_request();
        }
        if candidate == self.manual {
            return Ok(false);
        }
        self.manual = candidate;
        self.recompute();
        Ok(true)
    }

    pub(crate) fn replace_manual_normalized(&mut self, context: PaneWorkContext) -> bool {
        if context == self.manual {
            return false;
        }
        self.manual = context;
        self.recompute();
        true
    }

    pub(crate) fn clear_active_owner(&mut self) -> bool {
        let mut changed = false;
        for context in [
            &mut self.manual,
            &mut self.hook_turn,
            &mut self.git_observation,
            &mut self.restored_fallback,
        ] {
            changed |= context.active_owner;
            context.active_owner = false;
        }
        if changed {
            self.recompute();
        }
        changed
    }

    // PR-1 defines source-tier replacement before hook and git producers land.
    #[allow(dead_code)]
    pub fn replace_hook_turn(&mut self, context: PaneWorkContext) -> Result<bool, String> {
        let context = context.normalized()?;
        // Any live observation supersedes the unknown-provenance legacy value.
        let fallback_changed = self.clear_restored_fallback();
        if context == self.hook_turn && !fallback_changed {
            return Ok(false);
        }
        self.hook_turn = context;
        self.recompute();
        Ok(true)
    }

    /// Apply only the agent-reported session name to the hook tier.
    ///
    /// A rename arrives outside a turn, so it carries no prompt and therefore
    /// no ticket or link evidence. Replacing the whole hook tier would drop the
    /// context the last turn established, so the name is patched in place.
    pub fn set_hook_session_name(&mut self, session_name: Option<String>) -> Result<bool, String> {
        let session_name = normalize_optional_text("session name", session_name)?;
        if self.hook_turn.session_name == session_name {
            return Ok(false);
        }
        self.hook_turn.session_name = session_name;
        self.clear_restored_fallback();
        self.recompute();
        Ok(true)
    }

    /// Drops the hook tier when the agent session that authorized it ends or
    /// is replaced; manual, git, and restored-fallback tiers are preserved.
    pub fn clear_hook_turn(&mut self) -> bool {
        if self.hook_turn == PaneWorkContext::default() {
            return false;
        }
        self.hook_turn = PaneWorkContext::default();
        self.recompute();
        true
    }

    pub fn replace_git_observation(&mut self, context: PaneWorkContext) -> Result<bool, String> {
        let context = context.normalized()?;
        // Any live observation supersedes the unknown-provenance legacy value.
        let fallback_changed = self.clear_restored_fallback();
        if context == self.git_observation && !fallback_changed {
            return Ok(false);
        }
        self.git_observation = context;
        self.recompute();
        Ok(true)
    }

    fn clear_restored_fallback(&mut self) -> bool {
        if self.restored_fallback == PaneWorkContext::default() {
            return false;
        }
        self.restored_fallback = PaneWorkContext::default();
        true
    }

    fn recompute(&mut self) {
        let (role, active_owner) = [
            &self.manual,
            &self.hook_turn,
            &self.git_observation,
            &self.restored_fallback,
        ]
        .into_iter()
        .find_map(|context| context.role.map(|role| (Some(role), context.active_owner)))
        .unwrap_or((None, false));
        self.effective = PaneWorkContext {
            ticket_ids: stable_merge([
                &self.manual.ticket_ids,
                &self.hook_turn.ticket_ids,
                &self.git_observation.ticket_ids,
                &self.restored_fallback.ticket_ids,
            ]),
            pr_urls: stable_merge([
                &self.manual.pr_urls,
                &self.hook_turn.pr_urls,
                &self.git_observation.pr_urls,
                &self.restored_fallback.pr_urls,
            ]),
            preview_urls: stable_merge([
                &self.manual.preview_urls,
                &self.hook_turn.preview_urls,
                &self.git_observation.preview_urls,
                &self.restored_fallback.preview_urls,
            ])
            .into_iter()
            .take(MAX_PREVIEW_URLS)
            .collect(),
            missive_urls: stable_merge([
                &self.manual.missive_urls,
                &self.hook_turn.missive_urls,
                &self.git_observation.missive_urls,
                &self.restored_fallback.missive_urls,
            ])
            .into_iter()
            .take(MAX_MISSIVE_URLS)
            .collect(),
            branch: first_present([
                self.manual.branch.as_ref(),
                self.hook_turn.branch.as_ref(),
                self.git_observation.branch.as_ref(),
                self.restored_fallback.branch.as_ref(),
            ]),
            // Declaration beats observation. The git tier is derived from the
            // pane cwd, which is a poor proxy for the work: many sessions run
            // from one shared worktree while operating on other repositories.
            // Ordering it last is what stops a misleading cwd from misfiling a
            // pane that has declared its repository.
            repo: first_present([
                self.manual.repo.as_ref(),
                self.hook_turn.repo.as_ref(),
                self.git_observation.repo.as_ref(),
                self.restored_fallback.repo.as_ref(),
            ]),
            work_title: first_present([
                self.manual.work_title.as_ref(),
                self.hook_turn.work_title.as_ref(),
                self.git_observation.work_title.as_ref(),
                self.restored_fallback.work_title.as_ref(),
            ]),
            session_name: first_present([
                self.manual.session_name.as_ref(),
                self.hook_turn.session_name.as_ref(),
                self.git_observation.session_name.as_ref(),
                self.restored_fallback.session_name.as_ref(),
            ]),
            role,
            active_owner,
        };
    }
}

fn first_present<const N: usize>(values: [Option<&String>; N]) -> Option<String> {
    values.into_iter().flatten().next().cloned()
}

fn stable_merge<const N: usize>(sources: [&Vec<String>; N]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for value in sources.into_iter().flat_map(|source| source.iter()) {
        if seen.insert(value.clone()) {
            merged.push(value.clone());
        }
    }
    merged
}

fn normalize_optional_text(field: &str, value: Option<String>) -> Result<Option<String>, String> {
    value
        .map(|value| {
            let value = value.trim();
            if value.is_empty() || value.chars().any(char::is_control) {
                Err(format!("invalid {field}"))
            } else {
                Ok(value.to_string())
            }
        })
        .transpose()
}

fn ticket_regex() -> &'static Regex {
    static TICKET_REGEX: OnceLock<Regex> = OnceLock::new();
    TICKET_REGEX.get_or_init(|| {
        RegexBuilder::new(r"(?:MAT|SCA)-[0-9]+")
            .case_insensitive(true)
            .unicode(false)
            .build()
            .expect("work-context ticket regex is valid")
    })
}

fn is_ascii_token_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

pub fn extract_ticket_ids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut seen = HashSet::new();
    let mut tickets = Vec::new();
    for matched in ticket_regex().find_iter(text) {
        let left_ok = matched.start() == 0 || !is_ascii_token_char(bytes[matched.start() - 1]);
        let right_ok = matched.end() == bytes.len() || !is_ascii_token_char(bytes[matched.end()]);
        if left_ok && right_ok {
            let ticket = matched.as_str().to_ascii_uppercase();
            if seen.insert(ticket.clone()) {
                tickets.push(ticket);
            }
        }
    }
    tickets
}

/// Longest candidate a host-only preview URL may be; the preview normalizer rejects anything with a
/// path, so these are always short.
const MAX_PREVIEW_CANDIDATE_BYTES: usize = 128;
/// Longest candidate a routed URL may be. Every extractor must bound its candidate: a hostile or
/// merely pathological prompt (a log line of repeated `https://` with no whitespace) otherwise costs
/// O(len) per match over O(len) matches, and the turn hook stalls before it reports any metadata.
const MAX_ROUTED_CANDIDATE_BYTES: usize = 256;

/// The token starting at a match, or `None` when it runs past `max_bytes` without ending.
fn bounded_candidate(remaining: &str, max_bytes: usize) -> Option<&str> {
    match remaining
        .as_bytes()
        .iter()
        .take(max_bytes + 1)
        .position(|byte| byte.is_ascii_whitespace())
    {
        Some(end) => Some(&remaining[..end]),
        None if remaining.len() <= max_bytes => Some(remaining),
        None => None,
    }
}

pub fn extract_pr_urls(text: &str) -> Vec<String> {
    const PREFIX: &str = "https://github.com/";

    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    for (start, _) in text.match_indices(PREFIX) {
        if start > 0 && is_ascii_token_char(text.as_bytes()[start - 1]) {
            continue;
        }
        let Some(candidate) = bounded_candidate(&text[start..], MAX_ROUTED_CANDIDATE_BYTES) else {
            continue;
        };
        let Ok(url) = normalize_pr_url(candidate) else {
            continue;
        };
        if seen.insert(url.clone()) {
            urls.push(url);
        }
    }
    urls
}

pub fn extract_preview_urls(text: &str) -> Vec<String> {
    const PREFIX: &str = "https://";

    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    for (start, _) in text.match_indices(PREFIX) {
        if start > 0 && is_ascii_token_char(text.as_bytes()[start - 1]) {
            continue;
        }
        let Some(candidate) = bounded_candidate(&text[start..], MAX_PREVIEW_CANDIDATE_BYTES) else {
            continue;
        };
        let Ok(url) = normalize_preview_url(candidate) else {
            continue;
        };
        if seen.insert(url.clone()) {
            urls.push(url);
            if urls.len() == MAX_PREVIEW_URLS {
                break;
            }
        }
    }
    urls
}

pub fn extract_missive_urls(text: &str) -> Vec<String> {
    let prefix = format!("https://{MISSIVE_HOST}");
    // Hosts are case-insensitive, so scan a lowered copy. `to_ascii_lowercase` maps bytes 1:1 and
    // leaves non-ASCII untouched, so every index it yields is valid in the original text.
    let lowered = text.to_ascii_lowercase();
    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    for (start, _) in lowered.match_indices(prefix.as_str()) {
        if start > 0 && is_ascii_token_char(text.as_bytes()[start - 1]) {
            continue;
        }
        let Some(candidate) = bounded_candidate(&text[start..], MAX_ROUTED_CANDIDATE_BYTES) else {
            continue;
        };
        let Ok(url) = normalize_missive_url(candidate) else {
            continue;
        };
        if seen.insert(url.clone()) {
            urls.push(url);
            if urls.len() == MAX_MISSIVE_URLS {
                break;
            }
        }
    }
    urls
}

pub fn normalize_missive_urls<I, S>(urls: I) -> Result<Vec<String>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for url in urls {
        let url = normalize_missive_url(url.as_ref())?;
        if seen.insert(url.clone()) {
            normalized.push(url);
            if normalized.len() == MAX_MISSIVE_URLS {
                break;
            }
        }
    }
    Ok(normalized)
}

/// Missive conversation routes carry a hash path, so unlike preview URLs the
/// path and fragment are significant and must survive normalization.
fn normalize_missive_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches(|character: char| {
        matches!(
            character,
            '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ',' | ';' | '!' | '.' | '\'' | '"'
        )
    });
    // The message never echoes the input: it reaches logs, and a rejected candidate is untrusted
    // text of arbitrary length that may carry a token in its query.
    let invalid = || "invalid Missive URL".to_string();
    let authority_len = "https://".len() + MISSIVE_HOST.len();
    if trimmed.len() > MAX_ROUTED_CANDIDATE_BYTES
        || trimmed.len() <= authority_len
        || !trimmed.is_char_boundary(authority_len)
        || trimmed.chars().any(char::is_control)
        || trimmed.bytes().any(|byte| byte.is_ascii_whitespace())
    {
        return Err(invalid());
    }
    let (authority, rest) = trimmed.split_at(authority_len);
    // Scheme and host are case-insensitive; the route after them is not and must survive verbatim.
    if !authority.eq_ignore_ascii_case(&format!("https://{MISSIVE_HOST}")) {
        return Err(invalid());
    }
    if !rest.starts_with('/') && !rest.starts_with('#') {
        return Err(invalid());
    }
    if !missive_route_is_conversation(rest) {
        return Err(invalid());
    }
    Ok(format!("https://{MISSIVE_HOST}{rest}"))
}

/// True for a route addressing one conversation, under any view: `#inbox/conversations/<id>`
/// or `#custom/<team>/conversations/<id>`. Missive's other routes — settings, search, contacts
/// — carry no pane-specific work context, and accepting them lets a handful of them exhaust
/// `MAX_MISSIVE_URLS` before a real conversation link is reached.
fn missive_route_is_conversation(route: &str) -> bool {
    let route = route.split('?').next().unwrap_or(route);
    let mut segments = route
        .split(['/', '#'])
        .filter(|segment| !segment.is_empty())
        .skip_while(|segment| !segment.eq_ignore_ascii_case("conversations"));
    segments.next().is_some() && segments.next().is_some()
}

pub(crate) fn hook_turn_context(
    work_title: Option<String>,
    branch: Option<&str>,
    prompt_context: PaneWorkContext,
) -> Result<PaneWorkContext, String> {
    let prompt_context = prompt_context.normalized()?;
    let mut ticket_ids = Vec::new();
    if let Some(title) = work_title.as_deref() {
        ticket_ids.extend(extract_ticket_ids(title));
    }
    if let Some(branch) = branch {
        ticket_ids.extend(extract_ticket_ids(branch));
    }
    ticket_ids.extend(prompt_context.ticket_ids);

    Ok(PaneWorkContext {
        ticket_ids: normalize_ticket_ids(ticket_ids)?,
        pr_urls: prompt_context.pr_urls,
        preview_urls: prompt_context.preview_urls,
        missive_urls: prompt_context.missive_urls,
        branch: None,
        // The hook tier never carries a repository: see the note on
        // `PaneWorkContext::repo`. Agents declare theirs through the manual
        // tier (`herdr pane work-context set --repo`), which also outranks the
        // cwd-derived git observation.
        repo: None,
        work_title,
        session_name: prompt_context.session_name,
        role: None,
        active_owner: false,
    })
}

fn normalize_ticket_id(ticket: &str) -> Result<String, String> {
    let ticket = ticket.trim();
    let tickets = extract_ticket_ids(ticket);
    if tickets.len() == 1 && tickets[0].len() == ticket.len() {
        Ok(tickets.into_iter().next().expect("one ticket was checked"))
    } else {
        Err(format!("invalid ticket ID: {ticket}"))
    }
}

pub fn normalize_ticket_ids<I, S>(tickets: I) -> Result<Vec<String>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for ticket in tickets {
        let ticket = normalize_ticket_id(ticket.as_ref())?;
        if seen.insert(ticket.clone()) {
            normalized.push(ticket);
        }
    }
    Ok(normalized)
}

pub fn normalize_pr_urls<I, S>(urls: I) -> Result<Vec<String>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for url in urls {
        let url = normalize_pr_url(url.as_ref())?;
        if seen.insert(url.clone()) {
            normalized.push(url);
        }
    }
    Ok(normalized)
}

pub fn normalize_preview_urls<I, S>(urls: I) -> Result<Vec<String>, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for url in urls {
        let url = normalize_preview_url(url.as_ref())?;
        if seen.insert(url.clone()) {
            normalized.push(url);
            if normalized.len() == MAX_PREVIEW_URLS {
                break;
            }
        }
    }
    Ok(normalized)
}

fn normalize_pr_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_matches(|character: char| {
        matches!(
            character,
            '(' | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
                | ','
                | ';'
                | ':'
                | '!'
                | '.'
                | '\''
                | '"'
        )
    });
    let Some(path) = trimmed.strip_prefix("https://github.com/") else {
        return Err(format!("invalid GitHub pull request URL: {raw}"));
    };
    if trimmed.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(format!("invalid GitHub pull request URL: {raw}"));
    }
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 4
        || !valid_github_owner(parts[0])
        || !valid_github_repo(parts[1])
        || parts[2] != "pull"
        || parts[3].is_empty()
        || !parts[3].bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(format!("invalid GitHub pull request URL: {raw}"));
    }
    let number = parts[3]
        .parse::<u64>()
        .map_err(|_| format!("invalid GitHub pull request URL: {raw}"))?;
    if number == 0 {
        return Err(format!("invalid GitHub pull request URL: {raw}"));
    }
    Ok(format!(
        "https://github.com/{}/{}/pull/{number}",
        parts[0], parts[1]
    ))
}

fn normalize_preview_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_matches(|character: char| {
        matches!(
            character,
            '(' | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
                | ','
                | ';'
                | ':'
                | '!'
                | '.'
                | '\''
                | '"'
        )
    });
    let Some(authority) = trimmed.strip_prefix("https://") else {
        return Err(format!("invalid Vercel preview URL: {raw}"));
    };
    if authority.is_empty()
        || authority.bytes().any(|byte| byte.is_ascii_whitespace())
        || authority.contains(['/', '?', '#', '@', ':'])
    {
        return Err(format!("invalid Vercel preview URL: {raw}"));
    }

    let authority = authority.to_ascii_lowercase();
    let Some(subdomain) = authority.strip_suffix(".vercel.app") else {
        return Err(format!("invalid Vercel preview URL: {raw}"));
    };
    if !valid_vercel_subdomain(subdomain) {
        return Err(format!("invalid Vercel preview URL: {raw}"));
    }
    Ok(format!("https://{subdomain}.vercel.app"))
}

fn valid_vercel_subdomain(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// Canonicalize a repository declaration to `owner/repo`.
///
/// Accepts the bare slug, a GitHub web/API URL, and both SSH and HTTPS remote
/// forms, so a caller can pass whatever `git remote get-url origin` printed.
pub fn normalize_repo_slug(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("repository must not be empty".into());
    }
    if trimmed.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(format!("invalid repository: {raw}"));
    }
    // Strip a transport prefix so remote URLs and web URLs collapse onto the
    // same `owner/repo` path as a bare slug. Whenever a host is present it must
    // be github.com: `owner/repo` is only a repository identity on GitHub, and
    // routing a GitLab mirror into a GitHub-bound workspace would put a pane in
    // the wrong space and then block binding the two separately.
    let path = if let Some((_, rest)) = trimmed.split_once("://") {
        match rest.split_once('/') {
            Some((authority, path)) if is_github_authority(authority) => path,
            _ => return Err(format!("invalid repository: {raw}")),
        }
    } else if let Some((authority, path)) = trimmed.split_once(':') {
        if !is_github_authority(authority) {
            return Err(format!("invalid repository: {raw}"));
        }
        path
    } else {
        trimmed
    };
    let path = path.trim_start_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let parts: Vec<_> = path.split('/').collect();
    let [owner, repo] = parts.as_slice() else {
        return Err(format!("invalid repository: {raw}"));
    };
    if !valid_github_owner(owner) || !valid_github_repo(repo) {
        return Err(format!("invalid repository: {raw}"));
    }
    Ok(format!("{owner}/{repo}"))
}

/// Whether a remote authority points at github.com.
///
/// Accepts an optional `user@` prefix and an optional `:port` suffix, so both
/// `git@github.com` and `github.com:443` resolve. Any other host is refused
/// rather than silently reduced to its `owner/repo` path.
fn is_github_authority(authority: &str) -> bool {
    if authority.is_empty() || authority.contains('/') || authority.contains('\\') {
        return false;
    }
    let host = match authority.rsplit_once('@') {
        Some((user, host)) if !user.is_empty() => host,
        Some(_) => return false,
        None => authority,
    };
    let host = host.rsplit_once(':').map_or(host, |(host, _port)| host);
    host.eq_ignore_ascii_case("github.com")
}

/// Repository slugs are compared case-insensitively: GitHub preserves the case
/// an owner registered but does not distinguish on it, so `matthiasSchedel/ghx`
/// and `matthiasschedel/ghx` are one repository and must route to one space.
pub fn repo_slugs_match(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn normalize_optional_repo(value: Option<String>) -> Result<Option<String>, String> {
    value.map(|repo| normalize_repo_slug(&repo)).transpose()
}

/// Extract `owner/repo` from an already-normalized GitHub pull request URL.
pub fn repo_slug_from_pr_url(url: &str) -> Option<String> {
    let path = url.strip_prefix("https://github.com/")?;
    let mut parts = path.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if parts.next()? != "pull" || !valid_github_owner(owner) || !valid_github_repo(repo) {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn valid_github_owner(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_github_repo(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub fn linear_ticket_url(ticket: &str) -> Option<String> {
    let ticket = normalize_ticket_id(ticket).ok()?;
    Some(format!("https://linear.app/scalable/issue/{ticket}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac2_ticket_normalization_uses_ascii_token_boundaries_and_first_seen_order() {
        assert_eq!(
            extract_ticket_ids("sca-12, MAT-7; SCA-12"),
            vec!["SCA-12", "MAT-7"]
        );
        assert!(extract_ticket_ids("FORMAT-12 XMAT-3 MAT-4X _SCA-5").is_empty());
    }

    #[test]
    fn ac2_pr_url_normalization_rejects_ambiguous_or_unsafe_urls() {
        assert_eq!(
            normalize_pr_urls(["(https://github.com/owner/repo/pull/0042)."])
                .expect("valid pull URL"),
            vec!["https://github.com/owner/repo/pull/42"]
        );
        for invalid in [
            "http://github.com/o/r/pull/1",
            "https://user@github.com/o/r/pull/1",
            "https://github.com.evil.test/o/r/pull/1",
            "https://github.com:443/o/r/pull/1",
            "https://github.com/o/r/pull/0",
            "https://github.com/o/r/pull/1/",
            "https://github.com/o/r/pull/1?",
            "https://github.com/o/r/pull/1?x=1",
            "https://github.com/o/r/pull/1#fragment",
            "https://github.com/o/%2Frepo/pull/1",
        ] {
            assert!(normalize_pr_urls([invalid]).is_err(), "accepted {invalid}");
        }
    }

    #[test]
    fn ac25_preview_url_normalization_accepts_only_canonical_vercel_roots() {
        assert_eq!(
            normalize_preview_urls([
                "(https://Preview-123.Vercel.App).",
                "https://preview-123.vercel.app",
                "https://second-preview.vercel.app",
            ])
            .expect("valid preview URLs"),
            vec![
                "https://preview-123.vercel.app",
                "https://second-preview.vercel.app"
            ]
        );

        for invalid in [
            "http://preview.vercel.app",
            "https://vercel.app",
            "https://preview.other.test",
            "https://preview.team.vercel.app",
            "https://-preview.vercel.app",
            "https://preview-.vercel.app",
            "https://preview_vercel.vercel.app",
            "https://preview.vercel.app/path",
            "https://preview.vercel.app:443",
            "https://user:password@preview.vercel.app",
            "https://preview.vercel.app?token=secret",
            "https://preview.vercel.app#fragment",
            &format!("https://{}.vercel.app", "p".repeat(64)),
        ] {
            assert!(
                normalize_preview_urls([invalid]).is_err(),
                "accepted unsafe preview URL {invalid}"
            );
        }
    }

    #[test]
    fn ac25_preview_url_extraction_filters_untrusted_text_and_caps_stable_order() {
        let text = "https://first.vercel.app https://user@bad.vercel.app https://second.vercel.app https://first.vercel.app https://third.vercel.app";
        assert_eq!(
            extract_preview_urls(text),
            vec![
                "https://first.vercel.app",
                "https://second.vercel.app",
                "https://third.vercel.app"
            ]
        );

        let many = (0..MAX_PREVIEW_URLS + 2)
            .map(|index| format!("https://preview-{index}.vercel.app"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(extract_preview_urls(&many).len(), MAX_PREVIEW_URLS);
        assert_eq!(
            normalize_preview_urls(
                (0..MAX_PREVIEW_URLS + 2)
                    .map(|index| { format!("https://preview-{index}.vercel.app") })
            )
            .unwrap()
            .len(),
            MAX_PREVIEW_URLS
        );
    }

    #[test]
    fn missive_urls_are_extracted_with_their_hash_route_and_bounded() {
        let text = concat!(
            "see https://mail.missiveapp.com/#inbox/conversations/abc123, ",
            "https://mail.missiveapp.com/#inbox/conversations/abc123 ",
            "https://mail.missiveapp.com/#custom/team/conversations/def456 ",
            "https://mail.missiveapp.com ",
            "https://evil.example.com/#inbox/conversations/zzz ",
            "xhttps://mail.missiveapp.com/#inbox/conversations/prefixed"
        );

        assert_eq!(
            extract_missive_urls(text),
            vec![
                "https://mail.missiveapp.com/#inbox/conversations/abc123",
                "https://mail.missiveapp.com/#custom/team/conversations/def456",
            ]
        );

        let many = (0..MAX_MISSIVE_URLS + 2)
            .map(|index| format!("https://mail.missiveapp.com/#inbox/conversations/c{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(extract_missive_urls(&many).len(), MAX_MISSIVE_URLS);
        assert_eq!(
            normalize_missive_urls(
                (0..MAX_MISSIVE_URLS + 2).map(|index| format!(
                    "https://mail.missiveapp.com/#inbox/conversations/c{index}"
                ))
            )
            .unwrap()
            .len(),
            MAX_MISSIVE_URLS
        );
        assert!(normalize_missive_urls(["https://mail.missiveapp.com"]).is_err());
        assert!(normalize_missive_urls([
            "https://mail.missiveapp.com.evil.test/#inbox/conversations/abc123"
        ])
        .is_err());
    }

    #[test]
    fn url_extraction_stays_linear_on_a_whitespace_free_prefix_flood() {
        // Each repeated prefix used to scan the entire remaining suffix, so a single pasted log
        // line cost O(len^2) and stalled the turn hook for tens of seconds before reporting
        // anything. Every extractor must bound its candidate.
        let missive = "(https://mail.missiveapp.com/".repeat(20_000);
        let previews = "(https://preview.example.com".repeat(20_000);
        let prs = "(https://github.com/o/r/pull/1".repeat(20_000);
        for (name, text) in [("missive", &missive), ("preview", &previews), ("pr", &prs)] {
            let started = std::time::Instant::now();
            let _ = match name {
                "missive" => extract_missive_urls(text),
                "preview" => extract_preview_urls(text),
                _ => extract_pr_urls(text),
            };
            let elapsed = started.elapsed();
            assert!(
                elapsed < std::time::Duration::from_secs(2),
                "{name} extraction took {elapsed:?} on a {} byte flood",
                text.len()
            );
        }
    }

    #[test]
    fn missive_host_matching_ignores_case_without_touching_the_route() {
        assert_eq!(
            extract_missive_urls("see HTTPS://MAIL.MissiveApp.com/#inbox/conversations/AbC123 now"),
            vec!["https://mail.missiveapp.com/#inbox/conversations/AbC123"],
            "the host folds to lowercase but the conversation id must survive verbatim"
        );
        assert!(
            normalize_missive_urls(["https://MAIL.missiveapp.com/#inbox/conversations/x"]).is_ok()
        );
        // Case folding must not widen the host: a confusable neighbour still has to fail.
        assert!(normalize_missive_urls([
            "https://mail.missiveapp.com.evil.test/#inbox/conversations/x"
        ])
        .is_err());
        assert!(normalize_missive_urls([
            "https://mail.missiveapp.com@evil.test/#inbox/conversations/x"
        ])
        .is_err());
    }

    #[test]
    fn missive_non_conversation_routes_cannot_crowd_out_a_conversation_link() {
        for route in [
            "/#settings/organization",
            "/#search/from:someone",
            "/#contacts",
            "/",
            "/#inbox/conversations",
        ] {
            assert!(
                normalize_missive_urls([format!("https://{MISSIVE_HOST}{route}")]).is_err(),
                "{route} is not a conversation route"
            );
        }

        // The cap is small, so a handful of settings links pasted ahead of the real
        // conversation used to consume it and drop the only link worth showing.
        let noise = (0..MAX_MISSIVE_URLS)
            .map(|index| format!("https://{MISSIVE_HOST}/#settings/s{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let text = format!("{noise} https://{MISSIVE_HOST}/#inbox/conversations/kept");
        assert_eq!(
            extract_missive_urls(&text),
            vec![format!("https://{MISSIVE_HOST}/#inbox/conversations/kept")]
        );
    }

    #[test]
    fn missive_rejection_never_echoes_the_candidate() {
        let secret = "https://mail.missiveapp.com.evil.test/?token=SUPERSECRET";
        let error = normalize_missive_urls([secret]).unwrap_err();
        assert!(
            !error.contains("SUPERSECRET") && !error.contains("evil.test"),
            "a rejected candidate reaches logs and must not be echoed: {error}"
        );
    }

    #[test]
    fn missive_urls_survive_a_snapshot_round_trip_in_both_directions() {
        let context = PaneWorkContext {
            missive_urls: vec!["https://mail.missiveapp.com/#inbox/conversations/c1".into()],
            ..Default::default()
        };
        let encoded = serde_json::to_string(&context).unwrap();
        assert!(encoded.contains("conversations/c1"));
        assert_eq!(
            serde_json::from_str::<PaneWorkContext>(&encoded).unwrap(),
            context
        );

        // A snapshot written by an older build has no such key at all.
        let legacy = r#"{"ticket_ids":["MAT-1"],"pr_urls":[]}"#;
        assert_eq!(
            serde_json::from_str::<PaneWorkContext>(legacy)
                .unwrap()
                .missive_urls,
            Vec::<String>::new()
        );

        // And a snapshot written by this build must still load on a build without the field.
        #[derive(Default, serde::Deserialize)]
        #[serde(default)]
        struct OlderPaneWorkContext {
            ticket_ids: Vec<String>,
        }
        let older: OlderPaneWorkContext = serde_json::from_str(&encoded)
            .expect("an older build must ignore the field it does not know");
        assert!(older.ticket_ids.is_empty());
    }

    #[test]
    fn missive_links_follow_previews_and_copy_the_full_url() {
        let candidates = work_link_candidates(&PaneWorkContext {
            preview_urls: vec!["https://preview-1.vercel.app".into()],
            missive_urls: vec![
                "https://mail.missiveapp.com/#inbox/conversations/abc123".into(),
                "not-a-missive-url".into(),
            ],
            ..Default::default()
        });

        assert_eq!(
            candidates.iter().map(|c| c.kind).collect::<Vec<_>>(),
            vec![WorkLinkKind::Preview, WorkLinkKind::Missive]
        );
        let missive = candidates.last().expect("missive candidate");
        assert_eq!(missive.label, "missive/abc123");
        assert_eq!(
            missive.copy_value,
            "https://mail.missiveapp.com/#inbox/conversations/abc123"
        );
        assert_eq!(missive.url, missive.copy_value);
    }

    #[test]
    fn missive_urls_merge_across_tiers_and_never_persist_as_manual() {
        let mut state = PaneWorkContextState::default();
        state
            .replace_hook_turn(PaneWorkContext {
                missive_urls: vec!["https://mail.missiveapp.com/#inbox/conversations/hook".into()],
                ..Default::default()
            })
            .unwrap();
        state
            .replace_git_observation(PaneWorkContext {
                missive_urls: vec!["https://mail.missiveapp.com/#inbox/conversations/git".into()],
                ..Default::default()
            })
            .unwrap();

        assert_eq!(
            state.effective().missive_urls,
            vec![
                "https://mail.missiveapp.com/#inbox/conversations/hook",
                "https://mail.missiveapp.com/#inbox/conversations/git",
            ]
        );

        let restored = PaneWorkContextState::from_restored_with_tiers(
            PaneWorkContext::default(),
            Some(PaneWorkContextTiers {
                manual: PaneWorkContext {
                    repo: None,
                    missive_urls: vec![
                        "https://mail.missiveapp.com/#inbox/conversations/manual".into()
                    ],
                    ..Default::default()
                },
                ..Default::default()
            }),
        )
        .unwrap();
        assert!(restored.effective().missive_urls.is_empty());
    }

    #[test]
    fn ac26_work_link_candidates_are_ordered_and_defensively_canonicalized() {
        let candidates = work_link_candidates(&PaneWorkContext {
            ticket_ids: vec!["mat-7".into(), "SCA-8".into()],
            pr_urls: vec!["https://github.com/o/r/pull/0042".into(), "not-a-pr".into()],
            preview_urls: vec![
                "https://Preview-1.Vercel.App".into(),
                "https://preview-2.vercel.app".into(),
            ],
            ..Default::default()
        });

        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.url.as_str())
                .collect::<Vec<_>>(),
            vec![
                "https://linear.app/scalable/issue/MAT-7",
                "https://linear.app/scalable/issue/SCA-8",
                "https://github.com/o/r/pull/42",
                "https://preview-1.vercel.app",
                "https://preview-2.vercel.app",
            ]
        );
        assert_eq!(candidates[0].copy_value, "MAT-7");
        assert_eq!(candidates[2].copy_value, "https://github.com/o/r/pull/42");
    }

    #[test]
    fn ac25_preview_url_extraction_rejects_large_whitespace_free_input_quickly() {
        let text = "https://".repeat(1_000_000 / "https://".len());
        let started = std::time::Instant::now();

        assert!(extract_preview_urls(&text).is_empty());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn ac2_linear_url_canonicalizes_supported_ticket_ids() {
        assert_eq!(
            linear_ticket_url("mat-123").as_deref(),
            Some("https://linear.app/scalable/issue/MAT-123")
        );
        assert_eq!(linear_ticket_url("FORMAT-12"), None);
    }

    #[test]
    fn ac1_tiers_replace_and_merge_with_stable_dedupe() {
        let mut state = PaneWorkContextState::default();
        state
            .replace_git_observation(PaneWorkContext {
                ticket_ids: vec!["SCA-3".into(), "MAT-2".into()],
                branch: Some("feat/work".into()),
                ..PaneWorkContext::default()
            })
            .unwrap();
        state
            .replace_hook_turn(PaneWorkContext {
                ticket_ids: vec!["sca-1".into(), "SCA-3".into()],
                preview_urls: vec!["https://first.vercel.app".into()],
                work_title: Some("Hook title".into()),
                ..PaneWorkContext::default()
            })
            .unwrap();
        state
            .apply_manual_patch(PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-2".into(), "SCA-1".into()]),
                work_title: Some("Manual title".into()),
                ..PaneWorkContextPatch::default()
            })
            .unwrap();

        assert_eq!(
            state.effective().ticket_ids,
            vec!["MAT-2", "SCA-1", "SCA-3"]
        );
        assert_eq!(state.effective().branch.as_deref(), Some("feat/work"));
        assert_eq!(
            state.effective().preview_urls,
            vec!["https://first.vercel.app"]
        );
        assert_eq!(
            state.effective().work_title.as_deref(),
            Some("Manual title")
        );

        state
            .replace_hook_turn(PaneWorkContext {
                ticket_ids: vec!["MAT-9".into()],
                preview_urls: vec!["https://second.vercel.app".into()],
                ..PaneWorkContext::default()
            })
            .unwrap();
        assert_eq!(
            state.effective().ticket_ids,
            vec!["MAT-2", "SCA-1", "MAT-9", "SCA-3"]
        );
        assert_eq!(
            state.effective().preview_urls,
            vec!["https://second.vercel.app"]
        );
    }

    #[test]
    fn ac25_manual_and_git_tiers_keep_git_preview_urls_bounded() {
        let mut state = PaneWorkContextState::default();
        state
            .replace_git_observation(PaneWorkContext {
                preview_urls: vec![
                    "https://git-1.vercel.app".into(),
                    "https://git-2.vercel.app".into(),
                ],
                ..PaneWorkContext::default()
            })
            .unwrap();
        state
            .apply_manual_patch(PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-1".into()]),
                ..PaneWorkContextPatch::default()
            })
            .unwrap();

        assert_eq!(
            state.effective().preview_urls,
            vec!["https://git-1.vercel.app", "https://git-2.vercel.app"]
        );
    }

    #[test]
    fn hook_pr_url_precedes_git_observation_pr_url() {
        let mut state = PaneWorkContextState::default();
        state
            .replace_git_observation(PaneWorkContext {
                pr_urls: vec!["https://github.com/o/r/pull/1".into()],
                ..PaneWorkContext::default()
            })
            .unwrap();
        state
            .replace_hook_turn(PaneWorkContext {
                pr_urls: vec!["https://github.com/o/r/pull/2".into()],
                ..PaneWorkContext::default()
            })
            .unwrap();

        assert_eq!(
            state.effective().pr_urls,
            vec![
                "https://github.com/o/r/pull/2",
                "https://github.com/o/r/pull/1"
            ]
        );
    }

    #[test]
    fn repo_slugs_are_canonicalized_from_every_form_a_caller_may_hold() {
        for (input, expected) in [
            ("owner/repo", "owner/repo"),
            ("owner/repo/", "owner/repo"),
            ("owner/repo.git", "owner/repo"),
            ("https://github.com/owner/repo", "owner/repo"),
            ("https://github.com/owner/repo.git", "owner/repo"),
            ("git@github.com:owner/repo.git", "owner/repo"),
            ("ssh://git@github.com/owner/repo.git", "owner/repo"),
            ("matthiasSchedel/ghx", "matthiasSchedel/ghx"),
        ] {
            assert_eq!(
                normalize_repo_slug(input).as_deref(),
                Ok(expected),
                "{input} must canonicalize to {expected}"
            );
        }
    }

    #[test]
    fn an_unusable_repository_is_rejected_rather_than_guessed() {
        // A wrong repository routes a pane into someone else's space, so every
        // ambiguous form must fail closed instead of resolving to a guess.
        for input in [
            "",
            "owner",
            "owner/repo/extra",
            "owner /repo",
            "/repo",
            "owner/",
            // A host that is not github.com must be refused outright. Reducing
            // a GitLab mirror to its `owner/repo` path would route it into the
            // GitHub-bound workspace, and the one-workspace-per-repository rule
            // would then make binding the two separately impossible.
            "git@gitlab.com:acme/tools.git",
            "https://bitbucket.org/acme/tools",
            "https://github.enterprise.corp/a/b",
            "ssh://hg@x/y/z",
            "https://gitlab.com/acme/tools.git",
        ] {
            assert!(
                normalize_repo_slug(input).is_err(),
                "{input:?} must be rejected"
            );
        }
    }

    #[test]
    fn a_declared_repository_outranks_a_cwd_derived_observation() {
        let mut state = PaneWorkContextState::default();
        state
            .replace_git_observation(PaneWorkContext {
                repo: Some("owner/shared-worktree".into()),
                ..PaneWorkContext::default()
            })
            .expect("observation");
        assert_eq!(
            state.effective().repo.as_deref(),
            Some("owner/shared-worktree"),
            "an observation resolves the repository when nothing better exists"
        );

        state
            .apply_manual_patch(PaneWorkContextPatch {
                repo: Some("owner/real".into()),
                ..PaneWorkContextPatch::default()
            })
            .expect("declaration");

        assert_eq!(state.effective().repo.as_deref(), Some("owner/real"));

        // A later observation must not win it back.
        state
            .replace_git_observation(PaneWorkContext {
                repo: Some("owner/shared-worktree".into()),
                ..PaneWorkContext::default()
            })
            .expect("observation");
        assert_eq!(state.effective().repo.as_deref(), Some("owner/real"));
    }

    /// `--clear repo` must actually stick on a pane whose manual tier holds a
    /// pull request. The implication that fills `repo` from a bound PR ran
    /// after `clear_fields`, so the clear was silently undone and the patch
    /// reported success having changed nothing.
    #[test]
    fn clearing_the_repository_sticks_on_a_pane_holding_a_pull_request() {
        let mut state = PaneWorkContextState::default();
        state
            .apply_manual_patch(PaneWorkContextPatch {
                pr_urls: Some(vec![
                    "https://github.com/matthias-scale/herdr/pull/106".into()
                ]),
                ..PaneWorkContextPatch::default()
            })
            .expect("bind a pull request");
        assert_eq!(
            state.effective().repo.as_deref(),
            Some("matthias-scale/herdr"),
            "a bound pull request implies its repository"
        );

        let changed = state
            .apply_manual_patch(PaneWorkContextPatch {
                clear_fields: vec![PaneWorkContextField::Repo],
                ..PaneWorkContextPatch::default()
            })
            .expect("clear the repository");

        assert!(changed, "clearing the repository must report a change");
        assert_eq!(
            state.effective().repo,
            None,
            "the cleared repository must not be re-derived from the pull request"
        );
        assert_eq!(
            state.effective().pr_urls,
            vec!["https://github.com/matthias-scale/herdr/pull/106".to_string()],
            "clearing the repository must leave the pull request binding alone"
        );
    }

    /// Having cleared the repository, the pane must be pinnable elsewhere.
    #[test]
    fn a_cleared_repository_can_be_repinned_without_dropping_the_pull_request() {
        let mut state = PaneWorkContextState::default();
        state
            .apply_manual_patch(PaneWorkContextPatch {
                pr_urls: Some(vec![
                    "https://github.com/matthias-scale/herdr/pull/106".into()
                ]),
                ..PaneWorkContextPatch::default()
            })
            .expect("bind a pull request");
        state
            .apply_manual_patch(PaneWorkContextPatch {
                clear_fields: vec![PaneWorkContextField::Repo],
                ..PaneWorkContextPatch::default()
            })
            .expect("clear");

        state
            .apply_manual_patch(PaneWorkContextPatch {
                repo: Some("matthiasSchedel/ghx".into()),
                ..PaneWorkContextPatch::default()
            })
            .expect("repin");

        assert_eq!(
            state.effective().repo.as_deref(),
            Some("matthiasSchedel/ghx")
        );
    }

    /// A patch touching an unrelated field must not re-derive the repository,
    /// or an incidental edit could relocate the pane.
    #[test]
    fn an_unrelated_patch_does_not_re_derive_the_repository() {
        let mut state = PaneWorkContextState::default();
        state
            .apply_manual_patch(PaneWorkContextPatch {
                pr_urls: Some(vec![
                    "https://github.com/matthias-scale/herdr/pull/106".into()
                ]),
                ..PaneWorkContextPatch::default()
            })
            .expect("bind");
        state
            .apply_manual_patch(PaneWorkContextPatch {
                clear_fields: vec![PaneWorkContextField::Repo],
                ..PaneWorkContextPatch::default()
            })
            .expect("clear");

        state
            .apply_manual_patch(PaneWorkContextPatch {
                work_title: Some("unrelated edit".into()),
                ..PaneWorkContextPatch::default()
            })
            .expect("title edit");

        assert_eq!(
            state.effective().repo,
            None,
            "an unrelated edit must not resurrect the cleared repository"
        );
    }

    #[test]
    fn an_explicitly_bound_pull_request_implies_its_repository() {
        let bound = PaneWorkContext {
            pr_urls: vec!["https://github.com/matthias-scale/herdr/pull/106".into()],
            role: Some(PaneWorkRole::Ship),
            ..PaneWorkContext::default()
        }
        .normalized_spawn_binding()
        .expect("spawn binding");

        assert_eq!(bound.repo.as_deref(), Some("matthias-scale/herdr"));
    }

    #[test]
    fn a_prose_scraped_pull_request_does_not_imply_a_repository() {
        // The hook tier is built from prompt text. A pull request named there
        // is a mention, not a binding, so it must not set the repository.
        let hook = hook_turn_context(
            Some("Look at herdr".into()),
            None,
            PaneWorkContext {
                pr_urls: vec!["https://github.com/matthias-scale/herdr/pull/106".into()],
                ..PaneWorkContext::default()
            },
        )
        .expect("hook context");

        assert_eq!(hook.repo, None);
    }

    #[test]
    fn spawn_binding_wins_over_git_inference() {
        let mut state = PaneWorkContextState::default();
        let binding = PaneWorkContext {
            ticket_ids: vec!["SCA-42".into()],
            pr_urls: vec!["https://github.com/o/r/pull/42".into()],
            branch: Some("feat/spawn-binding".into()),
            role: Some(PaneWorkRole::Ship),
            active_owner: true,
            ..PaneWorkContext::default()
        }
        .normalized_spawn_binding()
        .unwrap();
        state.replace_manual_normalized(binding);
        state
            .replace_git_observation(PaneWorkContext {
                ticket_ids: vec!["SCA-99".into()],
                pr_urls: vec!["https://github.com/o/r/pull/99".into()],
                branch: Some("guessed".into()),
                ..PaneWorkContext::default()
            })
            .unwrap();

        assert_eq!(state.effective().primary_ticket(), Some("SCA-42"));
        assert_eq!(
            state.effective().primary_pr(),
            Some("https://github.com/o/r/pull/42")
        );
        assert_eq!(
            state.effective().branch.as_deref(),
            Some("feat/spawn-binding")
        );
        assert_eq!(state.effective().role, Some(PaneWorkRole::Ship));
        assert!(state.effective().active_owner);
    }

    #[test]
    fn work_role_and_owner_round_trip() {
        for role in [
            PaneWorkRole::Baseline,
            PaneWorkRole::Repair,
            PaneWorkRole::Ship,
            PaneWorkRole::Review,
        ] {
            let context = PaneWorkContext {
                pr_urls: vec!["https://github.com/o/r/pull/42".into()],
                role: Some(role),
                active_owner: true,
                ..PaneWorkContext::default()
            };
            let json = serde_json::to_string(&context).unwrap();
            let restored: PaneWorkContext = serde_json::from_str(&json).unwrap();
            assert_eq!(restored, context);
        }
    }

    #[test]
    fn ac1_patch_is_atomic_and_omitted_fields_are_untouched() {
        let mut state = PaneWorkContextState::from_restored_with_tiers(
            PaneWorkContext::default(),
            Some(PaneWorkContextTiers {
                manual: PaneWorkContext {
                    repo: None,
                    ticket_ids: vec!["MAT-1".into()],
                    pr_urls: vec!["https://github.com/o/r/pull/2".into()],
                    preview_urls: Vec::new(),
                    missive_urls: Vec::new(),
                    branch: Some("main".into()),
                    work_title: Some("Initial".into()),
                    session_name: None,
                    role: None,
                    active_owner: false,
                },
                ..PaneWorkContextTiers::default()
            }),
        )
        .unwrap();
        let before = state.clone();
        let error = state
            .apply_manual_patch(PaneWorkContextPatch {
                ticket_ids: Some(vec!["SCA-2".into()]),
                pr_urls: Some(vec!["https://evil.test/o/r/pull/2".into()]),
                ..PaneWorkContextPatch::default()
            })
            .unwrap_err();
        assert!(error.contains("pull request URL"));
        assert_eq!(state, before);

        assert!(state
            .apply_manual_patch(PaneWorkContextPatch {
                ticket_ids: Some(vec!["SCA-2".into()]),
                ..PaneWorkContextPatch::default()
            })
            .unwrap());
        assert_eq!(state.effective().ticket_ids, vec!["SCA-2"]);
        assert_eq!(
            state.effective().pr_urls,
            vec!["https://github.com/o/r/pull/2"]
        );
        assert_eq!(state.effective().branch.as_deref(), Some("main"));
    }

    #[test]
    fn ac1_restored_tiers_keep_hook_and_git_provenance_replaceable() {
        // Seed hook-only state, persist tiers, restore, then a NEW hook turn
        // must fully replace the restored hook value.
        let mut live = PaneWorkContextState::default();
        live.replace_hook_turn(PaneWorkContext {
            ticket_ids: vec!["MAT-1".into()],
            work_title: Some("Old hook title".into()),
            ..PaneWorkContext::default()
        })
        .unwrap();
        let mut restored = PaneWorkContextState::from_restored_with_tiers(
            live.effective().clone(),
            Some(live.snapshot_tiers()),
        )
        .unwrap();
        assert_eq!(restored.effective(), live.effective());
        restored
            .replace_hook_turn(PaneWorkContext {
                ticket_ids: vec!["MAT-2".into()],
                work_title: Some("New hook title".into()),
                ..PaneWorkContext::default()
            })
            .unwrap();
        assert_eq!(restored.effective().ticket_ids, vec!["MAT-2"]);
        assert_eq!(
            restored.effective().work_title.as_deref(),
            Some("New hook title")
        );

        // Same for a git-only branch observation.
        let mut live = PaneWorkContextState::default();
        live.replace_git_observation(PaneWorkContext {
            branch: Some("old-branch".into()),
            ..PaneWorkContext::default()
        })
        .unwrap();
        let mut restored = PaneWorkContextState::from_restored_with_tiers(
            live.effective().clone(),
            Some(live.snapshot_tiers()),
        )
        .unwrap();
        restored
            .replace_git_observation(PaneWorkContext {
                branch: Some("new-branch".into()),
                ..PaneWorkContext::default()
            })
            .unwrap();
        assert_eq!(restored.effective().branch.as_deref(), Some("new-branch"));
    }

    #[test]
    fn ac25_restored_preview_sources_are_bounded_to_hook_and_git() {
        let preview_urls = |prefix: &str, count: usize| {
            (0..count)
                .map(|index| format!("https://{prefix}-{index}.vercel.app"))
                .collect::<Vec<_>>()
        };
        let state = PaneWorkContextState::from_restored_with_tiers(
            PaneWorkContext::default(),
            Some(PaneWorkContextTiers {
                manual: PaneWorkContext {
                    preview_urls: preview_urls("manual", MAX_PREVIEW_URLS),
                    ..PaneWorkContext::default()
                },
                hook_turn: PaneWorkContext {
                    preview_urls: preview_urls("hook", 2),
                    ..PaneWorkContext::default()
                },
                git_observation: PaneWorkContext {
                    preview_urls: preview_urls("git", MAX_PREVIEW_URLS),
                    ..PaneWorkContext::default()
                },
                restored_fallback: PaneWorkContext {
                    preview_urls: preview_urls("fallback", MAX_PREVIEW_URLS),
                    ..PaneWorkContext::default()
                },
            }),
        )
        .unwrap();

        let tiers = state.snapshot_tiers();
        assert!(tiers.manual.preview_urls.is_empty());
        assert_eq!(
            tiers.git_observation.preview_urls,
            preview_urls("git", MAX_PREVIEW_URLS)
        );
        assert_eq!(
            state.effective().preview_urls,
            preview_urls("hook", 2)
                .into_iter()
                .chain(preview_urls("git", MAX_PREVIEW_URLS - 2))
                .collect::<Vec<_>>()
        );
        assert_eq!(state.effective().preview_urls.len(), MAX_PREVIEW_URLS);
    }

    #[test]
    fn ac1_legacy_flat_restore_is_fallback_not_manual_pin() {
        // A legacy flat snapshot has unknown provenance: it must load intact
        // but be superseded by later live hook/git observations.
        let mut restored = PaneWorkContextState::from_restored(PaneWorkContext {
            repo: None,
            ticket_ids: vec!["MAT-1".into()],
            pr_urls: vec!["https://github.com/o/r/pull/2".into()],
            preview_urls: Vec::new(),
            missive_urls: Vec::new(),
            branch: Some("old-branch".into()),
            work_title: Some("Old title".into()),
            session_name: None,
            role: None,
            active_owner: false,
        })
        .unwrap();
        assert_eq!(restored.effective().ticket_ids, vec!["MAT-1"]);
        assert_eq!(restored.effective().branch.as_deref(), Some("old-branch"));

        restored
            .replace_git_observation(PaneWorkContext {
                branch: Some("new-branch".into()),
                ..PaneWorkContext::default()
            })
            .unwrap();
        assert_eq!(restored.effective().branch.as_deref(), Some("new-branch"));
        assert!(restored.effective().ticket_ids.is_empty());
        assert!(restored.effective().pr_urls.is_empty());
        assert!(restored.effective().work_title.is_none());

        restored
            .replace_hook_turn(PaneWorkContext {
                ticket_ids: vec!["SCA-9".into()],
                work_title: Some("New title".into()),
                ..PaneWorkContext::default()
            })
            .unwrap();
        assert_eq!(restored.effective().ticket_ids, vec!["SCA-9"]);
        assert!(restored.effective().pr_urls.is_empty());
        assert_eq!(
            restored.effective().work_title.as_deref(),
            Some("New title")
        );
        assert_eq!(restored.effective().branch.as_deref(), Some("new-branch"));
    }

    #[test]
    fn ac1_restored_manual_tier_still_wins_over_later_observations() {
        let mut live = PaneWorkContextState::default();
        live.apply_manual_patch(PaneWorkContextPatch {
            branch: Some("pinned-branch".into()),
            work_title: Some("Pinned title".into()),
            ..PaneWorkContextPatch::default()
        })
        .unwrap();
        let mut restored = PaneWorkContextState::from_restored_with_tiers(
            live.effective().clone(),
            Some(live.snapshot_tiers()),
        )
        .unwrap();
        restored
            .replace_hook_turn(PaneWorkContext {
                work_title: Some("Hook title".into()),
                ..PaneWorkContext::default()
            })
            .unwrap();
        restored
            .replace_git_observation(PaneWorkContext {
                branch: Some("git-branch".into()),
                ..PaneWorkContext::default()
            })
            .unwrap();
        assert_eq!(
            restored.effective().work_title.as_deref(),
            Some("Pinned title")
        );
        assert_eq!(
            restored.effective().branch.as_deref(),
            Some("pinned-branch")
        );
    }

    #[test]
    fn ac1_hook_clear_preserves_manual_git_and_restored_fallback_tiers() {
        let mut state = PaneWorkContextState::from_restored_with_tiers(
            PaneWorkContext::default(),
            Some(PaneWorkContextTiers {
                manual: PaneWorkContext {
                    work_title: Some("Pinned title".into()),
                    ..PaneWorkContext::default()
                },
                hook_turn: PaneWorkContext {
                    ticket_ids: vec!["MAT-1".into()],
                    work_title: Some("Hook title".into()),
                    ..PaneWorkContext::default()
                },
                git_observation: PaneWorkContext {
                    branch: Some("git-branch".into()),
                    ..PaneWorkContext::default()
                },
                restored_fallback: PaneWorkContext {
                    pr_urls: vec!["https://github.com/o/r/pull/2".into()],
                    preview_urls: vec!["https://fallback.vercel.app".into()],
                    ..PaneWorkContext::default()
                },
            }),
        )
        .unwrap();

        assert!(state.clear_hook_turn());
        assert!(state.effective().ticket_ids.is_empty());
        assert_eq!(state.effective().branch.as_deref(), Some("git-branch"));
        assert_eq!(
            state.effective().work_title.as_deref(),
            Some("Pinned title")
        );
        assert_eq!(
            state.effective().pr_urls,
            vec!["https://github.com/o/r/pull/2"]
        );
        assert_eq!(
            state.effective().preview_urls,
            vec!["https://fallback.vercel.app"]
        );
        assert!(!state.clear_hook_turn());
    }

    #[test]
    fn ac1_set_and_clear_collision_is_rejected_without_mutation() {
        let mut state = PaneWorkContextState::default();
        let before = state.clone();
        assert!(state
            .apply_manual_patch(PaneWorkContextPatch {
                branch: Some("feat/x".into()),
                clear_fields: vec![PaneWorkContextField::Branch],
                ..PaneWorkContextPatch::default()
            })
            .is_err());
        assert_eq!(state, before);
    }
}
