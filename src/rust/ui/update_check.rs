//! In-app "update available" check.
//!
//! PRIVACY: this module ONLY performs an anonymous HTTP GET against the
//! project's public version feed on GitHub Pages (github.io) and sends NO data,
//! telemetry, or identifiers anywhere. It reads back a plain JSON list of
//! published versions and compares the highest one against the running version.
//!
//! The pure comparison ([`latest_newer_version`]) is unit-tested; the thin
//! network fetch ([`fetch_published_versions`]) is not (it touches the network).
//! The periodic background poll that wires these together lives in the app
//! `update()` loop (see `ui/app.rs`), mirroring the one-shot GPU-probe channel
//! discipline but on a timer.

use crate::config::AppSettings;
use std::time::Duration;

/// The public version feed: the Chocolatey flatcontainer index published to the
/// project's GitHub Pages site. It is a static JSON file — no GitHub API, no
/// auth, no rate limit. Shape: `{"versions": ["1.8.13", "1.8.14", ...]}`.
const VERSIONS_FEED_URL: &str =
    "https://factusconsulting.github.io/whisper-dictate/chocolatey/flatcontainer/whisper-dictate/index.json";

/// Short, fixed network timeout so a slow/unreachable feed never stalls the
/// background poll for long. The poll runs off the UI thread regardless.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

const USER_AGENT: &str = concat!(
    "whisper-dictate/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/FactusConsulting/whisper-dictate)"
);

/// A parsed, totally-ordered version: `major.minor.patch` plus an optional
/// `-rc.N` pre-release tail.
///
/// The ordering is SemVer-ish: a final release is NEWER than any of its own
/// release candidates, and rc.1 < rc.2 < … < final. This is realized by the
/// derived `Ord` over the field order below — `is_final` (a `bool`, where
/// `false` < `true`) sits AFTER the numeric triple but BEFORE `rc` so:
///   `1.10.0-rc.1` → (1,10,0,false,1)
///   `1.10.0-rc.2` → (1,10,0,false,2)
///   `1.10.0`      → (1,10,0,true ,0)
/// gives `1.10.0-rc.1 < 1.10.0-rc.2 < 1.10.0`, and a higher (major,minor,patch)
/// always dominates the tail. For a final, `rc` is `0` and inert (it is only
/// compared when `is_final` already ties, which it cannot across final/rc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    is_final: bool,
    rc: u64,
}

/// Parse a `major.minor.patch` version string into a comparable tuple.
///
/// Returns `None` for anything that is not three dot-separated non-negative
/// integers — this deliberately skips pre-release / build-metadata / non-numeric
/// entries (e.g. `1.9.0-rc1`, `latest`, `""`) so they never win the "newest"
/// comparison. A leading `v` is tolerated.
///
/// This is the STABLE-ONLY parser used when the "include release candidates"
/// setting is OFF: it is byte-for-byte the original behaviour, so the disabled
/// path is unchanged. Pre-release parsing lives in [`parse_version_ext`].
fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
    let raw = raw.trim().trim_start_matches('v');
    let mut parts = raw.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    // Reject extra components (e.g. "1.2.3.4") and any trailing junk so only
    // clean three-part numeric versions are considered.
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Parse a version, optionally accepting a `-rc.N` pre-release tail.
///
/// When `include_prereleases` is false this is exactly [`parse_version`] mapped
/// to a final [`Version`] — so the OFF path stays byte-identical. When true it
/// additionally accepts `X.Y.Z-rc.N` (case-insensitive `rc`, `N` a non-negative
/// integer), returning a pre-release [`Version`]. Only the `-rc.N` form is a
/// recognized pre-release; any other tail (e.g. `-beta`, `-rc` without a number,
/// `-rc.1.2`) still returns `None` so unknown shapes can never win the compare.
/// A leading `v` is tolerated.
fn parse_version_ext(raw: &str, include_prereleases: bool) -> Option<Version> {
    let raw = raw.trim().trim_start_matches('v');
    let (core, rc) = match raw.split_once('-') {
        None => (raw, None),
        Some((core, tail)) => {
            if !include_prereleases {
                // A pre-release tail is present but RCs are not wanted: skip it,
                // exactly like the stable-only parser does.
                return None;
            }
            // Truly case-insensitive `rc.` match (rc./RC./Rc./rC.), as the
            // docstring promises — digits are unaffected by the lowercasing.
            let lower = tail.to_ascii_lowercase();
            let rest = lower.strip_prefix("rc.")?;
            let n = rest.parse::<u64>().ok()?;
            (core, Some(n))
        }
    };
    let (major, minor, patch) = parse_version(core)?;
    Some(Version {
        major,
        minor,
        patch,
        is_final: rc.is_none(),
        rc: rc.unwrap_or(0),
    })
}

