use std::collections::HashSet;
use std::sync::OnceLock;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default)]
pub struct PaneWorkContext {
    pub ticket_ids: Vec<String>,
    pub pr_urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work_title: Option<String>,
}

impl PaneWorkContext {
    pub(crate) fn normalized(self) -> Result<Self, String> {
        Ok(Self {
            ticket_ids: normalize_ticket_ids(self.ticket_ids)?,
            pr_urls: normalize_pr_urls(self.pr_urls)?,
            branch: normalize_optional_text("branch", self.branch)?,
            work_title: normalize_optional_text("work title", self.work_title)?,
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

    #[allow(dead_code)]
    pub fn primary_action_url(&self) -> Option<String> {
        self.primary_ticket()
            .and_then(linear_ticket_url)
            .or_else(|| self.primary_pr().map(str::to_string))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaneWorkContextField {
    TicketIds,
    PrUrls,
    Branch,
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
    pub work_title: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clear_fields: Vec<PaneWorkContextField>,
}

impl PaneWorkContextPatch {
    pub fn is_empty(&self) -> bool {
        self.ticket_ids.is_none()
            && self.pr_urls.is_none()
            && self.branch.is_none()
            && self.work_title.is_none()
            && self.clear_fields.is_empty()
    }

    fn validate_collisions(&self) -> Result<(), String> {
        let cleared: HashSet<_> = self.clear_fields.iter().copied().collect();
        for (field, supplied) in [
            (PaneWorkContextField::TicketIds, self.ticket_ids.is_some()),
            (PaneWorkContextField::PrUrls, self.pr_urls.is_some()),
            (PaneWorkContextField::Branch, self.branch.is_some()),
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
            Self::WorkTitle => "work_title",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaneWorkContextState {
    manual: PaneWorkContext,
    hook_turn: PaneWorkContext,
    git_observation: PaneWorkContext,
    effective: PaneWorkContext,
}

impl PaneWorkContextState {
    pub fn from_restored(context: PaneWorkContext) -> Result<Self, String> {
        let manual = context.normalized()?;
        let effective = manual.clone();
        Ok(Self {
            manual,
            effective,
            ..Self::default()
        })
    }

    pub fn effective(&self) -> &PaneWorkContext {
        &self.effective
    }

    pub(crate) fn persisted(&self) -> &PaneWorkContext {
        &self.manual
    }

    pub fn apply_manual_patch(&mut self, patch: PaneWorkContextPatch) -> Result<bool, String> {
        if patch.is_empty() {
            return Err("missing work-context field to set or clear".into());
        }
        patch.validate_collisions()?;

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
        if let Some(work_title) = patch.work_title {
            candidate.work_title = Some(work_title);
        }
        for field in patch.clear_fields {
            match field {
                PaneWorkContextField::TicketIds => candidate.ticket_ids.clear(),
                PaneWorkContextField::PrUrls => candidate.pr_urls.clear(),
                PaneWorkContextField::Branch => candidate.branch = None,
                PaneWorkContextField::WorkTitle => candidate.work_title = None,
            }
        }
        let candidate = candidate.normalized()?;
        if candidate == self.manual {
            return Ok(false);
        }
        self.manual = candidate;
        self.recompute();
        Ok(true)
    }

    // PR-1 defines source-tier replacement before hook and git producers land.
    #[allow(dead_code)]
    pub fn replace_hook_turn(&mut self, context: PaneWorkContext) -> Result<bool, String> {
        let context = context.normalized()?;
        if context == self.hook_turn {
            return Ok(false);
        }
        self.hook_turn = context;
        self.recompute();
        Ok(true)
    }

    /// Drops the hook tier when the agent session that authorized it ends or
    /// is replaced; the manual and git tiers are preserved.
    pub fn clear_hook_turn(&mut self) -> bool {
        if self.hook_turn == PaneWorkContext::default() {
            return false;
        }
        self.hook_turn = PaneWorkContext::default();
        self.recompute();
        true
    }

    #[allow(dead_code)]
    pub fn replace_git_observation(&mut self, context: PaneWorkContext) -> Result<bool, String> {
        let context = context.normalized()?;
        if context == self.git_observation {
            return Ok(false);
        }
        self.git_observation = context;
        self.recompute();
        Ok(true)
    }

    fn recompute(&mut self) {
        self.effective = PaneWorkContext {
            ticket_ids: stable_merge([
                &self.manual.ticket_ids,
                &self.hook_turn.ticket_ids,
                &self.git_observation.ticket_ids,
            ]),
            pr_urls: stable_merge([
                &self.manual.pr_urls,
                &self.hook_turn.pr_urls,
                &self.git_observation.pr_urls,
            ]),
            branch: first_present([
                self.manual.branch.as_ref(),
                self.hook_turn.branch.as_ref(),
                self.git_observation.branch.as_ref(),
            ]),
            work_title: first_present([
                self.manual.work_title.as_ref(),
                self.hook_turn.work_title.as_ref(),
                self.git_observation.work_title.as_ref(),
            ]),
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

pub fn extract_pr_urls(text: &str) -> Vec<String> {
    const PREFIX: &str = "https://github.com/";

    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    for (start, _) in text.match_indices(PREFIX) {
        if start > 0 && is_ascii_token_char(text.as_bytes()[start - 1]) {
            continue;
        }
        let candidate = text[start..]
            .split_ascii_whitespace()
            .next()
            .unwrap_or_default();
        let Ok(url) = normalize_pr_url(candidate) else {
            continue;
        };
        if seen.insert(url.clone()) {
            urls.push(url);
        }
    }
    urls
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
        branch: None,
        work_title,
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
    fn ac2_ac3_prompt_refs_accept_boundaries_and_punctuation_but_reject_malicious_urls() {
        let text = "MAT-7, sca-9; FORMAT-12 XMAT-3 \
            (https://github.com/scalable-so/herdr/pull/0042). \
            https://user@github.com/evil/repo/pull/2#fragment \
            http://github.com/evil/repo/pull/3";

        assert_eq!(extract_ticket_ids(text), vec!["MAT-7", "SCA-9"]);
        assert_eq!(
            extract_pr_urls(text),
            vec!["https://github.com/scalable-so/herdr/pull/42"]
        );
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
            state.effective().work_title.as_deref(),
            Some("Manual title")
        );

        state
            .replace_hook_turn(PaneWorkContext {
                ticket_ids: vec!["MAT-9".into()],
                ..PaneWorkContext::default()
            })
            .unwrap();
        assert_eq!(
            state.effective().ticket_ids,
            vec!["MAT-2", "SCA-1", "MAT-9", "SCA-3"]
        );
    }

    #[test]
    fn ac1_patch_is_atomic_and_omitted_fields_are_untouched() {
        let mut state = PaneWorkContextState::from_restored(PaneWorkContext {
            ticket_ids: vec!["MAT-1".into()],
            pr_urls: vec!["https://github.com/o/r/pull/2".into()],
            branch: Some("main".into()),
            work_title: Some("Initial".into()),
        })
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
