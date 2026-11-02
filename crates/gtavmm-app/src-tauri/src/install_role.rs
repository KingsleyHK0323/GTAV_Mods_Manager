// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
// OpenAI-Codex_lspdfr_role_app

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use gtavmm_core::mod_analyzer::{MenyooCategory, ModFormat, ModPlan};
use gtavmm_core::providers::ModeProvider;
use rusqlite::Connection;

/// A user-selected destination, never an inferred plugin identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ManagedDllRole {
    #[serde(rename = "generalRph")]
    GeneralRph,
    #[serde(rename = "callout")]
    Callout,
}

impl ManagedDllRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::GeneralRph => "generalRph",
            Self::Callout => "callout",
        }
    }

    fn parse(role: Option<&str>) -> Result<Option<Self>, String> {
        match role {
            None | Some("") => Ok(None),
            Some("generalRph") => Ok(Some(Self::GeneralRph)),
            Some("callout") => Ok(Some(Self::Callout)),
            Some(_) => Err("Unknown managed DLL installation role.".to_string()),
        }
    }
}

pub struct RoleProvider<'a> {
    pub inner: &'a dyn ModeProvider,
    pub role: Option<ManagedDllRole>,
}

impl ModeProvider for RoleProvider<'_> {
    fn game_root(&self) -> &Path {
        self.inner.game_root()
    }
    fn resolve_asi_target(&self, name: &OsStr) -> PathBuf {
        self.inner.resolve_asi_target(name)
    }
    fn resolve_native_dll_target(&self, name: &OsStr) -> PathBuf {
        self.inner.resolve_native_dll_target(name)
    }
    fn resolve_managed_dll_target(&self, name: &OsStr) -> PathBuf {
        match self.role {
            Some(ManagedDllRole::GeneralRph) => self.game_root().join("Plugins").join(name),
            Some(ManagedDllRole::Callout) => self.game_root().join("Plugins/LSPDFR").join(name),
            None => self.inner.resolve_managed_dll_target(name),
        }
    }
    fn resolve_menyoo_target(&self, category: MenyooCategory, name: &OsStr) -> PathBuf {
        self.inner.resolve_menyoo_target(category, name)
    }
    fn resolve_folder_replacer_target(&self, relative: &Path) -> PathBuf {
        self.inner.resolve_folder_replacer_target(relative)
    }
    fn lspdfr_package_game_root(&self) -> Option<&Path> {
        self.inner.lspdfr_package_game_root()
    }
    fn resolve_add_on_pack_target(&self, name: &str, relative: &Path) -> PathBuf {
        self.inner.resolve_add_on_pack_target(name, relative)
    }
}

fn is_dll(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("dll"))
}

