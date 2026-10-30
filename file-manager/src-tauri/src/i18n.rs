//! The backend's messages in the window's language. The window tells it
//! which (`set_language`, from Settings or the system's preferences); the
//! messages are flat keys in `locales/<language>.json`, with `{named}`
//! values filled by the `t!` macro. Errors of the system (`io::Error`) are
//! put in words for the common kinds (not found, permission denied, disk
//! full ...); the rarer ones keep the system's own text.

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

/// The languages, in the order of `CATALOGS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En = 0,
    PtBr = 1,
}

const SOURCES: [&str; 2] = [include_str!("../locales/en.json"), include_str!("../locales/pt-BR.json")];

static CURRENT: AtomicU8 = AtomicU8::new(Lang::En as u8);
static CATALOGS: OnceLock<[HashMap<String, String>; 2]> = OnceLock::new();

fn catalogs() -> &'static [HashMap<String, String>; 2] {
    CATALOGS.get_or_init(|| SOURCES.map(|s| serde_json::from_str(s).expect("a catalog is valid JSON")))
}

impl Lang {
    /// "pt-BR", "pt", "en-US" ...; English for anything else.
    pub fn from_tag(tag: &str) -> Lang {
        if tag.to_ascii_lowercase().starts_with("pt") {
            Lang::PtBr
        } else {
            Lang::En
        }
    }
}

pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::SeqCst);
}

pub fn current() -> Lang {
    if CURRENT.load(Ordering::SeqCst) == Lang::PtBr as u8 {
        Lang::PtBr
    } else {
        Lang::En
    }
}

/// The message `key` in `lang` (English when that language lacks it, the key itself when no language has it).
pub fn tr_in(lang: Lang, key: &str) -> String {
    let c = catalogs();
    c[lang as usize].get(key).or_else(|| c[Lang::En as usize].get(key)).cloned().unwrap_or_else(|| key.to_string())
}

pub fn tr(key: &str) -> String {
    tr_in(current(), key)
}

/// `text` with each `{name}` replaced by its value.
pub fn fill(text: String, values: &[(&str, String)]) -> String {
    values.iter().fold(text, |acc, (name, value)| acc.replace(&format!("{{{name}}}"), value))
}

/// A message by key, with its values: `t!("ops.copy_failed", name = n, error = e)`.
#[macro_export]
macro_rules! t {
    ($key:literal) => {
        $crate::i18n::tr($key)
    };
    ($key:literal, $($name:ident = $value:expr),+ $(,)?) => {
        $crate::i18n::fill($crate::i18n::tr($key), &[$((stringify!($name), ($value).to_string())),+])
    };
}

/// An error of the system in words: the common kinds translated, the others as the system says.
pub fn io_text(e: &io::Error) -> String {
    use io::ErrorKind as K;
    let key = match e.kind() {
        K::NotFound => "io.not_found",
        K::PermissionDenied => "io.permission_denied",
        K::AlreadyExists => "io.already_exists",
        K::StorageFull => "io.storage_full",
        K::QuotaExceeded => "io.quota_exceeded",
        K::ReadOnlyFilesystem => "io.read_only",
        K::DirectoryNotEmpty => "io.directory_not_empty",
        K::NotADirectory => "io.not_a_directory",
        K::IsADirectory => "io.is_a_directory",
        K::ResourceBusy => "io.busy",
        K::FileTooLarge => "io.too_large",
        K::InvalidFilename => "io.invalid_filename",
        K::CrossesDevices => "io.crosses_devices",
        K::TimedOut => "io.timed_out",
        _ => return e.to_string(),
    };
    tr(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placeholders(s: &str) -> Vec<String> {
        let mut v: Vec<String> = s.split('{').skip(1).filter_map(|p| p.split_once('}').map(|(n, _)| n.to_string())).collect();
        v.sort();
        v
    }

    #[test]
    fn every_language_has_every_key_with_the_same_values() {
        let c = catalogs();
        let (en, pt) = (&c[Lang::En as usize], &c[Lang::PtBr as usize]);
        let mut missing: Vec<&String> = en.keys().filter(|k| !pt.contains_key(*k)).collect();
        missing.sort();
        assert!(missing.is_empty(), "missing in pt-BR: {missing:?}");
        let extra: Vec<&String> = pt.keys().filter(|k| !en.contains_key(*k)).collect();
        assert!(extra.is_empty(), "not in en: {extra:?}");
        for (k, v) in en {
            assert_eq!(placeholders(v), placeholders(&pt[k]), "the values of {k}");
        }
    }

    #[test]
    fn messages_fill_their_values_and_fall_back() {
        assert_eq!(fill("Could not copy \"{name}\": {error}".into(), &[("name", "a.txt".into()), ("error", "x".into())]), "Could not copy \"a.txt\": x");
        assert_eq!(tr_in(Lang::PtBr, "no.such.key"), "no.such.key");
        assert_eq!(tr_in(Lang::En, "io.permission_denied"), "permission denied");
        assert_eq!(tr_in(Lang::PtBr, "io.permission_denied"), "permissão negada");
        assert_eq!(Lang::from_tag("pt_BR.UTF-8"), Lang::PtBr);
        assert_eq!(Lang::from_tag("de-DE"), Lang::En);
    }
}
