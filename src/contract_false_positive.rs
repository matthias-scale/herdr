use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

#[derive(Debug, Serialize)]
pub(crate) struct ContractFalsePositive<'a> {
    pub ts: String,
    pub pane_id: &'a str,
    pub session_id: Option<&'a str>,
    pub contract: &'a str,
    pub age_s: u64,
}

impl<'a> ContractFalsePositive<'a> {
    pub(crate) fn now(
        pane_id: &'a str,
        session_id: Option<&'a str>,
        contract: &'a str,
        age_s: u64,
    ) -> Self {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default();
        Self {
            ts: format_utc_timestamp(seconds),
            pane_id,
            session_id,
            contract,
            age_s,
        }
    }
}

pub(crate) fn append(
    record: &ContractFalsePositive<'_>,
    path_override: Option<&Path>,
) -> io::Result<()> {
    let path = match path_override {
        Some(path) => path.to_path_buf(),
        None => state_path()?,
    };
    append_to_path(record, &path)
}

fn state_path() -> io::Result<PathBuf> {
    state_path_from(std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
}

fn state_path_from(
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
) -> io::Result<PathBuf> {
    let base = if let Some(path) = xdg_state_home.filter(|path| !path.is_empty()) {
        PathBuf::from(path)
    } else if let Some(path) = home.filter(|path| !path.is_empty()) {
        PathBuf::from(path).join(".local/state")
    } else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "XDG_STATE_HOME and HOME are unavailable",
        ));
    };
    Ok(base.join("herdr/contract-false-positives.jsonl"))
}

fn append_to_path(record: &ContractFalsePositive<'_>, path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_vec(record).map_err(io::Error::other)?;
    line.push(b'\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&line)
}

fn format_utc_timestamp(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let seconds_of_day = seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let hour = seconds_of_day / 3_600;
    let minute = seconds_of_day % 3_600 / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_path_uses_xdg_then_home_fallback() {
        assert_eq!(
            state_path_from(Some("/state".into()), Some("/home/test".into())).unwrap(),
            PathBuf::from("/state/herdr/contract-false-positives.jsonl")
        );
        assert_eq!(
            state_path_from(None, Some("/home/test".into())).unwrap(),
            PathBuf::from("/home/test/.local/state/herdr/contract-false-positives.jsonl")
        );
        assert!(state_path_from(None, None).is_err());
    }

    #[test]
    fn appends_one_complete_json_object_per_call() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "herdr-contract-false-positive-{}-{unique}",
            std::process::id()
        ));
        let path = dir.join("nested/contract-false-positives.jsonl");
        let first = ContractFalsePositive {
            ts: "2026-08-29T10:00:00Z".into(),
            pane_id: "w1:p1",
            session_id: Some("session-1"),
            contract: "tests pass",
            age_s: 4,
        };
        let second = ContractFalsePositive {
            ts: "2026-08-29T10:00:01Z".into(),
            pane_id: "w1:p1",
            session_id: None,
            contract: "tests pass",
            age_s: 5,
        };

        append_to_path(&first, &path).unwrap();
        append_to_path(&second, &path).unwrap();

        let lines = std::fs::read_to_string(path).unwrap();
        let parsed = lines
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0]["session_id"], "session-1");
        assert!(parsed[1]["session_id"].is_null());
        assert_eq!(parsed[1]["age_s"], 5);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn timestamp_is_rfc3339_utc() {
        assert_eq!(format_utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc_timestamp(86_400), "1970-01-02T00:00:00Z");
    }
}
