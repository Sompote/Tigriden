//! Which monospaced families this machine actually has.
//!
//! cosmic-text answers a family it cannot find by falling through to the
//! platform's *interface* font — Segoe UI on Windows — so a config naming a
//! font that is not installed quietly renders the terminal in a variable-width
//! face. Every family the app hands to cosmic-text goes through [`resolve`]
//! first, and the Settings picker is built from what the font database really
//! holds rather than from a list baked into the build.

use std::cell::RefCell;

use cosmic_text::fontdb::Database;

/// Monospaced families to fall back to, best first, when the configured one is
/// not installed. The head of the list is also what a fresh config is written
/// with.
#[cfg(target_os = "macos")]
pub const FALLBACKS: &[&str] = &["Menlo", "SF Mono", "Monaco", "Andale Mono", "Courier New"];
#[cfg(target_os = "windows")]
pub const FALLBACKS: &[&str] = &["Cascadia Mono", "Consolas", "Lucida Console", "Courier New"];
#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
pub const FALLBACKS: &[&str] =
    &["DejaVu Sans Mono", "Liberation Mono", "Noto Sans Mono", "Ubuntu Mono", "monospace"];

thread_local! {
    /// The families the font database holds, read once from the first
    /// `FontSystem` built. Empty until then, which leaves [`resolve`] taking
    /// names on trust.
    static INSTALLED: RefCell<Index> = const { RefCell::new(Index::empty()) };
}

struct Index {
    /// (lowercased, as-installed) family names, sorted, for case-insensitive
    /// lookup that answers with the spelling the font database knows.
    all: Vec<(String, String)>,
    /// Monospaced families as the picker should list them.
    mono: Vec<String>,
}

impl Index {
    const fn empty() -> Self {
        Self { all: Vec::new(), mono: Vec::new() }
    }

    fn find(&self, family: &str) -> Option<String> {
        let needle = family.trim().to_lowercase();
        self.all
            .binary_search_by(|(name, _)| name.as_str().cmp(needle.as_str()))
            .ok()
            .map(|i| self.all[i].1.clone())
    }
}

/// Reads the installed families out of `db`. Cheap to call again — the first
/// window's font database is the one that counts, and every window loads the
/// same system fonts.
pub fn index(db: &Database) {
    INSTALLED.with(|slot| {
        if !slot.borrow().all.is_empty() {
            return;
        }
        let mut all: Vec<(String, String)> = Vec::new();
        let mut mono: Vec<String> = Vec::new();
        for face in db.faces() {
            for (name, _) in &face.families {
                all.push((name.to_lowercase(), name.clone()));
            }
            if face.monospaced {
                if let Some((name, _)) = face.families.first() {
                    mono.push(name.clone());
                }
            }
        }
        all.sort();
        all.dedup_by(|a, b| a.0 == b.0);
        // A machine whose fonts all lack the fixed-pitch flag would otherwise
        // leave the picker empty, with no way back to a working font.
        if mono.is_empty() {
            mono = all.iter().map(|(_, name)| name.clone()).collect();
        }
        mono.sort_by_key(|n| n.to_lowercase());
        mono.dedup();
        *slot.borrow_mut() = Index { all, mono };
    });
}

/// The family that will actually be drawn for `family`: itself when the
/// machine has it, else the first fallback the machine does have — never a
/// name that would land on the platform's proportional interface font.
pub fn resolve(family: &str) -> String {
    let family = family.trim();
    INSTALLED.with(|slot| {
        let index = slot.borrow();
        if index.all.is_empty() {
            return family.to_string();
        }
        if let Some(found) = index.find(family) {
            return found;
        }
        FALLBACKS
            .iter()
            .find_map(|f| index.find(f))
            .or_else(|| index.mono.first().cloned())
            .unwrap_or_else(|| family.to_string())
    })
}

/// What a fresh config names, before the font database is up.
pub fn default_family() -> &'static str {
    FALLBACKS[0]
}

/// The Settings picker's list.
pub fn monospace_families() -> Vec<String> {
    INSTALLED.with(|slot| slot.borrow().mono.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A database standing in for a machine that has one monospaced font and
    /// one proportional one, and none of this platform's usual fallbacks.
    fn sparse_db() -> Database {
        let mut db = Database::new();
        db.load_font_file("vendor/cosmic-text/fonts/FiraMono-Medium.ttf")
            .expect("the vendored mono font is there");
        db.load_font_file("vendor/cosmic-text/fonts/Inter-Regular.ttf")
            .expect("the vendored sans font is there");
        db
    }

    /// Each test thread gets its own index, so one test per thread may fill it.
    #[test]
    fn a_missing_family_resolves_to_one_that_is_there() {
        index(&sparse_db());
        assert_eq!(resolve("Fira Mono"), "Fira Mono", "an installed family is left alone");
        assert_eq!(resolve("fira MONO"), "Fira Mono", "spelled as the font database has it");
        // "Menlo" (or Consolas, or DejaVu Sans Mono) is not in this database,
        // and neither is any other fallback: rather than let cosmic-text answer
        // with the proportional interface font, take the mono font on hand.
        assert_eq!(
            resolve("Menlo"),
            "Fira Mono",
            "an absent family falls back to something monospaced"
        );
        assert_eq!(monospace_families(), vec!["Fira Mono"], "the picker lists only mono faces");
    }

    /// With no font database read yet, a name is taken on trust — the app must
    /// not rewrite a config before it can tell what is installed.
    #[test]
    fn names_are_trusted_until_the_database_is_read() {
        assert_eq!(resolve("Menlo"), "Menlo");
        assert!(monospace_families().is_empty());
    }
}
