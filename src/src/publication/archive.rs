//! Publication timestamps from the Arch Linux Archive.
//!
//! The authoritative publication date of a version is the day it first
//! appeared in the archive, i.e. the earliest `/repos/YYYY/MM/DD/` snapshot
//! containing it. Probing daily snapshots would cost many requests per
//! package, so instead we read the package pool index
//! (`/packages/<first-letter>/<name>/`), whose per-file timestamps record
//! exactly when each version file entered the archive — the same date as its
//! first snapshot. One request per package.

use crate::date::epoch_from_date_time;
use crate::error::Result;
use crate::http::{FetchOutcome, HttpClient};
use crate::model::{Publication, PublicationBasis, UpgradeCandidate};

use super::PublicationSource;

pub const DEFAULT_BASE_URL: &str = "https://archive.archlinux.org";

pub struct ArchivePublicationSource<'a> {
    http: &'a dyn HttpClient,
    base_url: String,
    arch: String,
}

impl<'a> ArchivePublicationSource<'a> {
    pub fn new(http: &'a dyn HttpClient) -> Self {
        ArchivePublicationSource {
            http,
            base_url: DEFAULT_BASE_URL.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }

    #[cfg(test)]
    fn with_base_url(http: &'a dyn HttpClient, base_url: &str) -> Self {
        ArchivePublicationSource {
            http,
            base_url: base_url.to_string(),
            arch: "x86_64".to_string(),
        }
    }
}

impl PublicationSource for ArchivePublicationSource<'_> {
    fn publication(&self, candidate: &UpgradeCandidate) -> Result<Publication> {
        let first = candidate.name.chars().next().unwrap_or('_');
        let url = format!("{}/packages/{first}/{}/", self.base_url, candidate.name);
        match self.http.get(&url)? {
            FetchOutcome::NotFound => Ok(Publication::unknown()),
            FetchOutcome::Ok(html) => {
                let ts = parse_package_index(
                    &html,
                    &candidate.name,
                    &candidate.candidate_version,
                    &self.arch,
                );
                Ok(match ts {
                    Some(ts) => Publication::known(ts, PublicationBasis::Archive),
                    None => Publication::unknown(),
                })
            }
        }
    }
}

/// Find the earliest index timestamp for `name-version` in an Apache-style
/// directory listing of the archive package pool.
///
/// Rows look like:
/// `<a href="linux-6.8.2.arch1-2-x86_64.pkg.tar.zst">...</a>  2024-03-27 15:44  227M`
///
/// Package file names omit the epoch, so `1:2.0-1` matches `2.0-1`.
pub fn parse_package_index(html: &str, name: &str, version: &str, arch: &str) -> Option<i64> {
    let file_version = version.rsplit(':').next().unwrap_or(version);
    let prefix = format!("{name}-{file_version}-{arch}.pkg.tar.");

    let mut earliest: Option<i64> = None;
    let mut rest = html;
    while let Some(pos) = rest.find("href=\"") {
        rest = &rest[pos + 6..];
        let Some(end) = rest.find('"') else { break };
        let href = &rest[..end];
        rest = &rest[end..];
        // nginx percent-encodes characters like '+' (%2B) in hrefs.
        let href = percent_decode(href);
        if !href.starts_with(&prefix) || href.ends_with(".sig") {
            continue;
        }
        // The timestamp follows the anchor on the same line.
        let line_end = rest.find('\n').unwrap_or(rest.len());
        if let Some(ts) = scan_timestamp(&rest[..line_end]) {
            earliest = Some(earliest.map_or(ts, |e: i64| e.min(ts)));
        }
    }
    earliest
}