/// Parse the CURRENT running version for comparison.
///
/// `CARGO_PKG_VERSION` can itself be a pre-release (e.g. `1.10.0-rc.1`). When
/// the setting is ON we parse it faithfully (so an rc tester is compared as an
/// rc and offered both newer RCs and the eventual final). When the setting is
/// OFF and the current version is an rc, the stable-only parser would return
/// `None` and the check would go SILENT — so we fall back to the rc's BASE
/// version (`1.10.0-rc.1` → final `1.10.0`). That deliberately treats the
/// running rc as if it were already on the final line, so only a STRICTLY newer
/// final (`1.10.1`+) is offered and the user is never nagged to "upgrade" to the
/// very final their rc anticipates. Documented in docs/CONFIGURATION.md.
fn parse_current_version(current: &str, include_prereleases: bool) -> Option<Version> {
    if let Some(parsed) = parse_version_ext(current, include_prereleases) {
        return Some(parsed);
    }
    // OFF path and current is an rc: parse its base (strip the `-rc.N` tail) so a
    // newer final is still offered instead of the check silently disappearing.
    let base = current.trim().trim_start_matches('v').split('-').next()?;
    let (major, minor, patch) = parse_version(base)?;
    Some(Version {
        major,
        minor,
        patch,
        is_final: true,
        rc: 0,
    })
}

/// Sane floor (minutes) for the poll interval so a misconfigured tiny value
/// can't hammer the feed.
pub(in crate::ui) const MIN_INTERVAL_MINUTES: u64 = 5;

/// Resolve the configured interval string into a clamped poll [`Duration`].
///
/// Parses the raw minutes, falls back to the 15-minute default when unparseable,
/// then clamps to a `>= MIN_INTERVAL_MINUTES` floor. A huge/hostile value is
/// saturated via [`u64::saturating_mul`] so it never overflows. Pure / unit-tested.
pub(in crate::ui) fn poll_interval(raw_minutes: &str) -> Duration {
    let minutes = raw_minutes
        .trim()
        .parse::<u64>()
        .unwrap_or(15)
        .max(MIN_INTERVAL_MINUTES);
    Duration::from_secs(minutes.saturating_mul(60))
}

/// Return the highest published version IF it is strictly newer than `current`,
/// otherwise `None`.
///
/// Pure and unit-tested. Unparseable / non-numeric entries are ignored. When
/// `include_prereleases` is false, `-rc.N` feed entries are skipped too — this
/// path is byte-identical to the original stable-only behaviour. When it is
/// true, both finals and `X.Y.Z-rc.N` entries are considered and the SemVer-ish
/// ordering (`rc.1 < rc.2 < final`) decides the winner.
///
/// When `current` itself is unparseable, no update is reported (we can't reason
/// about "newer", so stay silent rather than nag) — except that a running rc is
/// still compared against its base final when the setting is off (see
/// [`parse_current_version`]). The returned string is derived from the original
/// feed entry with surrounding whitespace trimmed.
pub(in crate::ui) fn latest_newer_version(
    versions: &[String],
    current: &str,
    include_prereleases: bool,
) -> Option<String> {
    let current_parsed = parse_current_version(current, include_prereleases)?;
    versions
        .iter()
        .filter_map(|v| parse_version_ext(v, include_prereleases).map(|parsed| (parsed, v)))
        .filter(|(parsed, _)| *parsed > current_parsed)
        .max_by_key(|(parsed, _)| *parsed)
        .map(|(_, raw)| raw.trim().to_owned())
}

