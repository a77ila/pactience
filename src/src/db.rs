//! Readers for pacman's sync databases (`/var/lib/pacman/sync/*.db`) and the
//! local database (`/var/lib/pacman/local/`).
//!
//! Sync databases are tar archives (gzip- or zstd-compressed) containing one
//! directory per package with a `desc` file in ALPM's `%FIELD%` format. All
//! contents are treated as untrusted input and parsed defensively.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::vercmp::vercmp;

/// Comparison operator in a versioned dependency (`foo>=1.2-3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepOp {
    Ge,
    Le,
    Eq,
    Gt,
    Lt,
}

/// A dependency declaration, optionally with a version constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepSpec {
    pub name: String,
    pub constraint: Option<(DepOp, String)>,
}

impl DepSpec {
    /// Parse one `%DEPENDS%` entry such as `glibc`, `foo>=1.2-3` or
    /// `bar=1.0: optional description`.
    pub fn parse(raw: &str) -> Option<DepSpec> {
        // Descriptions are separated by ": " and constraints never contain
        // whitespace, so the spec ends at the first space or colon.
        let spec = raw.split([' ', ':']).next().unwrap_or("").trim();
        if spec.is_empty() {
            return None;
        }
        for op in [">=", "<=", "=", ">", "<"] {
            if let Some(idx) = spec.find(op) {
                let name = &spec[..idx];
                let version = &spec[idx + op.len()..];
                if name.is_empty() || version.is_empty() {
                    return None;
                }
                let op = match op {
                    ">=" => DepOp::Ge,
                    "<=" => DepOp::Le,
                    "=" => DepOp::Eq,
                    ">" => DepOp::Gt,
                    _ => DepOp::Lt,
                };
                return Some(DepSpec {
                    name: name.to_string(),
                    constraint: Some((op, version.to_string())),
                });
            }
        }
        Some(DepSpec {
            name: spec.to_string(),
            constraint: None,
        })
    }

    /// Does `installed_version` satisfy this dependency?
    pub fn satisfied_by(&self, installed_version: &str) -> bool {
        match &self.constraint {
            None => true,
            Some((op, required)) => {
                let ord = vercmp(installed_version, required);
                match op {
                    DepOp::Ge => ord != std::cmp::Ordering::Less,
                    DepOp::Le => ord != std::cmp::Ordering::Greater,
                    DepOp::Eq => ord == std::cmp::Ordering::Equal,
                    DepOp::Gt => ord == std::cmp::Ordering::Greater,
                    DepOp::Lt => ord == std::cmp::Ordering::Less,
                }
            }
        }
    }
}

/// A `%PROVIDES%` entry: a virtual capability, optionally versioned
/// (`libfoo=1.0`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provide {
    pub name: String,
    pub version: Option<String>,
}

impl Provide {
    pub fn parse(raw: &str) -> Option<Provide> {
        let spec = raw.split([' ', ':']).next().unwrap_or("").trim();
        if spec.is_empty() {
            return None;
        }
        match spec.split_once('=') {
            Some((name, version)) if !name.is_empty() && !version.is_empty() => Some(Provide {
                name: name.to_string(),
                version: Some(version.to_string()),
            }),
            _ => Some(Provide {
                name: spec.to_string(),
                version: None,
            }),
        }
    }
}

/// Metadata for one package version from a sync database.
#[derive(Debug, Clone, Default)]
pub struct RepoPackageMeta {
    pub name: String,
    pub version: String,
    pub build_date: Option<i64>,
    pub depends: Vec<DepSpec>,
    pub provides: Vec<Provide>,
}

/// All packages known to the configured repositories, keyed by name.
#[derive(Debug, Default)]
pub struct SyncDb {
    pub packages: HashMap<String, RepoPackageMeta>,
}

impl SyncDb {
    /// Load every `*.db` file under `dir` (usually `/var/lib/pacman/sync`).
    /// Unreadable or malformed files are skipped: a single broken mirror DB
    /// must not take down the whole run.
    pub fn load(dir: &Path) -> Result<SyncDb> {
        let mut packages = HashMap::new();
        let entries = std::fs::read_dir(dir).map_err(|e| Error::io(dir.to_path_buf(), e))?;
        for entry in entries {
            let entry = entry.map_err(|e| Error::io(dir.to_path_buf(), e))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("db") {
                continue;
            }
            if let Ok(meta) = load_sync_db_file(&path) {
                for pkg in meta {
                    packages.insert(pkg.name.clone(), pkg);
                }
            }
        }
        Ok(SyncDb { packages })
    }

    pub fn get(&self, name: &str) -> Option<&RepoPackageMeta> {
        self.packages.get(name)
    }
}

