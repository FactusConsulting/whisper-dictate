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

use std::time::Duration;

/// The public version feed: the Chocolatey flatcontainer index published to the
/// project's GitHub Pages site. It is a static JSON file — no GitHub API, no
/// auth, no rate limit. Shape: `{"versions": ["1.8.13", "1.8.14", ...]}`.
const VERSIONS_FEED_URL: &str =
    "https://factusconsulting.github.io/whisper-dictate/chocolatey/flatcontainer/whisper-dictate/index.json";

/// Short, fixed network timeout so a slow/unreachable feed never stalls the
/// background poll for long. The poll runs off the UI thread regardless.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

const USER_AGENT: &str =
    "whisper-dictate/0.3 (+https://github.com/FactusConsulting/whisper-dictate)";

/// Parse a `major.minor.patch` version string into a comparable tuple.
///
/// Returns `None` for anything that is not three dot-separated non-negative
/// integers — this deliberately skips pre-release / build-metadata / non-numeric
/// entries (e.g. `1.9.0-rc1`, `latest`, `""`) so they never win the "newest"
/// comparison. A leading `v` is tolerated.
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

/// Sane floor (minutes) for the poll interval so a misconfigured tiny value
/// can't hammer the feed.
pub(in crate::ui) const MIN_INTERVAL_MINUTES: u64 = 5;

/// Resolve the configured interval string into a clamped poll [`Duration`].
///
/// Parses the raw minutes, falls back to the 15-minute default when unparseable,
/// then clamps to a `>= MIN_INTERVAL_MINUTES` floor. Pure / unit-tested.
pub(in crate::ui) fn poll_interval(raw_minutes: &str) -> Duration {
    let minutes = raw_minutes
        .trim()
        .parse::<u64>()
        .unwrap_or(15)
        .max(MIN_INTERVAL_MINUTES);
    Duration::from_secs(minutes * 60)
}

/// Return the highest published version IF it is strictly newer than `current`,
/// otherwise `None`.
///
/// Pure and unit-tested. Unparseable / pre-release / non-numeric entries are
/// ignored. When `current` itself is unparseable, no update is reported (we
/// can't reason about "newer", so stay silent rather than nag). The returned
/// string is the original feed entry (preserving its exact text), not a
/// reformatted version.
pub(in crate::ui) fn latest_newer_version(versions: &[String], current: &str) -> Option<String> {
    let current_parsed = parse_version(current)?;
    versions
        .iter()
        .filter_map(|v| parse_version(v).map(|parsed| (parsed, v)))
        .filter(|(parsed, _)| *parsed > current_parsed)
        .max_by_key(|(parsed, _)| *parsed)
        .map(|(_, raw)| raw.trim().to_owned())
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
        .set("User-Agent", USER_AGENT)
        .timeout(FETCH_TIMEOUT)
        .call()
        .map_err(|err| format!("update check request failed: {err}"))?
        .into_json()
        .map_err(|err| format!("update check response was not valid JSON: {err}"))?;
    Ok(feed.versions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn newer_version_present_returns_highest() {
        let versions = v(&["1.8.13", "1.8.14", "1.8.15"]);
        assert_eq!(
            latest_newer_version(&versions, "1.8.13"),
            Some("1.8.15".to_owned())
        );
    }

    #[test]
    fn equal_version_returns_none() {
        let versions = v(&["1.8.13", "1.8.14", "1.8.15"]);
        assert_eq!(latest_newer_version(&versions, "1.8.15"), None);
    }

    #[test]
    fn only_older_versions_returns_none() {
        let versions = v(&["1.8.13", "1.8.14"]);
        assert_eq!(latest_newer_version(&versions, "1.8.15"), None);
    }

    #[test]
    fn current_newer_than_feed_returns_none() {
        // Running a dev build ahead of the feed must not nag.
        let versions = v(&["1.8.10", "1.8.11"]);
        assert_eq!(latest_newer_version(&versions, "1.9.0"), None);
    }

    #[test]
    fn highest_picked_regardless_of_order() {
        let versions = v(&["1.8.15", "1.8.13", "1.8.20", "1.8.14"]);
        assert_eq!(
            latest_newer_version(&versions, "1.8.13"),
            Some("1.8.20".to_owned())
        );
    }

    #[test]
    fn cross_minor_and_major_compared_numerically() {
        // String comparison would rank "1.8.9" above "1.10.0"; numeric must not.
        let versions = v(&["1.8.9", "1.10.0", "2.0.0"]);
        assert_eq!(
            latest_newer_version(&versions, "1.8.9"),
            Some("2.0.0".to_owned())
        );
        let versions = v(&["1.8.9", "1.10.0"]);
        assert_eq!(
            latest_newer_version(&versions, "1.9.0"),
            Some("1.10.0".to_owned())
        );
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
        assert_eq!(
            latest_newer_version(&versions, "1.8.14"),
            Some("v1.8.16".to_owned())
        );
    }

    #[test]
    fn unparseable_current_reports_no_update() {
        let versions = v(&["1.8.15"]);
        assert_eq!(latest_newer_version(&versions, "dev"), None);
        assert_eq!(latest_newer_version(&versions, ""), None);
    }

    #[test]
    fn empty_feed_returns_none() {
        assert_eq!(latest_newer_version(&[], "1.8.14"), None);
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
        assert_eq!(
            latest_newer_version(&versions, "v1.8.14"),
            Some("1.8.15".to_owned())
        );
    }
}
