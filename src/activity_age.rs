use std::time::{Duration, Instant};

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 60 * SECONDS_PER_MINUTE;
const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;
const MAX_DISPLAY_DAYS: u64 = 999;

pub(crate) fn compact_label(observed_at: Option<Instant>, now: Instant) -> String {
    let Some(observed_at) = observed_at else {
        return "--".into();
    };
    let seconds = now.saturating_duration_since(observed_at).as_secs();
    if seconds < SECONDS_PER_MINUTE {
        format!("{seconds}s")
    } else if seconds < SECONDS_PER_HOUR {
        format!("{}m", seconds / SECONDS_PER_MINUTE)
    } else if seconds < SECONDS_PER_DAY {
        format!("{}h", seconds / SECONDS_PER_HOUR)
    } else {
        format!("{}d", (seconds / SECONDS_PER_DAY).min(MAX_DISPLAY_DAYS))
    }
}

pub(crate) fn next_change_at(observed_at: Option<Instant>, now: Instant) -> Option<Instant> {
    let observed_at = observed_at?;
    let seconds = now.saturating_duration_since(observed_at).as_secs();
    let next_elapsed = if seconds < SECONDS_PER_MINUTE {
        seconds.saturating_add(1)
    } else if seconds < SECONDS_PER_HOUR {
        seconds
            .checked_div(SECONDS_PER_MINUTE)?
            .saturating_add(1)
            .saturating_mul(SECONDS_PER_MINUTE)
    } else if seconds < SECONDS_PER_DAY {
        seconds
            .checked_div(SECONDS_PER_HOUR)?
            .saturating_add(1)
            .saturating_mul(SECONDS_PER_HOUR)
    } else if seconds / SECONDS_PER_DAY < MAX_DISPLAY_DAYS {
        seconds
            .checked_div(SECONDS_PER_DAY)?
            .saturating_add(1)
            .saturating_mul(SECONDS_PER_DAY)
    } else {
        return None;
    };
    observed_at.checked_add(Duration::from_secs(next_elapsed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_age_uses_stable_unit_buckets() {
        let started = Instant::now();
        for (elapsed, expected) in [
            (Duration::ZERO, "0s"),
            (Duration::from_secs(59), "59s"),
            (Duration::from_secs(60), "1m"),
            (Duration::from_secs(3_599), "59m"),
            (Duration::from_secs(3_600), "1h"),
            (Duration::from_secs(86_400), "1d"),
        ] {
            assert_eq!(compact_label(Some(started), started + elapsed), expected);
        }
        assert_eq!(compact_label(None, started), "--");
    }

    #[test]
    fn next_change_matches_the_visible_bucket_boundary() {
        let started = Instant::now();
        assert_eq!(
            next_change_at(Some(started), started + Duration::from_secs(7)),
            Some(started + Duration::from_secs(8))
        );
        assert_eq!(
            next_change_at(Some(started), started + Duration::from_secs(75)),
            Some(started + Duration::from_secs(120))
        );
        assert_eq!(
            next_change_at(Some(started), started + Duration::from_secs(7_200)),
            Some(started + Duration::from_secs(10_800))
        );
    }
}
