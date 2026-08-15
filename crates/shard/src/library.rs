//! What has been saved, and what can be done with it.
//!
//! The desktop half of the offline library. Downloads land in the user's own
//! `Videos\Shard` and `Music\Shard`, where Explorer and every player can already
//! see them — so this keeps no database of its own and asks the folders what is
//! in them each time it is opened. A file deleted from Explorer is then simply
//! gone from here too, rather than being a row that opens nothing.
//!
//! Folders are real directories underneath those two. Grouping is therefore
//! something the file system keeps, not something only this program understands:
//! move a file out with Explorer and the grouping moves with it.

use std::path::{Path, PathBuf};

/// The app's own corner of each of the user's media folders.
pub const ROOT: &str = "Shard";

/// Which shelf a saved file belongs on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Video,
    Music,
}

impl Kind {
    pub const ALL: [Kind; 2] = [Kind::Video, Kind::Music];

    pub fn label(self) -> &'static str {
        match self {
            Kind::Video => "영상",
            Kind::Music => "음악",
        }
    }

    /// Where this shelf keeps its files.
    pub fn folder(self) -> PathBuf {
        let base = std::env::var("USERPROFILE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        match self {
            Kind::Video => base.join("Videos").join(ROOT),
            Kind::Music => base.join("Music").join(ROOT),
        }
    }
}

/// One saved file.
#[derive(Clone, Debug)]
pub struct Item {
    pub path: PathBuf,
    /// The file's name without its extension, which is what a title reads as.
    pub title: String,
    /// The folder it sits in, or empty for the top of the shelf.
    pub folder: String,
    pub bytes: u64,
    /// When it was saved, as seconds since the epoch.
    pub saved_at: u64,
    pub kind: Kind,
}

/// Everything on a shelf, newest first.
///
/// Blocking: it reads directories. Cheap enough for a folder of downloads, and
/// it is only done when the library is opened or something changes under it.
pub fn items(kind: Kind) -> Vec<Item> {
    let root = kind.folder();
    let mut found = Vec::new();
    sweep(&root, "", kind, &mut found);
    // Newest first: what was just saved is what is being looked for.
    found.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));

    // Then whatever arrangement the user has made of it. Anything they have not
    // placed — a download that landed since — keeps its place at the top rather
    // than falling to the end of a list they arranged before it existed.
    let arranged = order(kind);
    if !arranged.is_empty() {
        found.sort_by_key(|item| match arranged.iter().position(|k| *k == key(&item.path)) {
            Some(at) => (1, at),
            None => (0, 0),
        });
    }
    found
}

/// One level of folders, and the files in each — the same shape the phone keeps.
fn sweep(dir: &Path, folder: &str, kind: Kind, out: &mut Vec<Item>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            // Only the one level: a folder inside a folder is more places to
            // look through than a list of downloads is worth.
            if folder.is_empty() {
                let name = entry.file_name().to_string_lossy().to_string();
                sweep(&path, &name, kind, out);
            }
            continue;
        }
        // Whatever is in these folders is ours to show; a stray file the user
        // dropped in plays as well as one this program saved.
        let title = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if title.is_empty() {
            continue;
        }
        out.push(Item {
            path,
            title,
            folder: folder.to_string(),
            bytes: meta.len(),
            saved_at: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
            kind,
        });
    }
}

