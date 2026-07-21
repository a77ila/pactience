//! Package version comparison compatible with libalpm's `alpm_pkg_vercmp`.
//!
//! Versions have the form `[epoch:]upstream[-pkgrel]`. Epochs compare
//! numerically and dominate; upstream and pkgrel use the RPM-style segment
//! comparison that pacman inherited (`rpmvercmp`).

use std::cmp::Ordering;

/// Compare two full package versions the way pacman does.
pub fn vercmp(a: &str, b: &str) -> Ordering {
    if a == b {
        return Ordering::Equal;
    }
    let (epoch_a, ver_a, rel_a) = split_evr(a);
    let (epoch_b, ver_b, rel_b) = split_evr(b);

    match epoch_a.cmp(&epoch_b) {
        Ordering::Equal => {}
        other => return other,
    }

    match rpmvercmp(ver_a.as_bytes(), ver_b.as_bytes()) {
        Ordering::Equal => {}
        other => return other,
    }

    // libalpm only compares pkgrels when both are present.
    match (rel_a, rel_b) {
        (Some(ra), Some(rb)) => rpmvercmp(ra.as_bytes(), rb.as_bytes()),
        _ => Ordering::Equal,
    }
}

/// Split `[epoch:]version[-release]` into its parts. A missing epoch is 0.
fn split_evr(v: &str) -> (u64, &str, Option<&str>) {
    let mut rest = v;
    let mut epoch = 0u64;
    if let Some(colon) = rest.find(':')
        && colon > 0
        && rest[..colon].bytes().all(|b| b.is_ascii_digit())
    {
        epoch = rest[..colon].parse().unwrap_or(0);
        rest = &rest[colon + 1..];
    }
    // The release is everything after the last '-'; upstream pkgver may not
    // contain '-'.
    match rest.rfind('-') {
        Some(dash) => (epoch, &rest[..dash], Some(&rest[dash + 1..])),
        None => (epoch, rest, None),
    }
}

/// Faithful port of libalpm's `rpmvercmp` (itself from RPM 4.0.4).
fn rpmvercmp(a: &[u8], b: &[u8]) -> Ordering {
    let mut one = 0usize; // cursor into a
    let mut two = 0usize; // cursor into b

    while one < a.len() && two < b.len() {
        let seg_start_one = {
            let mut i = one;
            while i < a.len() && !a[i].is_ascii_alphanumeric() {
                i += 1;
            }
            i
        };
        let seg_start_two = {
            let mut i = two;
            while i < b.len() && !b[i].is_ascii_alphanumeric() {
                i += 1;
            }
            i
        };

        // Ran off the end of either string.
        if seg_start_one >= a.len() || seg_start_two >= b.len() {
            one = seg_start_one;
            two = seg_start_two;
            break;
        }

        // Different numbers of separators skipped: the version with more
        // separators is older.
        let skipped_one = seg_start_one - one;
        let skipped_two = seg_start_two - two;
        if skipped_one != skipped_two {
            return if skipped_one < skipped_two {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }

        // Grab the next alpha or numeric segment from each side.
        let isnum = a[seg_start_one].is_ascii_digit();
        let mut end_one = seg_start_one;
        let mut end_two = seg_start_two;
        if isnum {
            while end_one < a.len() && a[end_one].is_ascii_digit() {
                end_one += 1;
            }
            while end_two < b.len() && b[end_two].is_ascii_digit() {
                end_two += 1;
            }
        } else {
            while end_one < a.len() && a[end_one].is_ascii_alphabetic() {
                end_one += 1;
            }
            while end_two < b.len() && b[end_two].is_ascii_alphabetic() {
                end_two += 1;
            }
        }

        let seg_one = &a[seg_start_one..end_one];
        let seg_two = &b[seg_start_two..end_two];

        if isnum {
            // Numeric segments: strip leading zeros, longer digit run wins,
            // then plain lexicographic on equal lengths.
            let s1 = strip_zeros(seg_one);
            let s2 = strip_zeros(seg_two);
            match s1.len().cmp(&s2.len()) {
                Ordering::Equal => match s1.cmp(s2) {
                    Ordering::Equal => {}
                    other => return other,
                },
                other => return other,
            }
        } else {
            // Alpha segments compare lexicographically. If only one side is
            // alpha (the other exhausted or numeric), the numeric side is
            // newer; `cmp` on byte slices reproduces this because an empty
            // slice sorts before any non-empty one.
            match seg_one.cmp(seg_two) {
                Ordering::Equal => {}
                other => return other,
            }
        }

        one = end_one;
        two = end_two;
    }

    // Whichever side still has segments left is the newer version.
    match (one < a.len(), two < b.len()) {
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => Ordering::Equal,
    }
}

fn strip_zeros(seg: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < seg.len() && seg[i] == b'0' {
        i += 1;
    }
    &seg[i..]
}

#[cfg(test)]
mod tests {
    use super::*;
    use Ordering::{Equal, Greater, Less};

    #[test]
    fn equal_versions() {
        assert_eq!(vercmp("1.0-1", "1.0-1"), Equal);
        assert_eq!(vercmp("1:2.0-3", "1:2.0-3"), Equal);
    }

    #[test]
    fn basic_ordering() {
        assert_eq!(vercmp("1.0-1", "1.1-1"), Less);
        assert_eq!(vercmp("1.1-1", "1.0-1"), Greater);
        assert_eq!(vercmp("1.0-1", "1.0-2"), Less);
        assert_eq!(vercmp("2.0-1", "10.0-1"), Less); // numeric, not lexical
    }

    #[test]
    fn epoch_dominates() {
        assert_eq!(vercmp("1:1.0-1", "9.9-9"), Greater);
        assert_eq!(vercmp("2:0.1-1", "1:99.9-1"), Greater);
        assert_eq!(vercmp("1.0-1", "1:0.1-1"), Less);
    }

    #[test]
    fn alpha_segments() {
        assert_eq!(vercmp("1.0alpha-1", "1.0beta-1"), Less);
        // rpm/alpm semantics: extra segments make a version *newer*, so
        // "1.0rc1" > "1.0" (this is why rc→final transitions need an epoch).
        assert_eq!(vercmp("1.0alpha-1", "1.0-1"), Greater);
        assert_eq!(vercmp("1.0a-1", "1.0-1"), Greater);
        // Alpha segments compare newer than numeric ones in rpmvercmp.
        assert_eq!(vercmp("1.0-1", "1.a-1"), Less);
    }

    #[test]
    fn pkgrel_compared_when_upstream_equal() {
        assert_eq!(vercmp("6.8.1.arch1-1", "6.8.1.arch1-2"), Less);
        assert_eq!(vercmp("6.8.1.arch1-10", "6.8.1.arch1-2"), Greater);
    }

    #[test]
    fn leading_zeros_are_ignored() {
        assert_eq!(vercmp("1.01-1", "1.1-1"), Equal);
        assert_eq!(vercmp("1.002-1", "1.2-1"), Equal);
    }

    #[test]
    fn real_world_cases() {
        assert_eq!(vercmp("1.34-1", "1.35-1"), Less); // shadow utils style
        assert_eq!(vercmp("2.40+r14+gc3-1", "2.40+r16+gc4-1"), Less);
        assert_eq!(vercmp("2024.01-1", "2024.02-1"), Less);
    }
}
