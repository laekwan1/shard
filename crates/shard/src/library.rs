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