fn unsafe_link(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

// Missing destinations are valid; existing aliases and traversal never identify a role.
fn ensure_safe_path(path: &Path) -> Result<(), String> {
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err("Managed DLL role paths cannot contain traversal.".to_string());
    }
    for ancestor in path.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if unsafe_link(&metadata) => {
                return Err(
                    "Managed DLL role paths cannot use links or reparse points.".to_string()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

/// Shared by preview and install. Eligibility uses the original input, not extracted files.
pub fn classify(
    input: &Path,
    mode: &str,
    provider: &dyn ModeProvider,
    role: Option<&str>,
) -> Result<(ModPlan, Option<ManagedDllRole>, bool), String> {
    let role = ManagedDllRole::parse(role)?;
    let standalone = mode == "legacy-lspdfr"
        && is_dll(input)
        && std::fs::symlink_metadata(input)
            .is_ok_and(|metadata| metadata.is_file() && !unsafe_link(&metadata));
    if role.is_some() && !standalone {
        return Err(
            "Installation roles require a standalone managed DLL on Legacy LSPDFR.".to_string(),
        );
    }
    if standalone {
        ensure_safe_path(input)?;
    }
    let wrapped = RoleProvider {
        inner: provider,
        role,
    };
    let plan =
        gtavmm_core::mod_analyzer::classify(input, &wrapped).map_err(|error| error.to_string())?;
    let eligible = standalone
        && plan.format == ModFormat::ManagedDll
        && plan.files.len() == 1
        && plan.files[0].source == input;
    if role.is_some() && !eligible {
        return Err(
            "Installation roles require a standalone managed DLL on Legacy LSPDFR.".to_string(),
        );
    }
    if eligible {
        ensure_safe_path(&plan.files[0].target)?;
    }
    Ok((plan, role, eligible))
}

/// Infer only a recorded destination. Native and managed DLLs share DB source_type `dll`.
pub fn infer_saved_role(
    conn: &Connection,
    mod_id: i64,
    root: &Path,
) -> Result<Option<ManagedDllRole>, String> {
    let (source_type, mode, install_path, status): (String, Option<String>, String, String) = conn
        .query_row(
            "SELECT source_type,mode,install_path,status FROM installed_mod WHERE id=?1",
            [mod_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|error| error.to_string())?;
    if source_type != "dll" || mode.as_deref() != Some("legacy-lspdfr") {
        return Ok(None);
    }
    let invalid = || {
        "Recorded DLL installation role is ambiguous or inconsistent; preserve it for recovery."
            .to_string()
    };
    if !root.is_absolute()
        || !matches!(status.as_str(), "active" | "disabled")
        || Path::new(&install_path) != root
    {
        return Err(invalid());
    }
    ensure_safe_path(root)?;
    let mut statement = conn
        .prepare("SELECT target_path FROM installed_mod_file WHERE installed_mod_id=?1 ORDER BY id")
        .map_err(|error| error.to_string())?;
    let targets = statement
        .query_map([mod_id], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if targets.len() != 1 {
        return Err(invalid());
    }
    let target = Path::new(&targets[0]);
    if !is_dll(target) {
        return Err(invalid());
    }
    let name = target.file_name().ok_or_else(invalid)?;
    ensure_safe_path(target)?;
    if std::fs::symlink_metadata(target).is_ok_and(|metadata| !metadata.is_file()) {
        return Err(invalid());
    }
    if target == root.join("Plugins").join(name) {
        Ok(Some(ManagedDllRole::GeneralRph))
    } else if target == root.join("Plugins/LSPDFR").join(name) {
        Ok(Some(ManagedDllRole::Callout))
    } else if target == root.join(name) {
        // This is the existing native-DLL layout, not a managed role.
        Ok(None)
    } else {
        Err(invalid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtavmm_core::providers::LegacyLspdfrProvider;

    fn managed_dll(path: &Path) {
        let mut bytes = vec![0u8; 512];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[88..90].copy_from_slice(&0x10bu16.to_le_bytes());
        bytes[296..300].copy_from_slice(&1u32.to_le_bytes());
        bytes[300..304].copy_from_slice(&1u32.to_le_bytes());
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn closed_roles_route_only_standalone_managed_dlls() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let dll = source.path().join("Example.dll");
        managed_dll(&dll);
        let provider = LegacyLspdfrProvider::new(root.path().to_path_buf());
        for (role, folder) in [("generalRph", "Plugins"), ("callout", "Plugins/LSPDFR")] {
            let (plan, selected, required) =
                classify(&dll, "legacy-lspdfr", &provider, Some(role)).unwrap();
            assert!(required);
            assert_eq!(selected.unwrap().as_str(), role);
            assert_eq!(
                plan.files[0].target,
                root.path().join(folder).join("Example.dll")
            );
        }
        assert!(classify(&dll, "legacy-lspdfr", &provider, Some("../other")).is_err());
        assert_eq!(
            serde_json::to_string(&ManagedDllRole::GeneralRph).unwrap(),
            "\"generalRph\""
        );
        assert!(serde_json::from_str::<ManagedDllRole>("\"other\"").is_err());
        assert!(classify(&dll, "legacy-sp", &provider, Some("callout")).is_err());
        assert!(classify(source.path(), "legacy-lspdfr", &provider, Some("callout")).is_err());
        let native = source.path().join("native.dll");
        std::fs::write(&native, b"native").unwrap();
        assert!(classify(&native, "legacy-lspdfr", &provider, Some("callout")).is_err());
        let archive = source.path().join("package.zip");
        std::fs::write(&archive, b"not an archive").unwrap();
        assert!(classify(&archive, "legacy-lspdfr", &provider, Some("callout")).is_err());
        let (plan, role, required) = classify(&dll, "legacy-lspdfr", &provider, None).unwrap();
        assert!(role.is_none());
        assert!(required);
        assert_eq!(
            plan.files[0].target,
            root.path().join("Plugins/Example.dll")
        );
    }

    fn record(conn: &Connection, root: &Path, target: &Path, status: &str) -> i64 {
        conn.execute(
            "INSERT INTO installed_mod(name,source_type,install_path,status,mode,source_path) VALUES('Example','dll',?1,?2,'legacy-lspdfr',?3)",
            rusqlite::params![root.to_str().unwrap(), status, root.join("download/Example.dll").to_str().unwrap()],
        ).unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO installed_mod_file(installed_mod_id,target_path,file_hash) VALUES(?1,?2,'synthetic')",
            rusqlite::params![id, target.to_str().unwrap()],
        ).unwrap();
        id
    }

    #[test]
    fn saved_role_uses_recorded_targets_even_when_disabled_or_missing() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let root = tempfile::tempdir().unwrap();
        for status in ["active", "disabled"] {
            let id = record(
                &conn,
                root.path(),
                &root.path().join("Plugins/LSPDFR/Example.dll"),
                status,
            );
            conn.execute(
                "UPDATE installed_mod SET source_path=NULL WHERE id=?1",
                [id],
            )
            .unwrap();
            assert_eq!(
                infer_saved_role(&conn, id, root.path()).unwrap(),
                Some(ManagedDllRole::Callout)
            );
        }
        let id = record(
            &conn,
            root.path(),
            &root.path().join("Example.dll"),
            "active",
        );
        assert_eq!(infer_saved_role(&conn, id, root.path()).unwrap(), None);
    }

    #[test]
    fn saved_role_rejects_ambiguous_cross_root_and_inconsistent_records() {
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        for target in [
            other.path().join("Plugins/Example.dll"),
            root.path().join("Plugins/Other/Example.dll"),
            root.path().join("Plugins/../Example.dll"),
        ] {
            let id = record(&conn, root.path(), &target, "active");
            assert!(infer_saved_role(&conn, id, root.path()).is_err());
        }
        let id = record(
            &conn,
            root.path(),
            &root.path().join("Plugins/Example.dll"),
            "active",
        );
        conn.execute("INSERT INTO installed_mod_file(installed_mod_id,target_path,file_hash) VALUES(?1,'extra','synthetic')", [id]).unwrap();
        assert!(infer_saved_role(&conn, id, root.path()).is_err());
        assert!(infer_saved_role(&conn, id, other.path()).is_err());
        conn.execute("INSERT INTO installed_mod(name,source_type,install_path,status,mode) VALUES('missing','dll',?1,'active','legacy-lspdfr')", [root.path().to_str().unwrap()]).unwrap();
        assert!(infer_saved_role(&conn, conn.last_insert_rowid(), root.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn role_paths_reject_linked_destinations_before_writes() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let dll = source.path().join("Example.dll");
        managed_dll(&dll);
        std::os::unix::fs::symlink(outside.path(), root.path().join("Plugins")).unwrap();
        let provider = LegacyLspdfrProvider::new(root.path().to_path_buf());
        assert!(classify(&dll, "legacy-lspdfr", &provider, Some("callout")).is_err());
        let conn = gtavmm_core::db::open_in_memory().unwrap();
        let id = record(
            &conn,
            root.path(),
            &root.path().join("Plugins/LSPDFR/Example.dll"),
            "disabled",
        );
        assert!(infer_saved_role(&conn, id, root.path()).is_err());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
