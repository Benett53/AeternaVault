//! Which old backups a retention policy would remove. Read-only; the removal
//! itself happens in [`super::manage`].
//!
//! Similar to common backup tools: keep the newest *n* backups, plus the
//! newest backup of each of the last days, weeks and months. Only backups of
//! the given computer are considered, and only those whose details are known —
//! a locked encrypted backup is never removed.

use std::collections::HashSet;

use chrono::{DateTime, Datelike, Local, Utc};

use super::snapshots::SnapshotInfo;
use crate::config::Retention;

/// Qualified ids of the backups the policy does not keep, newest first.
pub fn to_remove(snapshots: &[SnapshotInfo], policy: &Retention, computer: &str) -> Vec<String> {
    let mut own: Vec<&SnapshotInfo> = snapshots
        .iter()
        .filter(|s| s.computer.eq_ignore_ascii_case(computer) && s.header.is_some())
        .collect();
    own.sort_by_key(|s| std::cmp::Reverse(s.header.as_ref().map(|h| h.started_at)));

    let usable: Vec<&SnapshotInfo> = own
        .iter()
        .copied()
        .filter(|s| s.is_usable_base() && !s.needs_unlock())
        .collect();
    let Some(newest) = usable.first() else {
        // Without a complete backup, nothing is removed.
        return Vec::new();
    };
    let newest_start = newest.header.as_ref().map(|h| h.started_at);

    let dated: Vec<(String, DateTime<Utc>)> = usable
        .iter()
        .filter_map(|s| Some((s.qualified_id(), s.header.as_ref()?.started_at)))
        .collect();
    let keep = kept(&dated, policy);

    own.into_iter()
        .filter(|s| !keep.contains(&s.qualified_id()))
        .filter(|s| {
            // Unfinished or locked backups newer than the newest complete one stay:
            // they may still be running or simply need the passphrase.
            let start = s.header.as_ref().map(|h| h.started_at);
            (s.is_usable_base() && !s.needs_unlock()) || start < newest_start
        })
        .filter(|s| !s.needs_unlock())
        .map(SnapshotInfo::qualified_id)
        .collect()
}

/// Which of `backups` (complete ones, newest first) the policy keeps.
fn kept(backups: &[(String, DateTime<Utc>)], policy: &Retention) -> HashSet<String> {
    let mut keep: HashSet<String> = HashSet::new();
    if let Some((newest, _)) = backups.first() {
        keep.insert(newest.clone());
    }
    for (id, _) in backups.iter().take(policy.keep_last as usize) {
        keep.insert(id.clone());
    }
    let mut keep_per = |limit: u32, bucket: &dyn Fn(DateTime<Local>) -> (i32, u32)| {
        let mut seen = HashSet::new();
        for (id, time) in backups {
            if seen.len() >= limit as usize {
                break;
            }
            if seen.insert(bucket(time.with_timezone(&Local))) {
                keep.insert(id.clone());
            }
        }
    };
    keep_per(policy.keep_daily, &|t| (t.year(), t.ordinal()));
    keep_per(policy.keep_weekly, &|t| {
        let week = t.iso_week();
        (week.year(), week.week())
    });
    keep_per(policy.keep_monthly, &|t| (t.year(), t.month()));
    keep
}

/// When an existing backup will be removed if backups keep being made at `future` times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// With the next backup (the rules already do not keep it).
    NextBackup,
    /// After the backup made at this time.
    After(DateTime<Utc>),
    /// Still kept after the last of the assumed future backups.
    NotWithin,
    /// Not handled by the rules (other computer, unfinished or locked).
    NotAffected,
}

