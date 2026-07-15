use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use crate::{
    error::{Result, WorkspaceError},
    model::{SnapshotListEntry, WorkspaceSnapshot, SNAPSHOT_VERSION},
};

#[derive(Debug, Clone)]
pub struct SnapshotStore {
    root: PathBuf,
}

impl SnapshotStore {
    pub fn open_default() -> Result<Self> {
        let root = dirs::data_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("workspace");
        Self::new(root)
    }

    pub fn new(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root).map_err(|source| WorkspaceError::CreateDataDir {
            path: root.clone(),
            source,
        })?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn save(&self, snapshot: &WorkspaceSnapshot, force: bool) -> Result<PathBuf> {
        validate_name(&snapshot.name)?;
        let path = self.snapshot_path(&snapshot.name)?;
        if path.exists() && !force {
            return Err(WorkspaceError::AlreadyExists(snapshot.name.clone()));
        }

        let tmp_path = self
            .root
            .join(format!(".{}.{}.tmp", snapshot.name, std::process::id()));
        let json = serde_json::to_vec_pretty(snapshot)?;

        {
            let mut file = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp_path)
                .map_err(|source| WorkspaceError::WriteFile {
                    path: tmp_path.clone(),
                    source,
                })?;
            file.write_all(&json)
                .and_then(|_| file.write_all(b"\n"))
                .and_then(|_| file.sync_all())
                .map_err(|source| WorkspaceError::WriteFile {
                    path: tmp_path.clone(),
                    source,
                })?;
        }

        fs::rename(&tmp_path, &path).map_err(|source| WorkspaceError::WriteFile {
            path: path.clone(),
            source,
        })?;

        let _ = File::open(&self.root).and_then(|directory| directory.sync_all());
        Ok(path)
    }

    pub fn load(&self, name: &str) -> Result<WorkspaceSnapshot> {
        let path = self.snapshot_path(name)?;
        if !path.exists() {
            return Err(WorkspaceError::NotFound(name.to_string()));
        }
        let bytes = fs::read(&path).map_err(|source| WorkspaceError::ReadFile {
            path: path.clone(),
            source,
        })?;
        let snapshot: WorkspaceSnapshot = serde_json::from_slice(&bytes)
            .map_err(|source| WorkspaceError::ParseSnapshot { path, source })?;
        if snapshot.version > SNAPSHOT_VERSION {
            return Err(WorkspaceError::UnsupportedSnapshotVersion {
                name: name.to_string(),
                found: snapshot.version,
                supported: SNAPSHOT_VERSION,
            });
        }
        Ok(snapshot)
    }

    pub fn list(&self) -> Result<Vec<SnapshotListEntry>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(|source| WorkspaceError::ReadFile {
            path: self.root.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| WorkspaceError::ReadFile {
                path: self.root.clone(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path).map_err(|source| WorkspaceError::ReadFile {
                path: path.clone(),
                source,
            })?;
            let snapshot: WorkspaceSnapshot =
                serde_json::from_slice(&bytes).map_err(|source| WorkspaceError::ParseSnapshot {
                    path: path.clone(),
                    source,
                })?;
            entries.push(SnapshotListEntry {
                name: snapshot.name,
                created_at: snapshot.created_at,
                path: path.display().to_string(),
                display_count: snapshot.displays.len(),
                window_count: snapshot.windows.len(),
            });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    pub fn delete(&self, name: &str) -> Result<()> {
        let path = self.snapshot_path(name)?;
        if !path.exists() {
            return Err(WorkspaceError::NotFound(name.to_string()));
        }
        fs::remove_file(&path).map_err(|source| WorkspaceError::DeleteFile { path, source })
    }

    pub fn snapshot_path(&self, name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        Ok(self.root.join(format!("{name}.json")))
    }
}

fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 128
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || character == '-'
                || character == '_'
                || character == '.'
        })
        && !name.starts_with('.')
        && !name.ends_with('.');

    if valid && Path::new(name).components().count() == 1 {
        Ok(())
    } else {
        Err(WorkspaceError::InvalidName(name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_safe_snapshot_names() {
        assert!(validate_name("coding").is_ok());
        assert!(validate_name("frontend.v2").is_ok());
        assert!(validate_name("../oops").is_err());
        assert!(validate_name("bad/name").is_err());
        assert!(validate_name(".hidden").is_err());
    }
}