/// Minimal percent-decoding for index hrefs; invalid sequences pass through.
fn percent_decode(input: &str) -> String {
    if !input.contains('%') {
        return input.to_string();
    }
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Scan a string for the first timestamp pattern and convert it to epoch
/// seconds (UTC). Handles both `YYYY-MM-DD HH:MM` (Apache) and
/// `DD-Mon-YYYY HH:MM` (nginx autoindex, used by archive.archlinux.org).
fn scan_timestamp(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    for start in 0..bytes.len().saturating_sub(15) {
        let window = &text[start..];
        if let Some(ts) = parse_iso_prefix(window).or_else(|| parse_nginx_prefix(window)) {
            return Some(ts);
        }
    }
    None
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `DD-Mon-YYYY HH:MM`, e.g. `27-Mar-2024 15:44`.
fn parse_nginx_prefix(text: &str) -> Option<i64> {
    if text.len() < 16 {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let s = text.get(range)?;
        if s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse().ok()
        } else {
            None
        }
    };
    let sep = |idx: usize, c: char| text.as_bytes().get(idx) == Some(&(c as u8));

    let day = num(0..2)?;
    if !sep(2, '-') {
        return None;
    }
    let month = MONTHS
        .iter()
        .position(|m| text[3..].starts_with(m))
        .map(|p| p as i64 + 1)?;
    if !sep(6, '-') {
        return None;
    }
    let year = num(7..11)?;
    if !sep(11, ' ') {
        return None;
    }
    let hour = num(12..14)?;
    if !sep(14, ':') {
        return None;
    }
    let minute = num(15..17)?;
    if !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    Some(epoch_from_date_time(year, month, day, hour, minute))
}

/// `YYYY-MM-DD HH:MM`.
fn parse_iso_prefix(text: &str) -> Option<i64> {
    let num = |range: std::ops::Range<usize>| -> Option<i64> {
        let s = text.get(range)?;
        if s.bytes().all(|b| b.is_ascii_digit()) {
            s.parse().ok()
        } else {
            None
        }
    };
    let sep = |idx: usize, c: char| text.as_bytes().get(idx) == Some(&(c as u8));

    let year = num(0..4)?;
    if !sep(4, '-') {
        return None;
    }
    let month = num(5..7)?;
    if !sep(7, '-') {
        return None;
    }
    let day = num(8..10)?;
    if !sep(10, ' ') {
        return None;
    }
    let hour = num(11..13)?;
    if !sep(13, ':') {
        return None;
    }
    let minute = num(14..16)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    Some(epoch_from_date_time(year, month, day, hour, minute))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::FetchOutcome;
    use crate::model::{PackageSource, UpgradeCandidate};
    use std::collections::HashMap;

    const INDEX: &str = r#"<!DOCTYPE html>
<html><head><title>Index of /packages/l/linux/</title></head>
<body><h1>Index of /packages/l/linux/</h1>
<pre><a href="../">../</a>
<a href="linux-6.8.1.arch1-1-x86_64.pkg.tar.zst">linux-6.8.1.arch1-1-x86_64.pkg.tar.zst</a>  2024-03-16 21:05  130M
<a href="linux-6.8.1.arch1-1-x86_64.pkg.tar.zst.sig">linux-6.8.1.arch1-1-x86_64.pkg.tar.zst.sig</a>  2024-03-16 21:05  143
<a href="linux-6.8.2.arch1-1-x86_64.pkg.tar.zst">linux-6.8.2.arch1-1-x86_64.pkg.tar.zst</a>  2024-03-27 15:44  131M
<a href="linux-6.8.2.arch1-1-x86_64.pkg.tar.zst.sig">linux-6.8.2.arch1-1-x86_64.pkg.tar.zst.sig</a>  2024-03-27 15:44  143
<a href="linux-6.8.2.arch1-2-x86_64.pkg.tar.zst">linux-6.8.2.arch1-2-x86_64.pkg.tar.zst</a>  2024-03-29 09:12  131M
</pre></body></html>"#;

    #[test]
    fn finds_exact_version_timestamp() {
        let ts = parse_package_index(INDEX, "linux", "6.8.2.arch1-1", "x86_64");
        assert_eq!(ts, Some(epoch_from_date_time(2024, 3, 27, 15, 44)));
    }

    #[test]
    fn missing_version_is_none() {
        assert_eq!(
            parse_package_index(INDEX, "linux", "6.9.0-1", "x86_64"),
            None
        );
    }

    #[test]
    fn epoch_is_stripped_for_filename_matching() {
        let index = r#"<a href="foo-2.0-1-x86_64.pkg.tar.zst">x</a>  2024-01-05 10:30  1M"#;
        let ts = parse_package_index(index, "foo", "1:2.0-1", "x86_64");
        assert_eq!(ts, Some(epoch_from_date_time(2024, 1, 5, 10, 30)));
    }

    #[test]
    fn parses_nginx_autoindex_timestamps() {
        // The real archive.archlinux.org index format.
        let index = r#"<a href="linux-6.15.2.arch1-1-x86_64.pkg.tar.zst">linux-6.15.2.arch1-1-x86_64.pkg.tar.zst</a>            11-Jun-2025 07:31   141M"#;
        let ts = parse_package_index(index, "linux", "6.15.2.arch1-1", "x86_64");
        assert_eq!(ts, Some(epoch_from_date_time(2025, 6, 11, 7, 31)));
    }

    #[test]
    fn percent_encoded_hrefs_are_decoded() {
        // '+' in versions is served as %2B in hrefs by the archive.
        let index = r#"<a href="glibc-2.43%2Br37%2Bgfdf10644d6ee-1-x86_64.pkg.tar.zst">glibc-2.43+r37+gfdf10644d6ee-1-x86_64.pkg.tar.zst</a>  25-Jun-2026 16:04   10M"#;
        let ts = parse_package_index(index, "glibc", "2.43+r37+gfdf10644d6ee-1", "x86_64");
        assert_eq!(ts, Some(epoch_from_date_time(2026, 6, 25, 16, 4)));
    }

    #[test]
    fn prefix_collision_does_not_match_other_versions() {
        // "1.2-3" must not match "1.2-30".
        let index = r#"<a href="foo-1.2-30-x86_64.pkg.tar.zst">x</a>  2024-01-05 10:30  1M"#;
        assert_eq!(parse_package_index(index, "foo", "1.2-3", "x86_64"), None);
    }

    struct MockHttp {
        pages: HashMap<String, FetchOutcome>,
    }

    impl HttpClient for MockHttp {
        fn get(&self, url: &str) -> Result<FetchOutcome> {
            Ok(self
                .pages
                .get(url)
                .cloned()
                .unwrap_or(FetchOutcome::NotFound))
        }
    }

    fn candidate(name: &str, version: &str) -> UpgradeCandidate {
        UpgradeCandidate {
            name: name.to_string(),
            installed_version: "0-1".to_string(),
            candidate_version: version.to_string(),
            source: PackageSource::Repo,
        }
    }

    #[test]
    fn source_resolves_via_index() {
        let mut pages = HashMap::new();
        pages.insert(
            "https://example.test/packages/l/linux/".to_string(),
            FetchOutcome::Ok(INDEX.to_string()),
        );
        let http = MockHttp { pages };
        let source = ArchivePublicationSource::with_base_url(&http, "https://example.test");
        let p = source
            .publication(&candidate("linux", "6.8.2.arch1-2"))
            .unwrap();
        assert_eq!(
            p.published_at,
            Some(epoch_from_date_time(2024, 3, 29, 9, 12))
        );
        assert_eq!(p.basis, PublicationBasis::Archive);
    }

    #[test]
    fn unknown_package_is_unknown_not_error() {
        let http = MockHttp {
            pages: HashMap::new(),
        };
        let source = ArchivePublicationSource::with_base_url(&http, "https://example.test");
        let p = source
            .publication(&candidate("nosuchpkg", "1.0-1"))
            .unwrap();
        assert_eq!(p.published_at, None);
        assert_eq!(p.basis, PublicationBasis::Unknown);
    }
}