/// For every backup in `snapshots` (same order): when the rules would remove it,
/// assuming a backup is made at each of `future` (sorted, oldest first).
pub fn forecast(
    snapshots: &[SnapshotInfo],
    policy: &Retention,
    computer: &str,
    future: &[DateTime<Utc>],
) -> Vec<(String, Removal)> {
    let now_removed: HashSet<String> = to_remove(snapshots, policy, computer).into_iter().collect();
    let mut usable: Vec<(String, DateTime<Utc>)> = snapshots
        .iter()
        .filter(|s| s.computer.eq_ignore_ascii_case(computer))
        .filter(|s| s.is_usable_base() && !s.needs_unlock())
        .filter(|s| !now_removed.contains(&s.qualified_id()))
        .filter_map(|s| Some((s.qualified_id(), s.header.as_ref()?.started_at)))
        .collect();
    usable.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
    let existing: HashSet<String> = usable.iter().map(|(id, _)| id.clone()).collect();

    let mut removed_at: std::collections::HashMap<String, DateTime<Utc>> = Default::default();
    for (n, at) in future.iter().enumerate() {
        usable.insert(0, (format!(":future-{n}"), *at));
        let keep = kept(&usable, policy);
        usable.retain(|(id, _)| {
            let stays = keep.contains(id);
            if !stays && existing.contains(id) {
                removed_at.insert(id.clone(), *at);
            }
            stays
        });
        if removed_at.len() == existing.len() {
            break;
        }
    }

    snapshots
        .iter()
        .map(|s| {
            let id = s.qualified_id();
            let removal = if now_removed.contains(&id) {
                Removal::NextBackup
            } else if let Some(at) = removed_at.get(&id) {
                Removal::After(*at)
            } else if existing.contains(&id) {
                Removal::NotWithin
            } else {
                Removal::NotAffected
            };
            (id, removal)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::manifest::{SnapshotHeader, SnapshotStats, SnapshotStatus};
    use crate::engine::snapshots::Location;
    use chrono::{Duration, TimeZone, Utc};
    use std::path::PathBuf;

    fn snapshot(id: usize, days_ago: i64, status: SnapshotStatus) -> SnapshotInfo {
        let started =
            Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap() - Duration::days(days_ago);
        SnapshotInfo {
            id: format!("s{id:03}"),
            computer: "PC".into(),
            location: Location::Plain {
                dir: PathBuf::from(format!("s{id:03}")),
                destination: PathBuf::new(),
            },
            header: Some(SnapshotHeader {
                format: 2,
                app_version: String::new(),
                id: format!("s{id:03}"),
                label: None,
                computer: "PC".into(),
                user: String::new(),
                mode: crate::config::BackupMode::Incremental,
                status,
                started_at: started,
                finished_at: None,
                base: None,
                encrypted: false,
                split: false,
                sources: Vec::new(),
                known_paths: Default::default(),
                stats: SnapshotStats::default(),
            }),
            companion: None,
            label: None,
        }
    }

    #[test]
    fn keeps_last_daily_weekly_monthly() {
        // One backup per day for 400 days, plus a cancelled one yesterday.
        let mut all: Vec<SnapshotInfo> = (0..400)
            .map(|d| snapshot(d, d as i64, SnapshotStatus::Complete))
            .collect();
        all.push(snapshot(999, 1, SnapshotStatus::Cancelled));
        let policy = Retention {
            enabled: true,
            keep_last: 3,
            keep_daily: 7,
            keep_weekly: 4,
            keep_monthly: 12,
        };
        let removed = to_remove(&all, &policy, "PC");
        let kept = all.len() - removed.len();
        // 7 daily (covers the last 3) + up to 4 weekly + 12 monthly, with overlaps.
        assert!((15..=23).contains(&kept), "kept {kept}");
        assert!(!removed.contains(&"s000".to_string()), "the newest stays");
        assert!(
            removed.contains(&"s999".to_string()),
            "old cancelled backups go"
        );
        assert!(removed.contains(&"s399".to_string()));

        // Other computers are left alone.
        assert!(to_remove(&all, &policy, "OTHER").is_empty());
    }

    #[test]
    fn forecast_follows_the_rules_over_time() {
        let all: Vec<SnapshotInfo> = (0..10)
            .map(|d| snapshot(d, d as i64, SnapshotStatus::Complete))
            .collect();
        let policy = Retention {
            enabled: true,
            keep_last: 2,
            keep_daily: 3,
            keep_weekly: 0,
            keep_monthly: 0,
        };
        let start = Utc.with_ymd_and_hms(2026, 9, 16, 12, 0, 0).unwrap();
        let future: Vec<_> = (0..30).map(|d| start + Duration::days(d)).collect();
        let plan = forecast(&all, &policy, "PC", &future);
        let removal = |id: &str| {
            plan.iter()
                .find(|(i, _)| i.ends_with(id))
                .unwrap()
                .1
                .clone()
        };
        // Three daily backups are kept; the older seven go with the next backup.
        assert_eq!(removal("s009"), Removal::NextBackup);
        assert_eq!(removal("s003"), Removal::NextBackup);
        // The oldest kept one goes first, then the next, one per day.
        assert_eq!(removal("s002"), Removal::After(future[0]));
        assert_eq!(removal("s001"), Removal::After(future[1]));
        assert_eq!(removal("s000"), Removal::After(future[2]));

        // With generous rules, backups outlive a short look ahead.
        let generous = Retention {
            keep_last: 20,
            ..policy
        };
        let plan = forecast(&all, &generous, "PC", &future[..3]);
        assert!(plan.iter().any(|(_, r)| *r == Removal::NotWithin));
    }

    #[test]
    fn nothing_is_removed_without_a_complete_backup() {
        let all = vec![
            snapshot(1, 1, SnapshotStatus::Cancelled),
            snapshot(2, 2, SnapshotStatus::Failed),
        ];
        assert!(to_remove(&all, &Retention::default(), "PC").is_empty());
    }
}