/// Whether an offered version string is a recognized `-rc.N` pre-release.
///
/// Pure helper used by the upgrade-action mapping so a prerelease offer can pin
/// `choco --prerelease` and point installer/portable users at the rc's tag URL
/// instead of the `releases/latest` page (which excludes prereleases). A leading
/// `v` is tolerated; only the `X.Y.Z-rc.N` shape counts.
pub(in crate::ui) fn version_is_prerelease(version: &str) -> bool {
    parse_version_ext(version, true).is_some_and(|v| !v.is_final)
}

/// Outcome of one background update-check cycle.
///
/// Using a typed enum (rather than collapsing everything into `Option<String>`)
/// lets the caller distinguish three cases:
/// - `Newer(v)` — feed was reachable AND a newer version was found.
/// - `UpToDate`  — feed was reachable AND no newer version exists.
/// - `Failed`    — the fetch failed (network error, bad JSON, …).
///
/// Only `Failed` must NOT clear an already-visible badge; the other two cases
/// set `update_available` definitively.
#[derive(Debug, PartialEq)]
pub(in crate::ui) enum UpdateCheckOutcome {
    Newer(String),
    UpToDate,
    Failed,
}

/// Pure helper: given the previous `update_available` state and a new poll
/// outcome, return the next state.
///
/// - `Newer(v)` → `Some(v)`
/// - `UpToDate`  → `None`  (we checked and there is nothing new)
/// - `Failed`    → `prev`  (transient error: leave badge untouched)
///
/// Factored out of `poll_update_check` so it can be driven directly in tests
/// without constructing the full app state.
pub(in crate::ui) fn apply_update_outcome(
    prev: Option<String>,
    outcome: UpdateCheckOutcome,
) -> Option<String> {
    match outcome {
        UpdateCheckOutcome::Newer(v) => Some(v),
        UpdateCheckOutcome::UpToDate => None,
        UpdateCheckOutcome::Failed => prev,
    }
}

/// True when a save changed any update-check-related setting: the enabled
/// flag, the poll interval, or the release-candidate opt-in.
///
/// Used by `save_settings` to reset the poll timer so the change takes effect
/// on the next frame instead of after the current poll interval (up to the
/// configured minutes — e.g. enabling "Include release candidates" should
/// offer an available RC immediately, not 15 minutes later). Pure / unit-tested.
pub(in crate::ui) fn update_check_settings_changed(old: &AppSettings, new: &AppSettings) -> bool {
    old.update_check != new.update_check
        || old.update_check_interval_minutes != new.update_check_interval_minutes
        || old.update_include_prereleases != new.update_include_prereleases
}

