//! Font families installed on the system, to choose a profile's font from.

use std::sync::mpsc::{Receiver, channel};
use std::thread;

use super::platform;

/// An installed font family.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Family {
    pub name: String,
    /// Whether all its characters take the same width, as a terminal's font should.
    pub mono: bool,
}

/// List the installed families in the background, calling `wake` once they are listed.
pub fn list(wake: impl Fn() + Send + 'static) -> Receiver<Vec<Family>> {
    let (sender, receiver) = channel();
    thread::spawn(move || {
        let _ = sender.send(families(platform::fonts()));
        wake();
    });
    receiver
}

/// Each family once, sorted by name, without the system's hidden ones.
fn families(fonts: Vec<(String, bool)>) -> Vec<Family> {
    let mut families: Vec<_> = fonts
        .into_iter()
        .map(|(name, mono)| Family { name: name.trim().into(), mono })
        .filter(|family| !family.name.is_empty() && !family.name.starts_with('.'))
        .collect();
    families.sort_by_cached_key(|family| (family.name.to_lowercase(), family.name.clone()));
    families.dedup_by(|family, kept| {
        let same = family.name == kept.name;
        kept.mono |= same && family.mono;
        same
    });
    families
}

/// Families whose names contain `query`, those starting with it first, and only monospaced ones
/// unless `all`.
pub fn matching<'a>(families: &'a [Family], query: &str, all: bool) -> Vec<&'a Family> {
    let query = query.trim().to_lowercase();
    let mut matching: Vec<_> = families
        .iter()
        .filter(|family| all || family.mono)
        .filter(|family| family.name.to_lowercase().contains(&query))
        .collect();
    matching.sort_by_key(|family| !family.name.to_lowercase().starts_with(&query));
    matching
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family(name: &str, mono: bool) -> Family {
        Family { name: name.into(), mono }
    }

    #[test]
    fn families_are_sorted_once_each() {
        let fonts = [("Noto Sans", false), ("DejaVu Sans Mono", true), ("noto mono", true)];
        let fonts = fonts.into_iter().chain([("DejaVu Sans Mono", false), (".Hidden", true)]);
        let families = families(fonts.map(|(name, mono)| (name.into(), mono)).collect());
        let expected = [
            family("DejaVu Sans Mono", true),
            family("noto mono", true),
            family("Noto Sans", false),
        ];
        assert_eq!(families, expected);
    }

    #[test]
    fn matches_start_with_the_query_first() {
        let families =
            [family("Adwaita Mono", true), family("Mono Sans", false), family("Monoid", true)];
        let names = |all| -> Vec<_> {
            matching(&families, " MONO", all).into_iter().map(|f| f.name.as_str()).collect()
        };
        assert_eq!(names(false), ["Monoid", "Adwaita Mono"]);
        assert_eq!(names(true), ["Mono Sans", "Monoid", "Adwaita Mono"]);
        assert_eq!(matching(&families, "", false).len(), 2);
    }
}
