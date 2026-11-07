use crate::adb;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const CORE_FILE: &str = "pp.dat";

const EXCLUDED_NAMES: [&str; 1] = ["resumableDownload"];
const EXCLUDED_PREFIXES: [&str; 1] = ["CDN."];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: i64,
}

#[derive(Clone, Debug)]
pub struct SaveProbe {
    pub core: Option<SaveEntry>,
    pub running: bool,
}

impl SaveProbe {
    pub fn has_save(&self) -> bool {
        self.core.is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncPlan {
    LeftToRight,
    RightToLeft,
    InSync,
    Ambiguous {
        mtime: i64,
        left_size: u64,
        right_size: u64,
    },
    NoSave,
}

#[derive(Clone, Debug)]
pub struct CopyReport {
    pub entries: usize,
    pub mtime: i64,
    pub backup: Option<PathBuf>,
}

/// Build the find command that lists the top level save entries.
///
/// Toybox emits a `.` entry even with `-mindepth 1`, so the parser skips it again.
/// CDN caches and resumable downloads are device specific and must not be copied.
fn list_command() -> String {
    format!(
        "if [ -d {dir} ]; then cd {dir} && find . -mindepth 1 -maxdepth 1 ! -name '.' \\( -name 'CDN.*' -o -name 'resumableDownload' \\) -prune -o -exec stat -c '%F|%s|%Y|%n' {{}} +; fi",
        dir = shell_quote(adb::SAVE_DIR)
    )
}

pub fn list_entries(serial: &str) -> Result<Vec<SaveEntry>, String> {
    let stdout = adb::shell(serial, &list_command())?;
    Ok(parse_entries(&stdout))
}

pub fn parse_entries(output: &str) -> Vec<SaveEntry> {
    let mut entries = Vec::new();
    for line in output.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }

        let mut parts = line.splitn(4, '|');
        let (Some(kind), Some(size), Some(mtime), Some(name)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };

        let name = name.strip_prefix("./").unwrap_or(name);
        if name.is_empty()
            || name == "."
            || name.contains('/')
            || EXCLUDED_NAMES.contains(&name)
            || EXCLUDED_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        {
            continue;
        }

        let Ok(size) = size.trim().parse::<u64>() else {
            continue;
        };
        let Ok(mtime) = mtime.trim().parse::<i64>() else {
            continue;
        };

        entries.push(SaveEntry {
            name: name.to_string(),
            is_dir: kind.trim().eq_ignore_ascii_case("directory"),
            size,
            mtime,
        });
    }

    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries
}

pub fn probe(serial: &str) -> Result<SaveProbe, String> {
    let running = adb::device_is_running(serial)?;
    let entries = list_entries(serial)?;
    let core = entries
        .iter()
        .find(|entry| entry.name == CORE_FILE)
        .cloned();
    Ok(SaveProbe { core, running })
}

pub fn plan(left: &SaveProbe, right: &SaveProbe) -> SyncPlan {
    match (&left.core, &right.core) {
        (None, None) => SyncPlan::NoSave,
        (Some(_), None) => SyncPlan::LeftToRight,
        (None, Some(_)) => SyncPlan::RightToLeft,
        (Some(left_core), Some(right_core)) => {
            if left_core.mtime > right_core.mtime {
                SyncPlan::LeftToRight
            } else if right_core.mtime > left_core.mtime {
                SyncPlan::RightToLeft
            } else if left_core.size == right_core.size {
                SyncPlan::InSync
            } else {
                SyncPlan::Ambiguous {
                    mtime: left_core.mtime,
                    left_size: left_core.size,
                    right_size: right_core.size,
                }
            }
        }
    }
}

