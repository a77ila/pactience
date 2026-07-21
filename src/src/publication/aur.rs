//! AUR metadata via the RPC interface (v5).
//!
//! The AUR has **no per-version publication or build timestamp**. The only
//! time signal is the package base's `LastModified`, which changes on any
//! PKGBUILD edit and says nothing about when the current version appeared.
//! Age for AUR packages is therefore "unknown" by default; the
//! `aur_heuristic` config option opts into `LastModified` and every result
//! produced that way is labelled as a heuristic.

use std::collections::HashMap;

use serde::Deserialize;

use crate::db::DepSpec;
use crate::error::{Error, Result};
use crate::http::{FetchOutcome, HttpClient};
use crate::model::{Publication, PublicationBasis, UpgradeCandidate};

use super::PublicationSource;

pub const DEFAULT_BASE_URL: &str = "https://aur.archlinux.org";

/// Metadata for one AUR package base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AurInfo {
    pub name: String,
    pub version: String,
    pub last_modified: Option<i64>,
    pub depends: Vec<DepSpec>,
}

/// Fetch metadata for the given package names in a single multi-info call.
pub fn fetch_infos(
    http: &dyn HttpClient,
    base_url: &str,
    names: &[String],
) -> Result<HashMap<String, AurInfo>> {
    if names.is_empty() {
        return Ok(HashMap::new());
    }
    let query = names
        .iter()
        .map(|n| format!("arg[]={n}"))
        .collect::<Vec<_>>()
        .join("&");
    let url = format!("{base_url}/rpc/v5/info?{query}");
    let body = match http.get(&url)? {
        FetchOutcome::Ok(body) => body,
        FetchOutcome::NotFound => {
            return Err(Error::http(url, "AUR RPC endpoint not found"));
        }
    };
    parse_rpc_response(&body).map_err(|e| Error::parse("AUR RPC response", e))
}

#[derive(Deserialize)]
struct RpcResponse {
    #[serde(rename = "type")]
    kind: String,
    error: Option<String>,
    #[serde(default)]
    results: Vec<RpcResult>,
}

#[derive(Deserialize)]
struct RpcResult {
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Version")]
    version: String,
    #[serde(rename = "LastModified")]
    last_modified: Option<i64>,
    #[serde(rename = "Depends")]
    depends: Option<Vec<String>>,
}

fn parse_rpc_response(body: &str) -> std::result::Result<HashMap<String, AurInfo>, String> {
    let response: RpcResponse = serde_json::from_str(body).map_err(|e| e.to_string())?;
    if response.kind == "error" {
        return Err(response
            .error
            .unwrap_or_else(|| "unspecified AUR error".to_string()));
    }
    let mut infos = HashMap::new();
    for r in response.results {
        let depends = r
            .depends
            .unwrap_or_default()
            .iter()
            .filter_map(|d| DepSpec::parse(d))
            .collect();
        infos.insert(
            r.name.clone(),
            AurInfo {
                name: r.name,
                version: r.version,
                last_modified: r.last_modified,
                depends,
            },
        );
    }
    Ok(infos)
}

/// Publication source backed by pre-fetched AUR metadata.
pub struct AurPublicationSource<'a> {
    pub infos: &'a HashMap<String, AurInfo>,
    pub heuristic: bool,
}

impl PublicationSource for AurPublicationSource<'_> {
    fn publication(&self, candidate: &UpgradeCandidate) -> Result<Publication> {
        let Some(info) = self.infos.get(&candidate.name) else {
            return Ok(Publication::unknown());
        };
        if !self.heuristic {
            return Ok(Publication::unknown());
        }
        // LastModified only describes the *current* AUR version; if the RPC
        // reports a different version than our candidate, the field says
        // nothing about the candidate.
        if info.version != candidate.candidate_version {
            return Ok(Publication::unknown());
        }
        match info.last_modified {
            Some(ts) => Ok(Publication::known(ts, PublicationBasis::AurLastModified)),
            None => Ok(Publication::unknown()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PackageSource;

    #[test]
    fn parses_multiinfo_response() {
        let body = r#"{
            "version": 5,
            "type": "multiinfo",
            "resultcount": 1,
            "results": [{
                "ID": 123,
                "Name": "paru-bin",
                "Version": "2.0.4-1",
                "LastModified": 1711559040,
                "Depends": ["git", "pacman>=6.0"]
            }]
        }"#;
        let infos = parse_rpc_response(body).unwrap();
        let info = infos.get("paru-bin").unwrap();
        assert_eq!(info.version, "2.0.4-1");
        assert_eq!(info.last_modified, Some(1711559040));
        assert_eq!(info.depends.len(), 2);
        assert_eq!(info.depends[1].name, "pacman");
    }

    #[test]
    fn error_response_is_an_error() {
        let body = r#"{"version":5,"type":"error","resultcount":0,"results":[],"error":"Too many arguments"}"#;
        assert!(parse_rpc_response(body).is_err());
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse_rpc_response("<html>502</html>").is_err());
    }

    fn candidate(name: &str, version: &str) -> UpgradeCandidate {
        UpgradeCandidate {
            name: name.to_string(),
            installed_version: "1.0-1".to_string(),
            candidate_version: version.to_string(),
            source: PackageSource::Aur,
        }
    }

    fn infos() -> HashMap<String, AurInfo> {
        HashMap::from([(
            "paru-bin".to_string(),
            AurInfo {
                name: "paru-bin".to_string(),
                version: "2.0.4-1".to_string(),
                last_modified: Some(1711559040),
                depends: vec![],
            },
        )])
    }

    #[test]
    fn unknown_by_default() {
        let map = infos();
        let source = AurPublicationSource {
            infos: &map,
            heuristic: false,
        };
        let p = source
            .publication(&candidate("paru-bin", "2.0.4-1"))
            .unwrap();
        assert_eq!(p.published_at, None);
        assert_eq!(p.basis, PublicationBasis::Unknown);
    }

    #[test]
    fn heuristic_uses_last_modified_when_versions_match() {
        let map = infos();
        let source = AurPublicationSource {
            infos: &map,
            heuristic: true,
        };
        let p = source
            .publication(&candidate("paru-bin", "2.0.4-1"))
            .unwrap();
        assert_eq!(p.published_at, Some(1711559040));
        assert_eq!(p.basis, PublicationBasis::AurLastModified);
    }

    #[test]
    fn heuristic_rejects_stale_last_modified() {
        let map = infos();
        let source = AurPublicationSource {
            infos: &map,
            heuristic: true,
        };
        // RPC version has moved on from our candidate.
        let p = source
            .publication(&candidate("paru-bin", "2.0.3-1"))
            .unwrap();
        assert_eq!(p.published_at, None);
    }
}
