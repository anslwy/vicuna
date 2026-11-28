// This build's version + semver ordering shared by the desktop, the embedded
// server and the `vicuna` CLI.
//
// Rolling dev builds bake `<maj>.<min>.<patch+1>-dev.<YYYYMMDDHHMMSS>` through
// the `VICUNA_BUILD_VERSION` env var at compile time (release.yml →
// scripts/dev-version.mjs); everything else reports Cargo.toml's version.

use std::cmp::Ordering;

/// The version this binary reports (`vicuna --version`, `/health`,
/// `server_info`, upgrade checks).
pub fn vicuna_version() -> &'static str {
    match option_env!("VICUNA_BUILD_VERSION") {
        Some(v) if !v.trim().is_empty() => v,
        _ => env!("CARGO_PKG_VERSION"),
    }
}

struct Parsed<'a> {
    core: (u64, u64, u64),
    pre: Vec<&'a str>,
}

fn parse(s: &str) -> Option<Parsed<'_>> {
    let s = s.trim().trim_start_matches('v');
    let s = s.split('+').next()?;
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, p.split('.').filter(|x| !x.is_empty()).collect()),
        None => (s, Vec::new()),
    };
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let core = (it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0));
    Some(Parsed { core, pre })
}

/// Semver precedence including prerelease tags: `0.1.2` > `0.1.2-dev.2` >
/// `0.1.2-dev.1` > `0.1.1`. `None` when either side isn't a version.
pub fn compare(a: &str, b: &str) -> Option<Ordering> {
    let (a, b) = (parse(a)?, parse(b)?);
    Some(a.core.cmp(&b.core).then_with(|| match (a.pre.is_empty(), b.pre.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => {
            for (x, y) in a.pre.iter().zip(b.pre.iter()) {
                let o = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    _ => x.cmp(y),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
            a.pre.len().cmp(&b.pre.len())
        }
    }))
}

/// True when `latest` sorts strictly after `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    compare(latest, current) == Some(Ordering::Greater)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dev_prereleases_order_like_semver() {
        assert!(is_newer("0.1.2-dev.20261128155414", "0.1.1")); // stable → dev
        assert!(is_newer("0.1.2-dev.20261128160000", "0.1.2-dev.20261128155414")); // dev → newer dev
        assert!(is_newer("0.1.2", "0.1.2-dev.20261128160000")); // dev → stable
        assert!(!is_newer("0.1.2-dev.20261128155414", "0.1.2-dev.20261128155414")); // same dev
        assert!(!is_newer("0.1.1", "0.1.2-dev.20261128155414")); // older stable is not newer
        assert!(is_newer("v0.1.2", "0.1.1")); // stable → stable
        assert!(!is_newer("v0.1.1", "0.1.1"));
        assert!(is_newer("v0.0.10", "0.0.9"));
        assert!(!is_newer("latest", "0.0.1"));
        assert!(!is_newer("0.1.1", "garbage"));
    }
    #[test]
    fn version_is_never_empty() {
        assert!(!vicuna_version().is_empty());
        assert!(parse(vicuna_version()).is_some());
    }
}