/// Copy the complete save entry set from one device to the other.
///
/// The target's current save is backed up under the local data directory first.
/// Every entry is pulled with `-a` so modification times survive.
///
/// Directories are created with `mkdir -p` and files are pushed one by one:
/// on Waydroid, `adb push` of a directory cannot create nested directories under
/// `Android/data` (`secure_mkdirs` returns `EPERM`), while shell `mkdir` and
/// pushing a file into an existing directory work. Top-level entry times are set
/// explicitly after the push as a second guarantee.
pub fn copy_save(
    source: &str,
    target: &str,
    progress: &mut dyn FnMut(&str),
) -> Result<CopyReport, String> {
    let entries = list_entries(source)?;
    let core = entries
        .iter()
        .find(|entry| entry.name == CORE_FILE)
        .cloned()
        .ok_or_else(|| format!("{source} has no Reflourished save ({CORE_FILE} is missing)"))?;

    let temp = TempDir::new()?;

    progress(&format!("Reading the save from {source}..."));
    for entry in &entries {
        pull_entry(source, &entry.name, temp.path())?;
    }

    let target_entries = list_entries(target)?;
    let backup = if target_entries.is_empty() {
        None
    } else {
        let backup_dir = new_backup_dir(target)?;
        progress(&format!(
            "Backing up the target save to {}...",
            backup_dir.display()
        ));
        for entry in &target_entries {
            pull_entry(target, &entry.name, &backup_dir)
                .map_err(|error| format!("could not back up the target save: {error}"))?;
        }
        Some(backup_dir)
    };

    // The game can be launched while the save is being read. Writing over a
    // live save risks corruption, so re-check right before changing the target.
    if adb::device_is_running(source)? || adb::device_is_running(target)? {
        return Err(format!(
            "the game was started during the transfer, so the target save was not changed{}",
            backup_note(&backup)
        ));
    }

    if !target_entries.is_empty() {
        progress(&format!("Removing the old save from {target}..."));
        let mut command = String::from("rm -rf");
        for entry in &target_entries {
            command.push(' ');
            command.push_str(&shell_quote(&format!("{}/{}", adb::SAVE_DIR, entry.name)));
        }
        adb::shell(target, &command).map_err(|error| {
            format!(
                "could not remove the old save from {target}: {error}{}",
                backup_note(&backup)
            )
        })?;
    }

    adb::mkdir_p(target, adb::SAVE_DIR).map_err(|error| {
        format!(
            "could not create {} on {target}: {error}{}",
            adb::SAVE_DIR,
            backup_note(&backup)
        )
    })?;

    let mut dirs = Vec::new();
    let mut files = Vec::new();
    for entry in &entries {
        if entry.is_dir {
            dirs.push(entry.name.clone());
            collect_tree(temp.path(), &entry.name, &mut dirs, &mut files)?;
        } else {
            files.push(entry.name.clone());
        }
    }
    dirs.sort();
    dirs.dedup();
    files.sort();
    files.dedup();

    if !dirs.is_empty() {
        progress(&format!("Preparing directories on {target}..."));
        let mut command = String::from("mkdir -p");
        for dir in &dirs {
            command.push(' ');
            command.push_str(&shell_quote(&format!("{}/{}", adb::SAVE_DIR, dir)));
        }
        adb::shell(target, &command).map_err(|error| {
            format!(
                "could not create the save directories on {target}: {error}{}",
                backup_note(&backup)
            )
        })?;
    }

    for file in &files {
        progress(&format!("Copying {file} to {target}..."));
        let local = temp.path().join(file);
        adb::push(target, &local, &remote_parent(file)).map_err(|error| {
            format!(
                "could not copy {file} to {target}: {error}{}",
                backup_note(&backup)
            )
        })?;
    }

    // File modification times must be set before directory times: pushing the
    // last child updates the parent directory's time on the device.
    let mut ordered: Vec<&SaveEntry> = entries.iter().collect();
    ordered.sort_by_key(|entry| entry.is_dir);
    for entry in ordered {
        let path = format!("{}/{}", adb::SAVE_DIR, entry.name);
        adb::set_mtime(target, &path, entry.mtime).map_err(|error| {
            format!(
                "could not restore the modification time of {} on {target}: {error}{}",
                entry.name,
                backup_note(&backup)
            )
        })?;
    }

    let remote = adb::stat_remote(target, &format!("{}/{}", adb::SAVE_DIR, CORE_FILE))?
        .ok_or_else(|| {
            format!(
                "copy finished but {CORE_FILE} is missing on {target}{}",
                backup_note(&backup)
            )
        })?;
    if remote.mtime != core.mtime || remote.size != core.size {
        return Err(format!(
            "copy finished but {CORE_FILE} on {target} does not match the source (expected {} bytes from {}, found {} bytes from {}){}",
            core.size,
            format_mtime(core.mtime),
            remote.size,
            format_mtime(remote.mtime),
            backup_note(&backup)
        ));
    }

    Ok(CopyReport {
        entries: entries.len(),
        mtime: core.mtime,
        backup,
    })
}

