//! Games: reading a game folder's `MicrosoftGame.config` (name, identity, logo) and finding
//! the game folders inside a library folder.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use quick_xml::XmlVersion;
use quick_xml::events::{BytesStart, Event};

/// The manifest every GDK game package has at its root.
const MANIFEST: &str = "MicrosoftGame.config";
/// The logo used when the manifest names none.
const DEFAULT_LOGO: &str = "xbs_logo_480.png";

/// How a game came into the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Found in a library folder.
    Library,
    /// Added on its own.
    Manual,
}

#[derive(Debug, Clone)]
pub struct Game {
    /// Stable key for per-game settings: the folder's path.
    pub id: String,
    /// The package folder (the one holding `MicrosoftGame.config`).
    pub root: PathBuf,
    /// The name people see.
    pub name: String,
    pub publisher: Option<String>,
    pub version: Option<String>,
    /// The name of XCL's data folder for this game (`<data root>/<title>`); computed as XCL does.
    pub title: String,
    pub logo: Option<PathBuf>,
    /// The manifest's tile background colour.
    pub background: Option<[u8; 3]>,
    pub source: Source,
    /// The folder is gone (a manually added game that was moved or deleted).
    pub missing: bool,
}

/// What a manifest says about the game.
#[derive(Debug, Default, PartialEq)]
struct Manifest {
    identity: Option<String>,
    version: Option<String>,
    display_name: Option<String>,
    publisher: Option<String>,
    logo_480: Option<String>,
    logo_150: Option<String>,
    background: Option<String>,
}

impl Game {
    /// Reads the game in `root`. Without a manifest the folder name stands in for the name.
    pub fn load(root: &Path, source: Source) -> Game {
        let manifest_path = find_manifest(root);
        let manifest = manifest_path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|raw| decode_text(&raw))
            .map(|xml| parse_manifest(&xml))
            .unwrap_or_default();
        let folder_name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = manifest
            .display_name
            .clone()
            .filter(|n| !n.trim().is_empty() && !n.starts_with("ms-resource:"))
            .unwrap_or_else(|| folder_name.clone());
        let title = sanitize(manifest.identity.as_deref().unwrap_or(&folder_name));
        let logo = [
            manifest.logo_480.as_deref(),
            Some(DEFAULT_LOGO),
            manifest.logo_150.as_deref(),
        ]
        .into_iter()
        .flatten()
        .map(|rel| root.join(rel.replace('\\', "/")))
        .find(|p| p.is_file());
        Game {
            id: root.to_string_lossy().into_owned(),
            root: root.to_path_buf(),
            name,
            publisher: manifest
                .publisher
                .filter(|p| !p.starts_with("ms-resource:")),
            version: manifest.version,
            title,
            logo,
            background: manifest.background.as_deref().and_then(parse_color),
            source,
            missing: !root.is_dir(),
        }
    }

    /// XCL's data folder for this game.
    pub fn data_dir(&self, data_root: &Path) -> PathBuf {
        crate::paths::child_ci(data_root, &self.title)
    }
}

/// True when `dir` is a game's package folder.
pub fn is_game_dir(dir: &Path) -> bool {
    find_manifest(dir).is_some()
}

/// The game folder for a picked file or folder: a picked `MicrosoftGame.config` or `.exe` stands
/// for its folder (or the nearest folder above it with a manifest).
pub fn game_root_for(picked: &Path) -> PathBuf {
    if picked.is_dir() {
        return picked.to_path_buf();
    }
    let start = picked.parent().unwrap_or(picked);
    start
        .ancestors()
        .take(5)
        .find(|d| is_game_dir(d))
        .unwrap_or(start)
        .to_path_buf()
}

/// All games: those in the library folders (the folders themselves and their sub-folders up to
/// two levels deep), then the manually added ones, without duplicates, sorted by name.
pub fn scan(library_dirs: &[PathBuf], manual: &[PathBuf]) -> Vec<Game> {
    let mut seen = HashSet::new();
    let mut games = Vec::new();
    for lib in library_dirs {
        for root in game_dirs_in(lib) {
            if seen.insert(root.clone()) {
                games.push(Game::load(&root, Source::Library));
            }
        }
    }
    for root in manual {
        if seen.insert(root.clone()) {
            games.push(Game::load(root, Source::Manual));
        }
    }
    games.sort_by_cached_key(|g| g.name.to_lowercase());
    games
}

fn game_dirs_in(lib: &Path) -> Vec<PathBuf> {
    if is_game_dir(lib) {
        return vec![lib.to_path_buf()];
    }
    let mut out = Vec::new();
    for dir in sub_dirs(lib) {
        if is_game_dir(&dir) {
            out.push(dir);
        } else {
            // a game kept one folder deeper ("Game/Content", "Publisher/Game")
            out.extend(sub_dirs(&dir).into_iter().filter(|d| is_game_dir(d)));
        }
    }
    out
}

