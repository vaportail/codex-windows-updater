//! Update check + defer-decision helpers.
//!
//! The update check is cheap: it resolves the Store's latest version string
//! via FE3 SyncUpdates (no download) and compares against `config.current_version`.
//! Callers apply the user's response by mutating the Config and saving it.
//!
//! Decision flow:
//!   1. Policy == Never          → Skipped
//!   2. suppress_until_unix > now → Skipped
//!   3. Resolve latest
//!      - err                     → Error (show but let user continue)
//!      - == current              → UpToDate
//!      - != current              → Available { current, latest }
//!
//! The policy (Always/Daily/Weekly) only gates *automatic* checks from proxy
//! mode — an explicit "Check for updates" action bypasses it.

use crate::config::{Config, UpdatePolicy};
use crate::store::{self, Fetcher};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub enum UpdateDecision {
    /// Skip the check entirely (policy=Never, suppressed, or policy cooldown).
    Skipped { reason: String },
    /// Check ran; we're on the latest version.
    UpToDate { version: String },
    /// Check ran; a newer version exists.
    Available { current: String, latest: String },
    /// Check failed — surface the error but don't block the app.
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeferChoice {
    /// Update right now.
    UpdateNow,
    /// Remind me on the next scheduled check.
    NotNow,
    /// Skip this specific version forever.
    SkipThisVersion,
    /// Snooze 1 day.
    SnoozeOneDay,
    /// Snooze 7 days.
    SnoozeSevenDays,
    /// Turn off all update prompts.
    Never,
}

/// Automatic check — honors policy cooldown + suppress_until. Use this from
/// proxy-mode startup.
pub fn check_auto(cfg: &Config, product_id: &str) -> UpdateDecision {
    let now = now_unix();
    if cfg.update_policy == UpdatePolicy::Never {
        return UpdateDecision::Skipped {
            reason: "update_policy = never".into(),
        };
    }
    if let Some(until) = cfg.suppress_until_unix {
        if now < until {
            let days = (until - now) / 86_400;
            return UpdateDecision::Skipped {
                reason: format!("suppressed for ~{days}d"),
            };
        }
    }
    if let Some(last) = cfg.last_check_unix {
        let cooldown = policy_cooldown_secs(cfg.update_policy);
        if now.saturating_sub(last) < cooldown {
            return UpdateDecision::Skipped {
                reason: "within cooldown".into(),
            };
        }
    }
    let decision = check_now(cfg, product_id);
    // Honor "skip this version" only while the Store's latest still matches
    // the skipped version — once Microsoft publishes something newer, the
    // suppression is implicitly lifted.
    if let UpdateDecision::Available { latest, .. } = &decision {
        if cfg.skipped_version.as_deref() == Some(latest.as_str()) {
            return UpdateDecision::Skipped {
                reason: format!("version {latest} skipped by user"),
            };
        }
    }
    decision
}

/// Force a check regardless of policy/suppression. Use this when the user
/// explicitly clicks "Check for updates".
pub fn check_now(cfg: &Config, product_id: &str) -> UpdateDecision {
    match store::resolve_latest_version(cfg.fetcher, product_id) {
        Ok(latest) => {
            if version_gt(&latest, &cfg.current_version) {
                UpdateDecision::Available {
                    current: cfg.current_version.clone(),
                    latest,
                }
            } else {
                UpdateDecision::UpToDate { version: latest }
            }
        }
        Err(e) => UpdateDecision::Error(format!("{:#}", e)),
    }
}

/// Apply a defer choice to `cfg`. Caller is responsible for saving afterward.
/// `latest` is the version the user was prompted about (used for SkipThisVersion).
pub fn apply_defer(cfg: &mut Config, choice: DeferChoice, latest: &str) {
    let now = now_unix();
    cfg.last_check_unix = Some(now);
    cfg.known_latest = Some(latest.to_string());
    match choice {
        DeferChoice::UpdateNow => {
            cfg.suppress_until_unix = None;
            cfg.skipped_version = None;
        }
        DeferChoice::NotNow => {
            // Let normal policy cooldown govern the next check — nothing to do.
        }
        DeferChoice::SkipThisVersion => {
            // Suppress prompts specifically for this version. `check_auto`
            // filters Available decisions where latest == skipped_version;
            // once the Store moves past this version, prompts resume.
            cfg.skipped_version = Some(latest.to_string());
        }
        DeferChoice::SnoozeOneDay => cfg.suppress_until_unix = Some(now + 86_400),
        DeferChoice::SnoozeSevenDays => cfg.suppress_until_unix = Some(now + 7 * 86_400),
        DeferChoice::Never => {
            cfg.update_policy = UpdatePolicy::Never;
            cfg.suppress_until_unix = None;
        }
    }
}

/// Record a successful up-to-date check — bumps `last_check_unix` + known_latest.
pub fn record_check(cfg: &mut Config, latest: &str) {
    cfg.last_check_unix = Some(now_unix());
    cfg.known_latest = Some(latest.to_string());
}

fn policy_cooldown_secs(p: UpdatePolicy) -> u64 {
    match p {
        UpdatePolicy::Always => 0,
        UpdatePolicy::Daily => 86_400,
        UpdatePolicy::Weekly => 7 * 86_400,
        UpdatePolicy::Never => u64::MAX, // unreachable (filtered earlier)
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Dotted-numeric compare. `a > b`?
fn version_gt(a: &str, b: &str) -> bool {
    let pa: Vec<u64> = a.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    let pb: Vec<u64> = b.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    pa > pb
}

// Keep this so the `Fetcher` import stays live if we later gate by it.
#[allow(dead_code)]
fn _fetcher_check(f: Fetcher) -> Fetcher {
    f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(version_gt("26.500.0.0", "26.422.2437.0"));
        assert!(!version_gt("26.422.2437.0", "26.422.2437.0"));
        assert!(!version_gt("26.100.0.0", "26.422.0.0"));
        assert!(version_gt("27.0.0.0", "26.999.999.999"));
    }
}
