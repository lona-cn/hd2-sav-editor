mod archive;
mod transaction;

use std::io;

use thiserror::Error;

pub use archive::{
    extract_release_archive, extract_updater_binary, release_version, sha256_file, validate_digest,
    MAX_ARCHIVE_BYTES,
};
pub use transaction::{
    acquire_install_lock, apply_staged_release, cleanup_update_staging, commit_install,
    create_update_transaction_dir, recover_incomplete_updates, rollback_install,
    validate_update_transaction_dir, write_update_health_marker, UpdateInstallReceipt,
};
#[cfg(windows)]
mod platform;
#[cfg(windows)]
pub use platform::{
    show_update_failure, wait_for_parent_process, wait_for_process, ParentProcessHandle,
};
#[cfg(not(windows))]
pub fn show_update_failure(message: &str) {
    eprintln!("HD2 Armor Desk 更新失败：{message}");
}
pub const RELEASES_PAGE_URL: &str = "https://github.com/lona-cn/hd2-sav-editor/releases";

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("{0}")]
    InvalidPackage(String),
    #[error("{0}")]
    Install(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Cursor, Write};
    use std::path::Path;

    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    use super::{
        apply_staged_release, cleanup_update_staging, commit_install,
        create_update_transaction_dir, extract_release_archive, recover_incomplete_updates,
    };

    const RELEASE_TAG: &str = "release-20260926-000000000-UTC8";

    fn make_archive(extra: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        let files = [
            ("hd2-armor-desk.exe", b"app".as_slice()),
            ("hd2-armor-desk-updater.exe", b"updater".as_slice()),
            ("README.md", b"readme".as_slice()),
            ("LICENSE", b"license".as_slice()),
            ("docs/images/armor-desk-overview.png", b"image".as_slice()),
            (
                "VERSION.txt",
                b"HD2 Armor Desk\nVersion: 20260926-000000000-UTC8\n".as_slice(),
            ),
        ];
        for (name, contents) in files.into_iter().chain(extra.iter().copied()) {
            writer.start_file(name, options).unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn write_archive(directory: &Path, bytes: &[u8]) -> std::path::PathBuf {
        let path = directory.join("package.zip");
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn extracts_only_the_managed_release_files_and_preserves_workspace() {
        let root = tempfile::tempdir().unwrap();
        let archive = write_archive(root.path(), &make_archive(&[]));
        let install = root.path().join("install");
        let staging = root.path().join("staging");
        fs::create_dir_all(&install).unwrap();
        fs::create_dir_all(install.join("workspace")).unwrap();
        fs::write(install.join("workspace/user.json"), b"keep me").unwrap();

        extract_release_archive(&archive, &staging, RELEASE_TAG).unwrap();

        assert_eq!(
            fs::read(staging.join("hd2-armor-desk.exe")).unwrap(),
            b"app"
        );
        assert_eq!(
            fs::read(staging.join("hd2-armor-desk-updater.exe")).unwrap(),
            b"updater"
        );
        assert_eq!(
            fs::read(install.join("workspace/user.json")).unwrap(),
            b"keep me"
        );
        assert!(staging
            .join("docs/images/armor-desk-overview.png")
            .is_file());
    }

    #[test]
    fn rejects_unexpected_paths_without_leaving_a_partial_stage() {
        let root = tempfile::tempdir().unwrap();
        let archive = write_archive(
            root.path(),
            &make_archive(&[("../workspace/user.json", b"overwrite")]),
        );
        let staging = root.path().join("staging");

        assert!(extract_release_archive(&archive, &staging, RELEASE_TAG).is_err());
        assert!(!staging.exists());
    }

    #[test]
    fn rejects_packages_for_a_different_release_tag() {
        let root = tempfile::tempdir().unwrap();
        let archive = write_archive(root.path(), &make_archive(&[]));
        let staging = root.path().join("staging");

        assert!(
            extract_release_archive(&archive, &staging, "release-20260927-000000000-UTC8").is_err()
        );
        assert!(!staging.exists());
    }

    #[test]
    fn uncommitted_install_is_recovered_without_touching_workspace() {
        let root = tempfile::tempdir().unwrap();
        let install = root.path().join("install");
        fs::create_dir_all(install.join("workspace")).unwrap();
        fs::write(install.join("hd2-armor-desk.exe"), b"old app").unwrap();
        fs::write(install.join("README.md"), b"old readme").unwrap();
        fs::write(install.join("workspace/user.json"), b"user data").unwrap();
        let transaction =
            create_update_transaction_dir(&install, "job-20260926-000000000").unwrap();
        let archive = write_archive(root.path(), &make_archive(&[]));
        let stage = transaction.join("preflight");
        extract_release_archive(&archive, &stage, RELEASE_TAG).unwrap();

        apply_staged_release(&install, &stage, &transaction, RELEASE_TAG).unwrap();
        assert_eq!(
            fs::read(install.join("hd2-armor-desk.exe")).unwrap(),
            b"app"
        );
        assert_eq!(
            fs::read(install.join("workspace/user.json")).unwrap(),
            b"user data"
        );

        assert_eq!(recover_incomplete_updates(&install).unwrap(), 1);
        assert_eq!(
            fs::read(install.join("hd2-armor-desk.exe")).unwrap(),
            b"old app"
        );
        assert_eq!(fs::read(install.join("README.md")).unwrap(), b"old readme");
        assert!(!install.join("hd2-armor-desk-updater.exe").exists());
        assert_eq!(
            fs::read(install.join("workspace/user.json")).unwrap(),
            b"user data"
        );

        cleanup_update_staging(&install).unwrap();
        assert!(install.join(".hd2-update/owner").is_file());
        assert_eq!(
            fs::read(install.join("workspace/user.json")).unwrap(),
            b"user data"
        );
    }

    #[test]
    fn committed_install_keeps_new_files_and_cleanup_removes_only_update_staging() {
        let root = tempfile::tempdir().unwrap();
        let install = root.path().join("install");
        fs::create_dir_all(install.join("workspace")).unwrap();
        fs::write(install.join("hd2-armor-desk.exe"), b"old app").unwrap();
        fs::write(install.join("workspace/user.json"), b"user data").unwrap();
        let transaction =
            create_update_transaction_dir(&install, "job-20260926-000000001").unwrap();
        let archive = write_archive(root.path(), &make_archive(&[]));
        let stage = transaction.join("preflight");
        extract_release_archive(&archive, &stage, RELEASE_TAG).unwrap();

        apply_staged_release(&install, &stage, &transaction, RELEASE_TAG).unwrap();
        commit_install(&transaction).unwrap();
        assert_eq!(recover_incomplete_updates(&install).unwrap(), 0);
        cleanup_update_staging(&install).unwrap();

        assert_eq!(
            fs::read(install.join("hd2-armor-desk.exe")).unwrap(),
            b"app"
        );
        assert!(install.join("hd2-armor-desk-updater.exe").is_file());
        assert_eq!(
            fs::read(install.join("workspace/user.json")).unwrap(),
            b"user data"
        );
        assert_eq!(
            fs::read_dir(install.join(".hd2-update")).unwrap().count(),
            1
        );
    }
}