/// Parse one sync database file into package metadata records.
fn load_sync_db_file(path: &Path) -> Result<Vec<RepoPackageMeta>> {
    let raw = std::fs::read(path).map_err(|e| Error::io(path.to_path_buf(), e))?;
    let decoded = decompress(&raw).map_err(|e| Error::parse(path.display().to_string(), e))?;
    let mut archive = tar::Archive::new(decoded.as_slice());
    let mut packages = Vec::new();
    let entries = archive
        .entries()
        .map_err(|e| Error::parse(path.display().to_string(), e.to_string()))?;
    for entry in entries {
        let mut entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // skip damaged entries, keep going
        };
        let is_desc = entry
            .path()
            .map(|p| p.file_name().and_then(|n| n.to_str()) == Some("desc"))
            .unwrap_or(false);
        if !is_desc {
            continue;
        }
        let mut content = String::new();
        if entry.read_to_string(&mut content).is_err() {
            continue;
        }
        let pkg = parse_desc(&content);
        if !pkg.name.is_empty() && !pkg.version.is_empty() {
            packages.push(pkg);
        }
    }
    Ok(packages)
}

/// Decompress gzip/zstd payloads, or pass plain tar through. Detection is by
/// magic bytes, not file extension.
fn decompress(raw: &[u8]) -> std::result::Result<Vec<u8>, String> {
    const GZIP: [u8; 2] = [0x1f, 0x8b];
    const ZSTD: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];
    if raw.starts_with(&GZIP) {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(raw)
            .read_to_end(&mut out)
            .map_err(|e| e.to_string())?;
        Ok(out)
    } else if raw.starts_with(&ZSTD) {
        let mut out = Vec::new();
        zstd::stream::Decoder::new(raw)
            .map_err(|e| e.to_string())?
            .read_to_end(&mut out)
            .map_err(|e| e.to_string())?;
        Ok(out)
    } else {
        Ok(raw.to_vec())
    }
}

/// Parse an ALPM `desc` file body into package metadata.
pub fn parse_desc(content: &str) -> RepoPackageMeta {
    let mut pkg = RepoPackageMeta::default();
    let mut section = String::new();
    for line in content.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('%') && line.ends_with('%') && line.len() > 2 {
            section = line.trim_matches('%').to_string();
            continue;
        }
        match section.as_str() {
            "NAME" => pkg.name = line.to_string(),
            "VERSION" => pkg.version = line.to_string(),
            "BUILDDATE" => pkg.build_date = line.parse::<i64>().ok(),
            "DEPENDS" => {
                if let Some(dep) = DepSpec::parse(line) {
                    pkg.depends.push(dep);
                }
            }
            "PROVIDES" => {
                if let Some(p) = Provide::parse(line) {
                    pkg.provides.push(p);
                }
            }
            _ => {}
        }
    }
    pkg
}

/// An installed package from the local pacman database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPackage {
    pub version: String,
    pub provides: Vec<Provide>,
}

/// Installed packages from the local pacman database, with an index over
/// their `%PROVIDES%` capabilities.
#[derive(Debug, Default)]
pub struct LocalDb {
    pub installed: HashMap<String, InstalledPackage>,
    /// capability name -> providing package names
    provides_index: HashMap<String, Vec<String>>,
}