fn pull_entry(serial: &str, name: &str, destination: &Path) -> Result<(), String> {
    let remote = format!("{}/{}", adb::SAVE_DIR, name);
    adb::pull(serial, &remote, destination)?;
    let local = destination.join(name);
    if !local.exists() {
        return Err(format!(
            "{remote} was pulled but {} is missing",
            local.display()
        ));
    }
    Ok(())
}

/// Walk a pulled directory and collect its relative directories and files.
fn collect_tree(
    root: &Path,
    relative: &str,
    dirs: &mut Vec<String>,
    files: &mut Vec<String>,
) -> Result<(), String> {
    let dir = root.join(relative);
    let entries =
        fs::read_dir(&dir).map_err(|error| format!("could not read {}: {error}", dir.display()))?;

    for entry in entries {
        let entry = entry.map_err(|error| format!("could not read {}: {error}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let child = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        let file_type = entry
            .file_type()
            .map_err(|error| format!("could not inspect {}: {error}", entry.path().display()))?;

        if file_type.is_dir() {
            dirs.push(child.clone());
            collect_tree(root, &child, dirs, files)?;
        } else if file_type.is_file() {
            files.push(child);
        }
    }

    Ok(())
}

/// Remote directory that receives the file with the given relative path.
fn remote_parent(relative: &str) -> String {
    match relative.rsplit_once('/') {
        Some((parent, _)) => format!("{}/{}/", adb::SAVE_DIR, parent),
        None => format!("{}/", adb::SAVE_DIR),
    }
}

fn new_backup_dir(target: &str) -> Result<PathBuf, String> {
    let root = backups_root();
    fs::create_dir_all(&root)
        .map_err(|error| format!("could not create {}: {error}", root.display()))?;

    let label = sanitize_serial(target);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut candidate = root.join(format!("{label}-{stamp}"));
    let mut suffix = 1;
    while candidate.exists() {
        candidate = root.join(format!("{label}-{stamp}-{suffix}"));
        suffix += 1;
    }
    fs::create_dir_all(&candidate)
        .map_err(|error| format!("could not create {}: {error}", candidate.display()))?;
    Ok(candidate)
}

pub fn backups_root() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("pvz2-save-sync")
        .join("backups")
}

pub fn format_mtime(mtime: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(mtime, 0).single() {
        Some(time) => time.format("%Y-%m-%d %H:%M").to_string(),
        None => format!("unix time {mtime}"),
    }
}

fn sanitize_serial(serial: &str) -> String {
    let mut label: String = serial
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if label.len() > 48 {
        label.truncate(48);
    }
    label
}

fn backup_note(backup: &Option<PathBuf>) -> String {
    match backup {
        Some(path) => format!(
            ". The previous target save is backed up at {}",
            path.display()
        ),
        None => String::new(),
    }
}

pub fn shell_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if character == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(character);
        }
    }
    quoted.push('\'');
    quoted
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Result<Self, String> {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let path =
            std::env::temp_dir().join(format!("pvz2-save-sync-{}-{unique}", std::process::id()));
        fs::create_dir_all(&path)
            .map_err(|error| format!("could not create {}: {error}", path.display()))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = "\
directory|3452|1794032796|.
regular file|24|1794032014|./global_save_data.hash
regular file|901|1794032014|./global_save_data
regular file|6129|1794032796|./pp.dat
regular file|2398|1794032652|./local_profiles
regular file|6128|1794032796|./pp.dat.bak
directory|3452|1794032615|./activequests
";

    fn probe_with(mtime: i64, size: u64) -> SaveProbe {
        SaveProbe {
            core: Some(SaveEntry {
                name: CORE_FILE.to_string(),
                is_dir: false,
                size,
                mtime,
            }),
            running: false,
        }
    }

    #[test]
    fn parses_listing_and_skips_dot_entry() {
        let entries = parse_entries(LISTING);
        assert_eq!(entries.len(), 6);
        assert_eq!(entries[0].name, "activequests");
        assert!(entries[0].is_dir);
        let core = entries.iter().find(|entry| entry.name == "pp.dat").unwrap();
        assert_eq!(core.mtime, 1794032796);
        assert_eq!(core.size, 6129);
        assert!(!core.is_dir);
    }

    #[test]
    fn skips_caches_and_traversal_names() {
        let output = "\
directory|1|1|./CDN.1.4.2
directory|1|1|./resumableDownload
regular file|1|1|./../escape
regular file|1|1|./ok
";
        let entries = parse_entries(output);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "ok");
    }

    #[test]
    fn newer_save_wins() {
        assert_eq!(
            plan(&probe_with(200, 10), &probe_with(100, 10)),
            SyncPlan::LeftToRight
        );
        assert_eq!(
            plan(&probe_with(100, 10), &probe_with(200, 10)),
            SyncPlan::RightToLeft
        );
    }

    #[test]
    fn equal_timestamp_and_size_is_in_sync() {
        assert_eq!(
            plan(&probe_with(100, 10), &probe_with(100, 10)),
            SyncPlan::InSync
        );
    }

    #[test]
    fn equal_timestamp_with_different_size_is_ambiguous() {
        assert_eq!(
            plan(&probe_with(100, 10), &probe_with(100, 20)),
            SyncPlan::Ambiguous {
                mtime: 100,
                left_size: 10,
                right_size: 20,
            }
        );
    }

    #[test]
    fn missing_saves_choose_the_available_side() {
        let empty = SaveProbe {
            core: None,
            running: false,
        };
        assert_eq!(plan(&probe_with(100, 10), &empty), SyncPlan::LeftToRight);
        assert_eq!(plan(&empty, &probe_with(100, 10)), SyncPlan::RightToLeft);
        assert_eq!(plan(&empty, &empty), SyncPlan::NoSave);
    }

    #[test]
    fn shell_quoting_escapes_single_quotes() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn collects_nested_directories_and_files() {
        let temp = TempDir::new().unwrap();
        let quests = temp.path().join("activequests/1794034435_ActiveQuests");
        fs::create_dir_all(&quests).unwrap();
        fs::write(quests.join("save_QuestsActive.rton"), b"quests").unwrap();
        fs::write(quests.join("_active_quests_valid"), b"").unwrap();
        fs::write(temp.path().join("pp.dat"), b"core").unwrap();

        let mut dirs = Vec::new();
        let mut files = Vec::new();
        collect_tree(temp.path(), "activequests", &mut dirs, &mut files).unwrap();
        dirs.sort();
        files.sort();

        assert_eq!(dirs, vec!["activequests/1794034435_ActiveQuests"]);
        assert_eq!(
            files,
            vec![
                "activequests/1794034435_ActiveQuests/_active_quests_valid",
                "activequests/1794034435_ActiveQuests/save_QuestsActive.rton",
            ]
        );
    }

    #[test]
    fn builds_remote_parent_directories() {
        assert_eq!(remote_parent("pp.dat"), format!("{}/", adb::SAVE_DIR));
        assert_eq!(
            remote_parent("activequests/1794034435_ActiveQuests/save_QuestsActive.rton"),
            format!("{}/activequests/1794034435_ActiveQuests/", adb::SAVE_DIR)
        );
    }
}
