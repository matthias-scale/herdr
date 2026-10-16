use std::collections::{BTreeMap, BTreeSet};

use super::AppState;

const LETTERS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

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
}
