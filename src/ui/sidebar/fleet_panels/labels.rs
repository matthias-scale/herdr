use std::collections::{BTreeMap, BTreeSet};

use super::{account_key, display_width, AppState, QuotaProvider};

const LETTERS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
const TAGS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// The companion Herdr Icons font supplies every character in this alphabet.
/// Default installations retain readable ordinary text until all clients have it.
pub(super) fn render(text: &str, app: &AppState) -> String {
    if !app.nerd_font || !app.subscript_glyphs {
        return text.to_string();
    }
    text.chars()
        .map(|c| {
            LETTERS
                .find(c)
                .and_then(|index| char::from_u32(0xF6100 + index as u32))
                .unwrap_or(c)
        })
        .collect()
}

fn inventory(app: &AppState) -> BTreeMap<(QuotaProvider, String), String> {
    let mut entries = BTreeMap::new();
    let has_local = app.fleet_snapshot.observations.iter().any(|observation| {
        app.fleet_snapshot
            .hosts
            .iter()
            .any(|host| host.local && host.name == observation.host)
    });
    let sources = app
        .fleet_snapshot
        .observations
        .iter()
        .map(|o| (o.host.as_str(), o.usage.accounts.as_slice()));
    let local = (!has_local).then_some((
        app.agent_host_name.as_str(),
        app.provider_usage.accounts.as_slice(),
    ));
    for (host, accounts) in sources.chain(local) {
        for account in accounts {
            let key = account_key(host, account);
            let label = account.label.clone();
            entries
                .entry(key)
                .and_modify(|old: &mut String| {
                    if label < *old {
                        *old = label.clone();
                    }
                })
                .or_insert(label);
        }
    }
    entries
}

fn abbreviations(entries: &[(String, String)]) -> Vec<String> {
    if entries.len() == 1 {
        return vec![String::new()];
    }
    let names: Vec<String> = entries
        .iter()
        .map(|(_, name)| {
            let clean: String = name
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .map(|c| c.to_ascii_uppercase())
                .collect();
            if clean.is_empty() {
                "A".into()
            } else {
                clean
            }
        })
        .collect();
    let mut used = BTreeSet::new();
    names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let first = name.chars().next().unwrap();
            let unique = names
                .iter()
                .filter(|other| other.starts_with(first))
                .count()
                == 1;
            let readable = unique
                .then(|| first.to_string())
                .into_iter()
                .chain(name.chars().skip(1).map(|c| format!("{first}{c}")));
            // Canonical identity order makes collision resolution independent of observation order.
            // Reserve the complete two-character namespace before considering a longer label.
            let pairs = TAGS
                .chars()
                .flat_map(|left| TAGS.chars().map(move |right| format!("{left}{right}")));
            readable
                .chain(pairs)
                .find(|tag| used.insert(tag.clone()))
                .unwrap_or_else(|| {
                    // A fleet beyond the finite 1–2 character namespace must retain identity, not collide.
                    format!("{}", index + 1)
                })
        })
        .collect()
}

pub(super) struct CompactAccount {
    pub(super) provider: QuotaProvider,
    pub(super) tag: String,
    pub(super) clearance: &'static str,
    pub(super) row: usize,
    pub(super) gap: usize,
}

pub(super) fn plan(app: &AppState, width: u16) -> Vec<CompactAccount> {
    let entries = inventory(app);
    let mut output = Vec::new();
    for provider in [
        QuotaProvider::Claude,
        QuotaProvider::Codex,
        QuotaProvider::Kimi,
        QuotaProvider::Agy,
        QuotaProvider::OpenCode,
    ] {
        let group: Vec<_> = entries
            .iter()
            .filter(|((p, _), _)| *p == provider)
            .map(|((_, key), label)| (key.clone(), label.clone()))
            .collect();
        for tag in abbreviations(&group) {
            let clearance = if !tag.is_empty()
                && app.nerd_font
                && matches!(provider, QuotaProvider::Kimi | QuotaProvider::OpenCode)
            {
                " "
            } else {
                ""
            };
            output.push(CompactAccount {
                provider,
                tag: render(&tag, app),
                clearance,
                row: 0,
                gap: 0,
            });
        }
    }
    let widths: Vec<_> = output
        .iter()
        .map(|entry| {
            display_width(crate::ui::icons::themed_usage_label(
                entry.provider,
                app.nerd_font,
                &app.palette,
            )) + display_width(&entry.tag)
                + entry.clearance.len()
                + 2
        })
        .collect();
    let mut row = 0;
    let mut used = display_width(if app.nerd_font {
        "▸ 󰓅"
    } else {
        "▸ Usage"
    });
    let mut previous = None;
    for index in 0..output.len() {
        let provider = output[index].provider;
        let new_group = previous != Some(provider);
        let mut gap = if previous.is_some() && new_group {
            2
        } else {
            1
        };
        if new_group {
            let end = (index..output.len())
                .find(|&i| output[i].provider != provider)
                .unwrap_or(output.len());
            let group_width = widths[index..end].iter().sum::<usize>() + end - index - 1;
            if group_width <= usize::from(width).saturating_sub(4)
                && used + gap + group_width > usize::from(width)
            {
                row += 1;
                used = 4;
                gap = 0;
            }
        }
        if used + gap + widths[index] > usize::from(width) {
            row += 1;
            used = 4;
            gap = 0;
        }
        output[index].row = row;
        output[index].gap = gap;
        used += gap + widths[index];
        previous = Some(provider);
    }
    output
}