/// Fetch the published version list from the public feed.
///
/// PRIVACY: anonymous GET only — no request body, no identifiers, no data sent.
/// Thin by design (not unit-tested): does the HTTP GET, parses
/// `{"versions":[...]}`, and returns the string list or a short error message.
pub(in crate::ui) fn fetch_published_versions() -> Result<Vec<String>, String> {
    #[derive(serde::Deserialize)]
    struct Feed {
        #[serde(default)]
        versions: Vec<String>,
    }

    let feed: Feed = ureq::get(VERSIONS_FEED_URL)
        .header("User-Agent", USER_AGENT)
        .config()
        .timeout_global(Some(FETCH_TIMEOUT))
        .build()
        .call()
        .map_err(|err| format!("update check request failed: {err}"))?
        .body_mut()
        .read_json()
        .map_err(|err| format!("update check response was not valid JSON: {err}"))?;
    Ok(feed.versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Stable-only wrapper preserving the original 2-arg call shape: the OFF
    /// path (`include_prereleases = false`) must stay byte-identical to the
    /// historical behaviour, so every legacy test runs through this.
    fn lnv(versions: &[String], current: &str) -> Option<String> {
        latest_newer_version(versions, current, false)
    }

    /// Opt-in (RCs included) wrapper used by the prerelease tests.
    fn lnv_rc(versions: &[String], current: &str) -> Option<String> {
        latest_newer_version(versions, current, true)
    }

    #[test]
    fn newer_version_present_returns_highest() {
        let versions = v(&["1.8.13", "1.8.14", "1.8.15"]);
        assert_eq!(lnv(&versions, "1.8.13"), Some("1.8.15".to_owned()));
    }

    #[test]
    fn equal_version_returns_none() {
        let versions = v(&["1.8.13", "1.8.14", "1.8.15"]);
        assert_eq!(lnv(&versions, "1.8.15"), None);
    }

    #[test]
    fn only_older_versions_returns_none() {
        let versions = v(&["1.8.13", "1.8.14"]);
        assert_eq!(lnv(&versions, "1.8.15"), None);
    }

    #[test]
    fn current_newer_than_feed_returns_none() {
        // Running a dev build ahead of the feed must not nag.
        let versions = v(&["1.8.10", "1.8.11"]);
        assert_eq!(lnv(&versions, "1.9.0"), None);
    }

    #[test]
    fn highest_picked_regardless_of_order() {
        let versions = v(&["1.8.15", "1.8.13", "1.8.20", "1.8.14"]);
        assert_eq!(lnv(&versions, "1.8.13"), Some("1.8.20".to_owned()));
    }

    #[test]
    fn cross_minor_and_major_compared_numerically() {
        // String comparison would rank "1.8.9" above "1.10.0"; numeric must not.
        let versions = v(&["1.8.9", "1.10.0", "2.0.0"]);
        assert_eq!(lnv(&versions, "1.8.9"), Some("2.0.0".to_owned()));
        let versions = v(&["1.8.9", "1.10.0"]);
        assert_eq!(lnv(&versions, "1.9.0"), Some("1.10.0".to_owned()));
    }

    #[test]
    fn garbage_and_prerelease_entries_ignored() {
        let versions = v(&[
            "",
            "latest",
            "1.8.16-rc1",
            "1.8.x",
            "1.8",
            "1.8.16.1",
            "v1.8.16",
            "1.8.14",
        ]);
        // Only "v1.8.16" (→1.8.16) and "1.8.14" parse; the highest newer is 1.8.16.
        assert_eq!(lnv(&versions, "1.8.14"), Some("v1.8.16".to_owned()));
    }

    #[test]
    fn unparseable_current_reports_no_update() {
        let versions = v(&["1.8.15"]);
        assert_eq!(lnv(&versions, "dev"), None);
        assert_eq!(lnv(&versions, ""), None);
    }

    #[test]
    fn empty_feed_returns_none() {
        assert_eq!(lnv(&[], "1.8.14"), None);
    }

    #[test]
    fn poll_interval_uses_value_clamps_floor_and_falls_back() {
        assert_eq!(poll_interval("15"), Duration::from_secs(15 * 60));
        assert_eq!(poll_interval("30"), Duration::from_secs(30 * 60));
        // Below the floor is clamped up.
        assert_eq!(
            poll_interval("1"),
            Duration::from_secs(MIN_INTERVAL_MINUTES * 60)
        );
        assert_eq!(
            poll_interval("0"),
            Duration::from_secs(MIN_INTERVAL_MINUTES * 60)
        );
        // Unparseable falls back to the 15-minute default.
        assert_eq!(poll_interval("abc"), Duration::from_secs(15 * 60));
        assert_eq!(poll_interval(""), Duration::from_secs(15 * 60));
    }

    #[test]
    fn leading_v_on_current_tolerated() {
        let versions = v(&["1.8.15"]);
        assert_eq!(lnv(&versions, "v1.8.14"), Some("1.8.15".to_owned()));
    }

    // ── poll_interval overflow safety ────────────────────────────────────────

    #[test]
    fn poll_interval_huge_value_does_not_overflow_or_panic() {
        // u64::MAX minutes would overflow minutes * 60 without saturating_mul.
        // We just assert no panic and that the result is >= the floor.
        let floor = Duration::from_secs(MIN_INTERVAL_MINUTES * 60);
        assert!(poll_interval(&u64::MAX.to_string()) >= floor);
        // A value just below u64::MAX / 60 also must not overflow.
        let near_max = (u64::MAX / 60).to_string();
        assert!(poll_interval(&near_max) >= floor);
    }

    // ── apply_update_outcome (Finding 4) ─────────────────────────────────────

    #[test]
    fn fetch_error_does_not_clear_prior_badge() {
        // A transient failure must leave the previously-found version in place.
        let prev = Some("1.9.0".to_owned());
        assert_eq!(
            apply_update_outcome(prev.clone(), UpdateCheckOutcome::Failed),
            prev
        );
    }

    #[test]
    fn fetch_error_when_no_prior_badge_stays_none() {
        assert_eq!(apply_update_outcome(None, UpdateCheckOutcome::Failed), None);
    }

    #[test]
    fn up_to_date_clears_badge() {
        let prev = Some("1.9.0".to_owned());
        assert_eq!(
            apply_update_outcome(prev, UpdateCheckOutcome::UpToDate),
            None
        );
    }

    #[test]
    fn newer_outcome_sets_badge() {
        assert_eq!(
            apply_update_outcome(None, UpdateCheckOutcome::Newer("2.0.0".to_owned())),
            Some("2.0.0".to_owned())
        );
        // Also replaces an older badge.
        assert_eq!(
            apply_update_outcome(
                Some("1.9.0".to_owned()),
                UpdateCheckOutcome::Newer("2.0.0".to_owned())
            ),
            Some("2.0.0".to_owned())
        );
    }

    // ── prerelease ordering / opt-in (update_include_prereleases) ─────────────

    #[test]
    fn version_ordering_rc_then_final() {
        // A final is newer than its own RCs, and rc.N is ordered numerically.
        let p = |s: &str| parse_version_ext(s, true).unwrap();
        assert!(p("1.10.0-rc.1") < p("1.10.0-rc.2"));
        assert!(p("1.10.0-rc.2") < p("1.10.0"));
        assert!(p("1.10.0-rc.1") < p("1.10.0"));
        // rc.10 > rc.2 (numeric, not lexicographic).
        assert!(p("1.10.0-rc.2") < p("1.10.0-rc.10"));
        // A higher (major,minor,patch) dominates any tail.
        assert!(p("1.10.0") < p("1.10.1-rc.1"));
        assert!(p("1.10.0-rc.9") < p("1.11.0-rc.1"));
    }

    #[test]
    fn parse_ext_off_skips_prereleases_like_stable_parser() {
        // OFF: a prerelease string never parses (byte-identical to the old skip).
        assert_eq!(parse_version_ext("1.10.0-rc.1", false), None);
        // A clean final still parses to a final version.
        let final_v = parse_version_ext("1.10.0", false).unwrap();
        assert!(final_v.is_final);
    }

    #[test]
    fn parse_ext_on_only_accepts_rc_dot_n_shape() {
        // Recognized prerelease shape — case-insensitive rc tail.
        assert!(parse_version_ext("1.10.0-rc.1", true).is_some());
        assert!(parse_version_ext("v1.10.0-RC.3", true).is_some());
        assert!(parse_version_ext("1.10.0-Rc.1", true).is_some());
        assert!(parse_version_ext("1.10.0-rC.2", true).is_some());
        // Unknown tails never win the compare.
        assert_eq!(parse_version_ext("1.10.0-rc1", true), None);
        assert_eq!(parse_version_ext("1.10.0-rc", true), None);
        assert_eq!(parse_version_ext("1.10.0-rc.1.2", true), None);
        assert_eq!(parse_version_ext("1.10.0-beta.1", true), None);
    }

    #[test]
    fn disabled_setting_is_byte_identical_regression() {
        // With the setting OFF, a feed carrying RCs must behave EXACTLY as the
        // historical stable-only logic: RCs ignored, only finals considered.
        let versions = v(&["1.9.5", "1.10.0-rc.1", "1.10.0-rc.2"]);
        // No newer FINAL than 1.9.5 → no update offered (RCs are invisible).
        assert_eq!(lnv(&versions, "1.9.5"), None);
        // A newer final IS offered, RCs still ignored.
        let versions = v(&["1.9.5", "1.9.6", "1.10.0-rc.1"]);
        assert_eq!(lnv(&versions, "1.9.5"), Some("1.9.6".to_owned()));
    }

    #[test]
    fn enabled_setting_offers_newest_rc() {
        // With the setting ON, the newest RC wins when no final is newer.
        let versions = v(&["1.9.5", "1.10.0-rc.1", "1.10.0-rc.2"]);
        assert_eq!(lnv_rc(&versions, "1.9.5"), Some("1.10.0-rc.2".to_owned()));
    }

    #[test]
    fn enabled_setting_prefers_final_over_rc() {
        // A final outranks its own RCs even if the RC string sorts later.
        let versions = v(&["1.10.0-rc.1", "1.10.0-rc.2", "1.10.0"]);
        assert_eq!(lnv_rc(&versions, "1.9.5"), Some("1.10.0".to_owned()));
    }

    #[test]
    fn enabled_current_is_rc_offers_newer_rc_and_final() {
        // Running 1.10.0-rc.1 (ON): a newer RC is offered…
        let versions = v(&["1.10.0-rc.1", "1.10.0-rc.2"]);
        assert_eq!(
            lnv_rc(&versions, "1.10.0-rc.1"),
            Some("1.10.0-rc.2".to_owned())
        );
        // …and the eventual final is offered as newer than the rc.
        let versions = v(&["1.10.0-rc.1", "1.10.0"]);
        assert_eq!(lnv_rc(&versions, "1.10.0-rc.1"), Some("1.10.0".to_owned()));
        // The rc's OWN final is not "newer" than… itself once we're ON it would
        // be; but the running rc anticipates 1.10.0, which IS strictly newer.
    }

    #[test]
    fn enabled_current_rc_not_nagged_by_equal_rc() {
        // Running 1.10.0-rc.2 (ON): an equal rc.2 is not newer; an older rc.1 is
        // not newer either.
        let versions = v(&["1.10.0-rc.1", "1.10.0-rc.2"]);
        assert_eq!(lnv_rc(&versions, "1.10.0-rc.2"), None);
    }

    #[test]
    fn disabled_current_is_rc_falls_back_to_base_and_offers_newer_final() {
        // Running 1.10.0-rc.1 with the setting OFF: the stable-only parser would
        // return None and the check would go SILENT. Instead we compare against
        // the rc's BASE final (1.10.0), so a newer FINAL is still offered…
        let versions = v(&["1.10.0", "1.10.1"]);
        assert_eq!(lnv(&versions, "1.10.0-rc.1"), Some("1.10.1".to_owned()));
        // …and the rc's OWN final (1.10.0) is NOT offered (base==final, not newer),
        // so the rc tester isn't nagged to "upgrade" to the final they anticipate.
        let versions = v(&["1.10.0"]);
        assert_eq!(lnv(&versions, "1.10.0-rc.1"), None);
        // RCs in the feed remain invisible while OFF.
        let versions = v(&["1.10.0-rc.2"]);
        assert_eq!(lnv(&versions, "1.10.0-rc.1"), None);
    }

    #[test]
    fn version_is_prerelease_detects_rc_only() {
        assert!(version_is_prerelease("1.10.0-rc.1"));
        assert!(version_is_prerelease("v1.10.0-RC.2"));
        assert!(!version_is_prerelease("1.10.0"));
        assert!(!version_is_prerelease("1.10.0-beta.1"));
        assert!(!version_is_prerelease("garbage"));
    }
}
