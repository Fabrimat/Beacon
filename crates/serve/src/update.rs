//! Phase-5 contract: updater policy (9, 9.2). The app checks a static feed and installs through
//! `tauri-plugin-updater`, which verifies the downloaded installer's minisign signature against
//! the public key compiled into this build before running it. This module decides what the app
//! may offer; it never downloads or installs.
//!
//! - **Fails closed.** A build without a public key or a feed ([`BUILD`]: both unset until the
//!   maintainer generates the key and publishes a feed) is [`Decision::Disabled`]; the plugin
//!   gets the same key, so an unsigned or wrongly signed installer is never run.
//! - **Never without consent.** `update_check` only fetches and evaluates; `update_install`
//!   runs only on the user's click. No background install, no identifiers in requests (1).
//! - **No downgrades** through the feed: a version not newer than the installed one is
//!   [`Decision::UpToDate`]. Going back is [`Rollback`], the user's explicit choice, and reuses
//!   the previous installer kept (with its blake3) when the update was installed.
//! - **Channels** are separate feeds: `<feed>/<channel>/latest.json`.
//! - Feed shape = Tauri's static updater JSON: `{"version", "notes", "pub_date",
//!   "platforms": {"windows-x86_64": {"url", "signature"}}}`.

use beacon_engine::diag::{Code, Diagnostic};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Beta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateConfig {
    /// minisign public key (the content of `tauri signer generate`'s `.pub`).
    pub pubkey: Option<&'static str>,
    /// Feed base URL (`https://`).
    pub feed: Option<&'static str>,
}

/// This build's updater config, from the release pipeline's environment. Unset (disabled)
/// in every build until signing and a feed exist.
pub const BUILD: UpdateConfig = UpdateConfig {
    pubkey: option_env!("BEACON_UPDATE_PUBKEY"),
    feed: option_env!("BEACON_UPDATE_FEED"),
};

impl UpdateConfig {
    /// Feed URL for a channel; `None` when the updater is disabled.
    pub fn feed_url(&self, channel: Channel) -> Option<String> {
        let (Some(_), Some(feed)) = (self.pubkey, self.feed) else {
            return None;
        };
        let ch = match channel {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
        };
        Some(format!("{}/{ch}/latest.json", feed.trim_end_matches('/')))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pub_date: Option<String>,
    pub platforms: BTreeMap<String, Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Asset {
    pub url: String,
    /// minisign signature of the installer (verified by the plugin before install).
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum Decision {
    /// No key or no feed in this build (`UpdateDisabled`).
    Disabled,
    UpToDate,
    Available {
        version: String,
        notes: String,
        url: String,
        signature: String,
    },
}

/// The previous version's installer, kept to roll back to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rollback {
    pub version: String,
    pub installer: PathBuf,
    /// blake3 hex recorded when it was kept; checked again before it runs.
    pub blake3: String,
    /// Its minisign signature from the feed it came from; verified again before it runs.
    pub signature: String,
}

/// `((major, minor, patch), pre)`; a release sorts after its pre-releases.
type Version<'a> = ((u64, u64, u64), Option<&'a str>);

fn parse_version(v: &str) -> Option<Version<'_>> {
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let mut n = core.split('.').map(|p| p.parse::<u64>().ok());
    let t = (n.next()??, n.next()??, n.next()??);
    n.next().is_none().then_some((t, pre))
}

fn newer(candidate: &str, current: &str) -> Option<bool> {
    let ((a, ap), (b, bp)) = (parse_version(candidate)?, parse_version(current)?);
    Some(match a.cmp(&b) {
        std::cmp::Ordering::Equal => match (ap, bp) {
            (None, Some(_)) => true,
            (Some(x), Some(y)) => x > y,
            _ => false,
        },
        o => o.is_gt(),
    })
}

fn rejected(msg: String) -> Diagnostic {
    Diagnostic::new(Code::UpdateRejected, msg)
}

/// What the app may offer, given the fetched feed bytes. Fails closed: a disabled build never
/// looks at the bytes; a malformed feed, an unparsable version, or an asset that is not
/// `https://` or has no signature is `UpdateRejected`.
pub fn evaluate(
    cfg: &UpdateConfig,
    current: &str,
    feed: &[u8],
    platform: &str,
) -> Result<Decision, Diagnostic> {
    if cfg.pubkey.is_none() || cfg.feed.is_none() {
        return Ok(Decision::Disabled);
    }
    let m: Manifest = serde_json::from_slice(feed)
        .map_err(|e| rejected(format!("update feed is not valid: {e}")))?;
    let is_newer = newer(&m.version, current).ok_or_else(|| {
        rejected(format!(
            "update version `{}` or installed `{current}` is not x.y.z",
            m.version
        ))
    })?;
    let Some(asset) = m.platforms.get(platform).filter(|_| is_newer) else {
        return Ok(Decision::UpToDate);
    };
    if !asset.url.starts_with("https://") || asset.signature.trim().is_empty() {
        return Err(rejected(format!(
            "update {} for {platform} has no https URL or no signature",
            m.version
        )));
    }
    Ok(Decision::Available {
        version: m.version,
        notes: m.notes,
        url: asset.url.clone(),
        signature: asset.signature.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ON: UpdateConfig = UpdateConfig {
        pubkey: Some("RWTEST"),
        feed: Some("https://example.invalid/beacon/"),
    };

    fn feed(version: &str, url: &str, sig: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "version": version, "notes": "n", "pub_date": "2026-10-19T00:00:00Z",
            "platforms": {"windows-x86_64": {"url": url, "signature": sig}}
        }))
        .unwrap()
    }

    #[test]
    fn this_build_is_disabled_and_ignores_the_feed() {
        let off = UpdateConfig {
            pubkey: None,
            feed: Some("https://x"),
        };
        assert_eq!(
            evaluate(&off, "0.1.0", b"not json", "windows-x86_64").unwrap(),
            Decision::Disabled
        );
        assert_eq!(off.feed_url(Channel::Stable), None);
        assert_eq!(
            ON.feed_url(Channel::Beta).unwrap(),
            "https://example.invalid/beacon/beta/latest.json"
        );
    }

    #[test]
    fn fails_closed() {
        let w = "windows-x86_64";
        let ok = evaluate(&ON, "0.1.0", &feed("0.2.0", "https://h/i.exe", "sig"), w).unwrap();
        assert!(matches!(ok, Decision::Available { ref version, .. } if version == "0.2.0"));
        for bad in [
            feed("0.2.0", "http://h/i.exe", "sig"),
            feed("0.2.0", "https://h/i.exe", " "),
            feed("two", "https://h/i.exe", "sig"),
            b"{".to_vec(),
        ] {
            assert_eq!(
                evaluate(&ON, "0.1.0", &bad, w).unwrap_err().code,
                Code::UpdateRejected
            );
        }
        // no downgrade, no re-install, no asset for this platform
        for (v, cur, p) in [
            ("0.1.0", "0.2.0", w),
            ("0.2.0", "0.2.0", w),
            ("0.2.0", "0.1.0", "darwin-aarch64"),
        ] {
            assert_eq!(
                evaluate(&ON, cur, &feed(v, "https://h/i.exe", "s"), p).unwrap(),
                Decision::UpToDate
            );
        }
        assert_eq!(newer("0.2.0", "0.2.0-alpha.1"), Some(true));
        assert_eq!(newer("0.2.0-alpha.2", "0.2.0-alpha.1"), Some(true));
        assert_eq!(newer("0.2.0-alpha.1", "0.2.0"), Some(false));
    }
}