/// The folders on a shelf, in the order they should be shown.
///
/// Read from the disk rather than gathered from the files: a folder just made
/// has nothing in it yet, and one that only existed where its files said it did
/// would not appear until something was put in it — which is exactly when it is
/// least useful.
pub fn folders(kind: Kind) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(kind.folder()) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.metadata().map(|m| m.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// Make a folder on a shelf. Returns false when the name is unusable or the
/// file system refuses it.
pub fn add_folder(kind: Kind, name: &str) -> bool {
    let clean = clean(name);
    if clean.is_empty() {
        return false;
    }
    std::fs::create_dir_all(kind.folder().join(clean)).is_ok()
}

/// A name for a file that means the same file next time the program runs.
///
/// FNV-1a over its path: a few lines, no dependency, and far better spread than
/// a sum for what is only a lookup key.
pub fn key(path: &Path) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Where a shelf's hand-made order is written down.
///
/// Beside the config rather than among the media: the user's Videos folder is
/// theirs, and a file of ours in it is something they would have to wonder
/// about. Losing it costs the arrangement, not the files.
fn order_file(kind: Kind) -> PathBuf {
    let name = match kind {
        Kind::Video => "order-video.txt",
        Kind::Music => "order-music.txt",
    };
    uikit::config::app_dir(crate::config::APP_NAME).join(name)
}

/// The order the shelf was last arranged in, oldest arrangement first.
pub fn order(kind: Kind) -> Vec<String> {
    std::fs::read_to_string(order_file(kind))
        .map(|text| text.lines().map(|line| line.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

/// Write down an arrangement, by the keys of the files in it.
pub fn set_order(kind: Kind, keys: &[String]) -> anyhow::Result<()> {
    let path = order_file(kind);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, keys.join("\n"))?;
    Ok(())
}

/// Take a folder off a shelf, keeping everything that was in it.
///
/// What is inside comes out to the top of the shelf first, and only the empty
/// folder itself is removed. Deleting a folder is a tidying-up gesture — the
/// files in it are hours of downloading, and nobody means to lose them by
/// putting a folder away. Anything already at the top with that name is left
/// alone and the file inside keeps a number on the end.
///
/// Returns how many files came out.
pub fn drop_folder(kind: Kind, name: &str) -> anyhow::Result<usize> {
    use anyhow::Context;
    let clean = clean(name);
    if clean.is_empty() {
        anyhow::bail!("이름이 비어 있습니다");
    }
    let root = kind.folder();
    let folder = root.join(&clean);
    if !folder.is_dir() {
        anyhow::bail!("{} 폴더가 없습니다", folder.display());
    }
    let mut moved = 0;
    for entry in std::fs::read_dir(&folder).with_context(|| format!("reading {}", folder.display()))? {
        let Ok(entry) = entry else { continue };
        let from = entry.path();
        if !from.is_file() {
            continue;
        }
        let Some(file) = from.file_name() else { continue };
        if std::fs::rename(&from, free_name(&root, file)).is_ok() {
            moved += 1;
        }
    }
    // Only when it is empty. A folder that still holds something — a file that
    // could not be moved, a folder of its own — is left where it is rather than
    // taken away with what is inside it.
    std::fs::remove_dir(&folder).with_context(|| format!("removing {}", folder.display()))?;
    Ok(moved)
}

/// `name` inside `root`, with a number added if something is already there.
fn free_name(root: &std::path::Path, name: &std::ffi::OsStr) -> std::path::PathBuf {
    let first = root.join(name);
    if !first.exists() {
        return first;
    }
    let name = std::path::Path::new(name);
    let stem = name.file_stem().unwrap_or(name.as_os_str()).to_string_lossy().to_string();
    let extension = name.extension().map(|e| e.to_string_lossy().to_string());
    for n in 2..1000 {
        let tried = match &extension {
            Some(extension) => root.join(format!("{stem} ({n}).{extension}")),
            None => root.join(format!("{stem} ({n})")),
        };
        if !tried.exists() {
            return tried;
        }
    }
    first
}

/// Put an item in a folder, or back at the top of its shelf when [folder] is
/// empty. The file is moved on disk, so Explorer sees the same thing this does.
pub fn move_to(item: &Item, folder: &str) -> bool {
    let clean = clean(folder);
    if item.folder == clean {
        return true;
    }
    let mut target = item.kind.folder();
    if !clean.is_empty() {
        target = target.join(&clean);
        if std::fs::create_dir_all(&target).is_err() {
            return false;
        }
    }
    let Some(name) = item.path.file_name() else { return false };
    std::fs::rename(&item.path, target.join(name)).is_ok()
}

/// Delete a saved file for good.
pub fn delete(item: &Item) -> bool {
    std::fs::remove_file(&item.path).is_ok()
}

/// Give a saved file another name, keeping the extension it already has.
///
/// The extension is not the user's to type: it says what the file is, and a
/// title typed over it would leave a video that nothing will open. Only the
/// part that reads as a title changes.
pub fn rename(item: &Item, title: &str) -> bool {
    let clean = clean(title);
    if clean.is_empty() || clean == item.title {
        return false;
    }
    let target = match item.path.extension().and_then(|e| e.to_str()) {
        Some(extension) => item.path.with_file_name(format!("{clean}.{extension}")),
        None => item.path.with_file_name(clean),
    };
    if target.exists() {
        return false;
    }
    std::fs::rename(&item.path, target).is_ok()
}

/// A folder name the file system will take.
///
/// Separators are what matter: a name carrying one would put the folder
/// somewhere else entirely, which is not what typing it meant.
pub fn clean(name: &str) -> String {
    name.trim()
        .chars()
        .filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .take(40)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Bytes, as a person reads them.
pub fn human(bytes: u64) -> String {
    const GB: u64 = 1 << 30;
    const MB: u64 = 1 << 20;
    match bytes {
        b if b >= GB => format!("{:.1} GB", b as f64 / GB as f64),
        b if b >= MB => format!("{} MB", b / MB),
        b => format!("{} KB", (b / 1024).max(1)),
    }
}

/// How long ago it was saved, in the words a list wants.
pub fn age(saved_at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if saved_at == 0 || saved_at > now {
        return String::new();
    }
    let days = (now - saved_at) / 86_400;
    match days {
        0 => "오늘".into(),
        1 => "어제".into(),
        d if d < 30 => format!("{d}일 전"),
        d => format!("{}개월 전", d / 30),
    }
}

/// Hand a file to whatever the user opens that kind with.
///
/// Their own player, not one built into this program: a media player is a large
/// thing to write badly, and the one they have chosen already knows their
/// subtitles, their volume, and where they left off.
pub fn open(path: &Path) -> bool {
    open_with_shell(path.as_os_str())
}

/// Open a folder in Explorer.
pub fn reveal(path: &Path) -> bool {
    open_with_shell(path.as_os_str())
}

#[cfg(windows)]
fn open_with_shell(what: &std::ffi::OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt;

    // ShellExecuteW rather than `cmd /C start`: no console window flashes up,
    // and a path with a space or an ampersand in it needs no quoting rules
    // guessed at. Names here are Korean video titles — exactly the case a
    // command line mangles.
    let wide: Vec<u16> = what.encode_wide().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: isize,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> isize;
    }
    // Anything above 32 is success; below is one of the shell's error codes.
    const SW_SHOWNORMAL: i32 = 1;
    unsafe {
        ShellExecuteW(
            0,
            verb.as_ptr(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        ) > 32
    }
}

#[cfg(not(windows))]
fn open_with_shell(_what: &std::ffi::OsStr) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_name_loses_what_a_path_would_take_as_a_separator() {
        assert_eq!(clean("여행/2024"), "여행2024");
        assert_eq!(clean("  운동  "), "운동");
        assert_eq!(clean("a:b*c?"), "abc");
    }

    #[test]
    fn a_folder_name_is_kept_to_a_length_a_list_can_show() {
        assert_eq!(clean(&"가".repeat(80)).chars().count(), 40);
    }

    #[test]
    fn the_two_shelves_are_the_users_own_media_folders() {
        let video = Kind::Video.folder();
        let music = Kind::Music.folder();
        assert!(video.ends_with(std::path::Path::new("Videos").join(ROOT)));
        assert!(music.ends_with(std::path::Path::new("Music").join(ROOT)));
    }

    #[test]
    fn a_file_coming_out_of_a_folder_does_not_land_on_one_already_there() {
        let root = std::env::temp_dir().join("shard-free-name-test");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let name = std::ffi::OsString::from("노래.webm");

        // Nothing there: the name it already has.
        assert_eq!(free_name(&root, &name), root.join("노래.webm"));

        // Something there: numbered, and the extension stays an extension.
        std::fs::write(root.join("노래.webm"), b"x").unwrap();
        assert_eq!(free_name(&root, &name), root.join("노래 (2).webm"));
        std::fs::write(root.join("노래 (2).webm"), b"x").unwrap();
        assert_eq!(free_name(&root, &name), root.join("노래 (3).webm"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sizes_read_the_way_a_person_says_them() {
        assert_eq!(human(1 << 20), "1 MB");
        assert_eq!(human(3 << 30), "3.0 GB");
        assert_eq!(human(2048), "2 KB");
    }

    #[test]
    fn a_shelf_lists_what_is_in_its_folders_newest_first() {
        let dir = std::env::temp_dir().join(format!("shard-lib-test-{}", std::process::id()));
        let inner = dir.join("여행");
        std::fs::create_dir_all(&inner).expect("make test folders");
        std::fs::write(dir.join("첫번째.mp4"), b"aaaa").expect("write");
        std::fs::write(inner.join("두번째.mp4"), b"bb").expect("write");

        let mut found = Vec::new();
        sweep(&dir, "", Kind::Video, &mut found);
        assert_eq!(found.len(), 2);

        let top = found.iter().find(|i| i.title == "첫번째").expect("top item");
        assert_eq!(top.folder, "");
        assert_eq!(top.bytes, 4);
        let inside = found.iter().find(|i| i.title == "두번째").expect("foldered item");
        assert_eq!(inside.folder, "여행");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