impl LocalDb {
    /// Load `/var/lib/pacman/local` (one subdirectory per installed package,
    /// each with an uncompressed `desc` file).
    pub fn load(dir: &Path) -> Result<LocalDb> {
        let mut db = LocalDb::default();
        let entries = std::fs::read_dir(dir).map_err(|e| Error::io(dir.to_path_buf(), e))?;
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let desc: PathBuf = entry.path().join("desc");
            let Ok(content) = std::fs::read_to_string(&desc) else {
                continue;
            };
            let pkg = parse_desc(&content);
            if !pkg.name.is_empty() && !pkg.version.is_empty() {
                db.insert(
                    pkg.name.clone(),
                    InstalledPackage {
                        version: pkg.version,
                        provides: pkg.provides,
                    },
                );
            }
        }
        Ok(db)
    }

    /// Insert a package, maintaining the provides index.
    pub fn insert(&mut self, name: String, pkg: InstalledPackage) {
        for provide in &pkg.provides {
            self.provides_index
                .entry(provide.name.clone())
                .or_default()
                .push(name.clone());
        }
        self.installed.insert(name, pkg);
    }

    pub fn version_of(&self, name: &str) -> Option<&str> {
        self.installed.get(name).map(|p| p.version.as_str())
    }

    /// Packages installed on the system that provide the given capability.
    pub fn providers_of(&self, capability: &str) -> &[String] {
        self.provides_index
            .get(capability)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Version with which `name` provides `capability`, if versioned.
    pub fn provided_version(&self, name: &str, capability: &str) -> Option<Option<&str>> {
        self.installed.get(name)?.provides.iter().find_map(|p| {
            if p.name == capability {
                Some(p.version.as_deref())
            } else {
                None
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_depspec_without_constraint() {
        let dep = DepSpec::parse("glibc").unwrap();
        assert_eq!(dep.name, "glibc");
        assert_eq!(dep.constraint, None);
        assert!(dep.satisfied_by("2.39-1"));
    }

    #[test]
    fn parses_versioned_depspecs() {
        let dep = DepSpec::parse("foo>=1.2-3").unwrap();
        assert_eq!(dep.name, "foo");
        assert_eq!(dep.constraint, Some((DepOp::Ge, "1.2-3".to_string())));
        assert!(dep.satisfied_by("1.2-3"));
        assert!(dep.satisfied_by("1.3-1"));
        assert!(!dep.satisfied_by("1.1-9"));

        let dep = DepSpec::parse("bar=2.0-1").unwrap();
        assert!(dep.satisfied_by("2.0-1"));
        assert!(!dep.satisfied_by("2.0-2"));

        let dep = DepSpec::parse("baz<2.0").unwrap();
        assert!(dep.satisfied_by("1.9-1"));
        assert!(!dep.satisfied_by("2.0-1"));
    }

    #[test]
    fn depspec_strips_descriptions() {
        let dep = DepSpec::parse("foo>=1.0: needed for bar").unwrap();
        assert_eq!(dep.name, "foo");
        assert_eq!(dep.constraint, Some((DepOp::Ge, "1.0".to_string())));
    }

    #[test]
    fn rejects_garbage_depspecs() {
        assert!(DepSpec::parse("").is_none());
        assert!(DepSpec::parse("  ").is_none());
        assert!(DepSpec::parse(">=1.0").is_none());
        assert!(DepSpec::parse("foo>=").is_none());
    }

    #[test]
    fn parses_provides() {
        assert_eq!(
            Provide::parse("sh").unwrap(),
            Provide {
                name: "sh".into(),
                version: None
            }
        );
        assert_eq!(
            Provide::parse("libfoo.so=1.0-64").unwrap(),
            Provide {
                name: "libfoo.so".into(),
                version: Some("1.0-64".into())
            }
        );
    }

    #[test]
    fn parses_desc_content() {
        let content = "%NAME%\nfoo\n\n%VERSION%\n1.2-3\n\n%BUILDDATE%\n1712345678\n\n%DEPENDS%\nglibc\nbar>=1.0: why not\n\n%PROVIDES%\nsh\nlibfoo=1.2\n\n";
        let pkg = parse_desc(content);
        assert_eq!(pkg.name, "foo");
        assert_eq!(pkg.version, "1.2-3");
        assert_eq!(pkg.build_date, Some(1712345678));
        assert_eq!(pkg.depends.len(), 2);
        assert_eq!(pkg.depends[1].name, "bar");
        assert_eq!(pkg.provides.len(), 2);
        assert_eq!(pkg.provides[1].version, Some("1.2".to_string()));
    }

    #[test]
    fn desc_with_bad_builddate_yields_none() {
        let pkg = parse_desc("%NAME%\nfoo\n\n%VERSION%\n1-1\n\n%BUILDDATE%\nsoon\n");
        assert_eq!(pkg.build_date, None);
    }

    #[test]
    fn loads_plain_tar_sync_db() {
        let dir = std::env::temp_dir().join(format!("aag-syncdb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("core.db");

        // Build an uncompressed tar with one package desc.
        let mut builder = tar::Builder::new(Vec::new());
        let desc = "%NAME%\nfoo\n\n%VERSION%\n1.2-3\n\n%BUILDDATE%\n1712345678\n\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(desc.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "foo-1.2-3/desc", desc.as_bytes())
            .unwrap();
        let tar_bytes = builder.into_inner().unwrap();
        std::fs::write(&db_path, tar_bytes).unwrap();

        let db = SyncDb::load(&dir).unwrap();
        let pkg = db.get("foo").unwrap();
        assert_eq!(pkg.version, "1.2-3");
        assert_eq!(pkg.build_date, Some(1712345678));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn decompresses_gzip() {
        use flate2::Compression;
        use flate2::write::GzEncoder;
        use std::io::Write;
        let mut enc = GzEncoder::new(Vec::new(), Compression::fast());
        enc.write_all(b"hello tar").unwrap();
        let compressed = enc.finish().unwrap();
        assert_eq!(decompress(&compressed).unwrap(), b"hello tar");
    }
}
