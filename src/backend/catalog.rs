//! Operating systems known to libosinfo, the database virt-install uses for defaults.
use std::sync::OnceLock;

pub struct OsEntry {
    /// libosinfo short id, as virt-install's `--os-variant` takes it ("win11").
    pub id: String,
    pub name: String,
    /// libosinfo URI, as domain metadata stores it ("http://microsoft.com/win/11").
    pub uri: String,
}

/// Every libosinfo operating system, newest release first. Falls back to a short list
/// when `osinfo-query` is unavailable. Loaded once.
pub fn os_catalog() -> &'static [OsEntry] {
    static CATALOG: OnceLock<Vec<OsEntry>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut list = vec![OsEntry {
            id: "generic".into(),
            name: "Other".into(),
            uri: String::new(),
        }];
        let output = std::process::Command::new("osinfo-query")
            .args(["os", "--fields=short-id,name,release-date,id"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let mut rows: Vec<(String, String, String, String)> = output
            .as_deref()
            .unwrap_or("")
            .lines()
            .skip(2)
            .filter_map(|line| {
                let mut cols = line.split('|').map(str::trim);
                let id = cols.next()?.to_owned();
                let name = cols.next()?.to_owned();
                let date = cols.next().unwrap_or("").to_owned();
                let uri = cols.next().unwrap_or("").to_owned();
                (!id.is_empty() && !name.is_empty()).then_some((id, name, date, uri))
            })
            .collect();
        if rows.is_empty() {
            rows = [
                ("win11", "Microsoft Windows 11", "http://microsoft.com/win/11"),
                ("win10", "Microsoft Windows 10", "http://microsoft.com/win/10"),
                ("ubuntu24.04", "Ubuntu 24.04 LTS", "http://ubuntu.com/ubuntu/24.04"),
                ("fedora41", "Fedora Linux 41", "http://fedoraproject.org/fedora/41"),
                ("debian12", "Debian 12", "http://debian.org/debian/12"),
                ("archlinux", "Arch Linux", "http://archlinux.org/archlinux/rolling"),
                ("freebsd14.0", "FreeBSD 14.0", "http://freebsd.org/freebsd/14.0"),
            ]
            .into_iter()
            .map(|(id, name, uri)| (id.into(), name.into(), String::new(), uri.into()))
            .collect();
        }
        rows.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));
        list.extend(rows.into_iter().map(|(id, name, _, uri)| OsEntry {
            id,
            name,
            uri,
        }));
        list
    })
}

/// Best catalog match for an installation image name, such as
/// "ubuntu-24.04.1-desktop-amd64" or "Win11_24H2_English_x64".
pub fn infer_os(stem: &str) -> Option<&'static str> {
    let compact: String =
        stem.to_ascii_lowercase().chars().filter(|c| !matches!(c, '-' | '_' | ' ')).collect();
    let catalog = os_catalog();
    // An exact short-id inside the name wins; the longest one is the most specific.
    if let Some(entry) = catalog
        .iter()
        .filter(|e| e.id != "generic" && e.id.len() > 3 && compact.contains(&e.id))
        .max_by_key(|e| e.id.len())
    {
        return Some(&entry.id);
    }
    // Otherwise the newest release of the named family, e.g. "fedora" -> fedora41.
    catalog
        .iter()
        .find(|e| {
            let family: String = e.id.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
            family.len() > 3 && compact.contains(&family)
        })
        .map(|e| e.id.as_str())
}

fn prefix(id: &str) -> String {
    let p: String = id.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    if p.is_empty() {
        id.to_owned()
    } else {
        p
    }
}

/// Family label for each id prefix: the words its versions share ("Microsoft Windows").
fn labels() -> &'static std::collections::HashMap<String, String> {
    static LABELS: OnceLock<std::collections::HashMap<String, String>> = OnceLock::new();
    LABELS.get_or_init(|| {
        let mut groups: Vec<(String, Vec<&str>)> = Vec::new();
        for entry in os_catalog().iter().skip(1) {
            let key = prefix(&entry.id);
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, names)) => names.push(&entry.name),
                None => groups.push((key, vec![&entry.name])),
            }
        }
        groups
            .into_iter()
            .map(|(key, names)| {
                let mut common: Vec<&str> = names[0].split_whitespace().collect();
                for name in &names[1..] {
                    let words: Vec<&str> = name.split_whitespace().collect();
                    let shared = common.iter().zip(&words).take_while(|(a, b)| a == b).count();
                    common.truncate(shared);
                }
                while common.len() > 1
                    && common.last().is_some_and(|w| w.chars().any(|c| c.is_ascii_digit()))
                {
                    common.pop();
                }
                let label = if common.is_empty() {
                    names[0].split_whitespace().next().unwrap_or(&key).to_owned()
                } else {
                    common.join(" ")
                };
                (key, label)
            })
            .collect()
    })
}

/// Family of a catalog id, keyed by its visible label so families sharing a name merge.
pub fn family_of(id: &str) -> String {
    if id == "generic" {
        return "generic".into();
    }
    labels().get(&prefix(id)).cloned().unwrap_or_else(|| prefix(id))
}

/// Operating system families: "Other" first, then alphabetical.
pub fn os_families() -> &'static [(String, String)] {
    static FAMILIES: OnceLock<Vec<(String, String)>> = OnceLock::new();
    FAMILIES.get_or_init(|| {
        let mut names: Vec<String> = labels().values().cloned().collect();
        names.sort_by_key(|n| n.to_lowercase());
        names.dedup();
        let mut families = vec![("generic".to_owned(), "Other".to_owned())];
        families.extend(names.into_iter().map(|n| (n.clone(), n)));
        families
    })
}

/// Versions within a family, newest first.
pub fn os_versions(family: &str) -> Vec<(String, String)> {
    if family == "generic" {
        return vec![("generic".into(), "Generic".into())];
    }
    os_catalog()
        .iter()
        .skip(1)
        .filter(|e| family_of(&e.id) == family)
        .map(|e| (e.id.clone(), e.name.clone()))
        .collect()
}

/// Catalog entry for a libosinfo URI stored in domain metadata.
pub fn os_by_uri(uri: &str) -> Option<&'static OsEntry> {
    (!uri.is_empty()).then(|| os_catalog().iter().find(|e| e.uri == uri)).flatten()
}

/// Display name for a machine's operating system, from its libosinfo URI.
pub fn os_name(uri: &str) -> Option<&'static str> {
    os_by_uri(uri).map(|e| e.name.as_str())
}