pub(super) fn machine_aliases(names: &[String]) -> BTreeMap<String, char> {
    let mut names = names.to_vec();
    names.sort();
    names.dedup();
    let mut used = BTreeSet::new();
    let mut aliases = BTreeMap::new();
    for name in &names {
        if let Some(code) =
            crate::config::FleetConfig::device_code(name).filter(char::is_ascii_alphanumeric)
        {
            if names
                .iter()
                .filter(|other| crate::config::FleetConfig::device_code(other) == Some(code))
                .count()
                == 1
            {
                used.insert(code);
                aliases.insert(name.clone(), code);
            }
        }
    }
    for name in names {
        if aliases.contains_key(&name) {
            continue;
        }
        let candidates = name
            .chars()
            .rev()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .chain("abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ".chars());
        if let Some(code) = candidates.into_iter().find(|c| used.insert(*c)) {
            aliases.insert(name, code);
        }
    }
    aliases
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readable_abbreviations_resolve_collisions_without_row_numbers() {
        let labels = vec![
            ("a".into(), "alder".into()),
            ("b".into(), "alpha".into()),
            ("c".into(), "atlas".into()),
            ("d".into(), "birch".into()),
        ];
        let tags = abbreviations(&labels);
        assert_eq!(tags, ["AL", "AP", "AT", "B"]);
        assert_eq!(tags.iter().collect::<BTreeSet<_>>().len(), 4);
        assert!(tags.iter().all(|tag| tag.len() <= 2));
        assert_eq!(abbreviations(&[("x".into(), "only".into())]), [""]);
    }

    #[test]
    fn machine_aliases_preserve_unique_device_codes_and_disambiguate_collisions() {
        let names: Vec<String> = ["ub1", "ub2", "om1", "lab-a", "lab-b", "lab-c"]
            .map(str::to_string)
            .into();
        let aliases = machine_aliases(&names);
        assert_eq!(aliases["ub1"], '1');
        assert_eq!(aliases["ub2"], '2');
        assert_eq!(aliases["om1"], 'o');
        assert_eq!(aliases["lab-a"], 'a');
        assert_eq!(aliases["lab-b"], 'b');
        assert_eq!(aliases["lab-c"], 'c');
        assert_eq!(aliases.values().collect::<BTreeSet<_>>().len(), names.len());
        assert_eq!(
            machine_aliases(&names.into_iter().rev().collect::<Vec<_>>()),
            aliases
        );
    }

    #[test]
    fn plan_is_deterministic_groups_providers_and_keeps_whole_accounts() {
        let mut app = AppState::test_new();
        app.provider_usage.accounts = [
            (QuotaProvider::Codex, "beta"),
            (QuotaProvider::Claude, "atlas"),
            (QuotaProvider::Claude, "alder"),
            (QuotaProvider::Codex, "cedar"),
        ]
        .map(|(provider, label)| super::super::ProviderAccountUsage {
            provider,
            profile_id: label.into(),
            label: label.into(),
            usage: Default::default(),
        })
        .into();
        let before: Vec<_> = plan(&app, 18)
            .into_iter()
            .map(|entry| (entry.provider, entry.tag, entry.row, entry.gap))
            .collect();
        app.provider_usage.accounts.reverse();
        let after: Vec<_> = plan(&app, 18)
            .into_iter()
            .map(|entry| (entry.provider, entry.tag, entry.row, entry.gap))
            .collect();
        assert_eq!(before, after);
        assert_eq!(
            before.iter().map(|entry| entry.0).collect::<Vec<_>>(),
            [
                QuotaProvider::Claude,
                QuotaProvider::Claude,
                QuotaProvider::Codex,
                QuotaProvider::Codex
            ]
        );
        assert_eq!(before[2].2, before[3].2); // Codex fits together on a fresh row.
        assert!(before[2].2 > before[1].2);
        assert!(before.iter().all(|entry| entry.1.is_ascii())); // Compatibility is safe by default.
        app.subscript_glyphs = true;
        let lowered: Vec<_> = plan(&app, 18).into_iter().map(|entry| entry.tag).collect();
        assert!(lowered.iter().all(|tag| !tag.is_ascii()));
        app.nerd_font = false;
        assert!(plan(&app, 18).iter().all(|entry| entry.tag.is_ascii()));
    }
}