fn sub_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    dirs
}

/// The manifest in `dir`, whatever the case of its name (packages ship `MicrosoftGame.Config`).
fn find_manifest(dir: &Path) -> Option<PathBuf> {
    let exact = dir.join(MANIFEST);
    if exact.is_file() {
        return Some(exact);
    }
    if cfg!(windows) {
        return None;
    }
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .find(|e| {
            e.file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(MANIFEST)
        })
        .map(|e| e.path())
        .filter(|p| p.is_file())
}

/// The data folder name XCL uses for a title (`launcher::sanitize` in XCL).
pub fn sanitize(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    if t.is_empty() { "title".into() } else { t }
}

/// Manifests are UTF-8 or UTF-16 with a byte order mark.
fn decode_text(raw: &[u8]) -> Option<String> {
    let (body, little_endian) = if let Some(b) = raw.strip_prefix(&[0xFF, 0xFE]) {
        (b, true)
    } else if let Some(b) = raw.strip_prefix(&[0xFE, 0xFF]) {
        (b, false)
    } else {
        let body = raw.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(raw);
        return String::from_utf8(body.to_vec()).ok();
    };
    let units: Vec<u16> = body
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| {
            if little_endian {
                u16::from_le_bytes([b[0], b[1]])
            } else {
                u16::from_be_bytes([b[0], b[1]])
            }
        })
        .collect();
    String::from_utf16(&units).ok()
}

fn parse_manifest(xml: &str) -> Manifest {
    let mut m = Manifest::default();
    let mut reader = quick_xml::Reader::from_str(xml);
    let (mut have_identity, mut have_visuals) = (false, false);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) => match e.local_name().as_ref() {
                "Identity" if !have_identity => {
                    have_identity = true;
                    m.identity = attr(&e, "Name");
                    m.version = attr(&e, "Version");
                }
                "ShellVisuals" if !have_visuals => {
                    have_visuals = true;
                    m.display_name = attr(&e, "DefaultDisplayName");
                    m.publisher = attr(&e, "PublisherDisplayName");
                    m.logo_480 = attr(&e, "Square480x480Logo");
                    m.logo_150 = attr(&e, "Square150x150Logo");
                    m.background = attr(&e, "BackgroundColor");
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    m
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().as_ref() == name)
        .and_then(|a| a.normalized_value(XmlVersion::Implicit1_0).ok())
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// `#rrggbb` → RGB.
fn parse_color(s: &str) -> Option<[u8; 3]> {
    let hex = s.trim().strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(hex, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    const XML: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<Game configVersion="1">
  <!-- <Identity Name="Commented.Out" /> -->
  <Identity Name="Studio.Game" Publisher="CN=X" Version="2.3.4.5" />
  <ShellVisuals DefaultDisplayName="Ratchet &amp; Clank" PublisherDisplayName="Studio"
      Square480x480Logo="xbs_logo_480.png" Square150x150Logo="xbs_logo_150.png" BackgroundColor="#1a2B3c" />
</Game>"##;

    #[test]
    fn reads_the_manifest_fields() {
        let m = parse_manifest(XML);
        assert_eq!(m.identity.as_deref(), Some("Studio.Game"));
        assert_eq!(m.version.as_deref(), Some("2.3.4.5"));
        assert_eq!(m.display_name.as_deref(), Some("Ratchet & Clank"));
        assert_eq!(m.logo_480.as_deref(), Some("xbs_logo_480.png"));
        assert_eq!(
            m.background.as_deref().and_then(parse_color),
            Some([0x1a, 0x2b, 0x3c])
        );
    }

    #[test]
    fn decodes_utf16_manifests() {
        let mut raw = vec![0xFF, 0xFE];
        raw.extend(XML.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(
            parse_manifest(&decode_text(&raw).unwrap())
                .identity
                .as_deref(),
            Some("Studio.Game")
        );
    }

    #[test]
    fn sanitizes_like_xcl() {
        assert_eq!(
            sanitize("StudioInc.12345678ABCDE"),
            "StudioInc.12345678ABCDE"
        );
        assert_eq!(sanitize("Some Game 2.3.4.5"), "Some_Game_2.3.4.5");
        assert_eq!(sanitize(""), "title");
    }

    #[test]
    fn scans_library_folders() {
        let tmp = std::env::temp_dir().join(format!("xcl-launcher-scan-{}", std::process::id()));
        let game = tmp.join("lib").join("Publisher").join("Game");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::create_dir_all(tmp.join("lib").join("NotAGame")).unwrap();
        std::fs::write(game.join("MicrosoftGame.Config"), XML).unwrap();
        let games = scan(&[tmp.join("lib")], &[]);
        std::fs::remove_dir_all(&tmp).unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].name, "Ratchet & Clank");
        assert_eq!(games[0].title, "Studio.Game");
    }
}
