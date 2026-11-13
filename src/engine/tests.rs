//! End-to-end tests of the engine on temporary folders.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::crypto::{Cipher, KdfParams, VaultKey};
use super::manifest::META_DIR;
use super::plan::{
    self, BackupInput, BackupPlan, ItemKind, RestoreOptions, RestorePlan, RestoreTarget,
};
use super::sources::Sources;
use super::{CancelToken, Progress, backup, restore, snapshots, vault};
use crate::config::{BackupMode, Config, ConflictPolicy, Source};
use crate::platform::known_paths::KnownPaths;
use crate::platform::vss::LiveFiles;

const COMPUTER: &str = "TESTPC";

fn quiet() -> impl FnMut(&Progress) {
    |_| {}
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

fn setup() -> (tempfile::TempDir, PathBuf, Config) {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("Documents");
    write(&source.join("letter.txt"), "Dear archive,");
    write(&source.join("photos/summer.jpg"), "not really a jpeg");
    write(&source.join("cache/skip.tmp"), "temporary");

    let config = Config {
        destination: tmp.path().join("Vault"),
        sources: vec![Source::new("Documents", &source, true)],
        ..Config::default()
    };
    (tmp, source, config)
}

fn plan(config: &Config, key: Option<&VaultKey>) -> BackupPlan {
    let sources = Sources::collect(config, &KnownPaths::current());
    plan_with(config, &sources, key)
}

fn plan_with(config: &Config, sources: &Sources, key: Option<&VaultKey>) -> BackupPlan {
    let input = BackupInput {
        config,
        sources,
        computer: COMPUTER,
        key,
    };
    plan::plan_backup(&input, &CancelToken::default(), &mut quiet()).unwrap()
}

fn run(plan: &BackupPlan, key: Option<&VaultKey>) -> backup::BackupReport {
    backup::run_backup(plan, key, &LiveFiles, &CancelToken::default(), &mut quiet()).unwrap()
}

fn restore_plan(destination: &Path, target: RestoreTarget, key: Option<&VaultKey>) -> RestorePlan {
    let latest = snapshots::find(destination, "latest", COMPUTER, key).unwrap();
    let options = RestoreOptions {
        target,
        conflict: ConflictPolicy::ReplaceChanged,
        verify: true,
        skip: HashSet::new(),
    };
    plan::plan_restore(&latest, options, key, &CancelToken::default(), &mut quiet()).unwrap()
}

/// Changes a stored file on purpose. Backed-up files are read-only, which is
/// itself checked here.
fn tamper(path: &Path) {
    assert!(
        fs::write(path, "tampered").is_err(),
        "backed-up files are read-only"
    );
    let mut permissions = fs::metadata(path).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions).unwrap();
    fs::write(path, "tampered").unwrap();
}

fn list_all_files(root: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect()
}

#[test]
fn preview_does_not_write_anything() {
    let (tmp, _source, config) = setup();
    let before = list_all_files(tmp.path());
    let plan = plan(&config, None);
    assert_eq!(
        plan.summary.count(ItemKind::New),
        2,
        "*.tmp is excluded by default"
    );
    assert!(
        !config.destination.exists(),
        "preview must not create the destination"
    );
    assert_eq!(before, list_all_files(tmp.path()));
}

#[test]
fn full_then_incremental_then_restore() {
    let (tmp, source, mut config) = setup();

    // First backup: everything is new.
    config.mode = BackupMode::Full;
    let report1 = run(&plan(&config, None), None);
    assert_eq!(report1.header.stats.copied_files, 2);
    // Flat layout: the source folder sits directly in the backup folder.
    assert!(
        report1
            .snapshot_dir
            .join("Documents")
            .join("letter.txt")
            .is_file()
    );
    assert!(
        report1
            .snapshot_dir
            .join(META_DIR)
            .join("snapshot.json")
            .is_file()
    );
    assert_eq!(report1.snapshot_dir.parent().unwrap(), config.destination);

    // Change one file, add one, remove one.
    std::thread::sleep(std::time::Duration::from_millis(20));
    write(&source.join("letter.txt"), "Dear archive, a second page.");
    write(&source.join("new.txt"), "fresh");
    fs::remove_file(source.join("photos/summer.jpg")).unwrap();

    config.mode = BackupMode::Incremental;
    let plan2 = plan(&config, None);
    assert_eq!(plan2.summary.count(ItemKind::New), 1);
    assert_eq!(plan2.summary.count(ItemKind::Changed), 1);
    assert_eq!(plan2.summary.count(ItemKind::Removed), 1);
    let report2 = run(&plan2, None);
    assert_eq!(report2.header.stats.copied_files, 2);
    assert_ne!(
        report1.header.id, report2.header.id,
        "same minute gets a suffix"
    );

    // Third backup without changes: nothing is copied.
    let plan3 = plan(&config, None);
    assert_eq!(plan3.summary.count(ItemKind::Unchanged), 2);
    let report3 = run(&plan3, None);
    assert_eq!(report3.header.stats.copied_files, 0);
    assert_eq!(
        report3.header.stats.linked_files + report3.header.stats.referenced_files,
        2
    );

    assert_eq!(snapshots::list(&config.destination, None).unwrap().len(), 3);

    // Restore the latest backup into a separate folder.
    let target = tmp.path().join("Restored");
    let restore_plan1 = restore_plan(
        &config.destination,
        RestoreTarget::Folder(target.clone()),
        None,
    );
    assert_eq!(restore_plan1.summary.count(ItemKind::New), 2);
    assert!(!target.exists(), "restore preview must not write");

    let report =
        restore::run_restore(&restore_plan1, None, &CancelToken::default(), &mut quiet()).unwrap();
    assert_eq!(report.restored_files, 2);
    assert_eq!(
        fs::read_to_string(target.join("Documents/letter.txt")).unwrap(),
        "Dear archive, a second page."
    );

    // A second restore finds everything identical.
    let again = restore_plan(
        &config.destination,
        RestoreTarget::Folder(target.clone()),
        None,
    );
    assert_eq!(again.summary.count(ItemKind::Unchanged), 2);
    assert!(
        again.sources[again.items[0].source]
            .root
            .starts_with(&target)
    );
}

#[test]
fn damaged_backup_is_detected_on_restore() {
    let (tmp, _source, config) = setup();
    let report = run(&plan(&config, None), None);

    // Silently corrupt a stored file.
    tamper(&report.snapshot_dir.join("Documents/letter.txt"));

    let target = tmp.path().join("Restored");
    let restore_plan = restore_plan(
        &config.destination,
        RestoreTarget::Folder(target.clone()),
        None,
    );
    let result =
        restore::run_restore(&restore_plan, None, &CancelToken::default(), &mut quiet()).unwrap();
    assert_eq!(result.failed, 1);
    assert!(!target.join("Documents/letter.txt").exists());
}

#[test]
fn chosen_destination_gets_an_app_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let drive = tmp.path().join("Backup drive");
    fs::create_dir_all(&drive).unwrap();
    assert_eq!(
        snapshots::chosen_destination(&drive, true),
        drive.join(snapshots::APP_FOLDER)
    );
    assert_eq!(snapshots::chosen_destination(&drive, false), drive);

    let named = tmp.path().join("aeternavault");
    assert_eq!(snapshots::chosen_destination(&named, true), named);

    // A folder that already holds backups is used as it is.
    let existing = tmp.path().join("Old backups");
    fs::create_dir_all(existing.join("2026-09-14 20-00").join(META_DIR)).unwrap();
    assert_eq!(snapshots::chosen_destination(&existing, true), existing);
}

#[test]
fn destination_inside_source_is_skipped() {
    let (_tmp, source, mut config) = setup();
    config.destination = source.join("Backups");
    run(&plan(&config, None), None);

    // The second run must not back up the first backup.
    let plan2 = plan(&config, None);
    assert!(
        plan2
            .items
            .iter()
            .all(|i| !i.rel.to_lowercase().starts_with("backups/"))
    );
}

#[test]
fn partial_selection_is_respected() {
    let (_tmp, _source, mut config) = setup();
    config.sources[0].exclude_paths = vec!["photos".into()];
    let only_letter = plan(&config, None);
    assert_eq!(only_letter.summary.count(ItemKind::New), 1);
    assert_eq!(only_letter.items[0].rel, "letter.txt");

    config.sources[0].exclude_paths = vec![".".into()];
    config.sources[0].include_paths = vec!["photos".into()];
    let only_photos = plan(&config, None);
    assert_eq!(only_photos.summary.count(ItemKind::New), 1);
    assert_eq!(only_photos.items[0].rel, "photos/summer.jpg");
}

#[test]
fn encrypted_backup_hides_names_deduplicates_and_restores() {
    let (tmp, _source, mut config) = setup();
    let created = vault::create_with(
        &config.destination,
        "secret words",
        vault::VaultOptions {
            cipher: Cipher::XChaCha20Poly1305,
            kdf: KdfParams::TEST,
        },
        KdfParams::TEST,
    )
    .unwrap();
    config.encryption.enabled = true;
    let key = created.key;

    // Without the key, planning refuses.
    let sources = Sources::collect(&config, &KnownPaths::current());
    let input = BackupInput {
        config: &config,
        sources: &sources,
        computer: COMPUTER,
        key: None,
    };
    assert!(plan::plan_backup(&input, &CancelToken::default(), &mut quiet()).is_err());

    let report = run(&plan(&config, Some(&key)), Some(&key));
    assert_eq!(report.header.stats.copied_files, 2);
    assert!(report.header.encrypted);

    // No plain names or contents anywhere in the vault.
    for file in list_all_files(&config.destination) {
        let name = file.display().to_string();
        assert!(
            !name.contains("letter") && !name.contains("summer"),
            "{name}"
        );
        if file.extension().is_some_and(|e| e == "avb") {
            let bytes = fs::read(&file).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("Dear archive"));
        }
    }

    // Locked listing shows the backup without details.
    let locked = snapshots::list(&config.destination, None).unwrap();
    assert!(locked[0].is_locked());

    // A second backup stores nothing new.
    let report2 = run(&plan(&config, Some(&key)), Some(&key));
    assert_eq!(report2.header.stats.copied_files, 0);

    let target = tmp.path().join("Restored");
    let restore_plan = restore_plan(
        &config.destination,
        RestoreTarget::Folder(target.clone()),
        Some(&key),
    );
    let result = restore::run_restore(
        &restore_plan,
        Some(&key),
        &CancelToken::default(),
        &mut quiet(),
    )
    .unwrap();
    assert_eq!(result.restored_files, 2);
    assert_eq!(
        fs::read_to_string(target.join("Documents/letter.txt")).unwrap(),
        "Dear archive,"
    );

    // Wrong key cannot read the index.
    let wrong = VaultKey::generate(Cipher::default());
    let listed = snapshots::list(&config.destination, Some(&wrong)).unwrap();
    assert!(listed.iter().all(|s| s.header.is_none()));
}

#[test]
fn partly_encrypted_backup_keeps_marked_files_in_the_vault() {
    let (tmp, source, mut config) = setup();
    let created = vault::create_with(
        &config.destination,
        "secret words",
        vault::VaultOptions {
            cipher: Cipher::Aes256Gcm,
            kdf: KdfParams::TEST,
        },
        KdfParams::TEST,
    )
    .unwrap();
    let key = created.key;
    config.encryption.enabled = true;
    config.encryption.scope = crate::config::EncryptionScope::Selected;
    config.sources[0].encrypt_paths = vec!["photos".into()];

    let plan1 = plan(&config, Some(&key));
    assert!(plan1.plain_part && plan1.encrypted_part);
    let report = run(&plan1, Some(&key));
    assert!(report.header.split && !report.header.encrypted);
    assert_eq!(report.header.stats.files, 1);
    assert_eq!(report.encrypted_part.as_ref().unwrap().stats.files, 1);
    assert!(report.snapshot_dir.join("Documents/letter.txt").is_file());
    assert!(!report.snapshot_dir.join("Documents/photos").exists());
    for file in list_all_files(&vault::vault_dir(&config.destination)) {
        assert!(!file.display().to_string().contains("summer"));
    }

    // One entry in the list, with its encrypted part attached.
    let listed = snapshots::list(&config.destination, None).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].is_split() && listed[0].needs_unlock());

    // Nothing changed: nothing is copied in either part.
    let report2 = run(&plan(&config, Some(&key)), Some(&key));
    assert_eq!(report2.total_stats().copied_files, 0);

    // Unmarking the folder moves the photos into the plain part.
    config.sources[0].encrypt_paths.clear();
    let plan3 = plan(&config, Some(&key));
    assert!(plan3.plain_part && !plan3.encrypted_part);
    let report3 = run(&plan3, Some(&key));
    assert!(
        report3
            .snapshot_dir
            .join("Documents/photos/summer.jpg")
            .is_file()
    );
    assert_eq!(
        report3.header.stats.copied_files, 1,
        "only the photo is new here"
    );

    // Restoring the partly encrypted backup needs the key and brings back both.
    let all = snapshots::list(&config.destination, Some(&key)).unwrap();
    let split = all.iter().find(|s| s.is_split()).unwrap().clone();
    let options = || RestoreOptions {
        target: RestoreTarget::Folder(tmp.path().join("Restored")),
        conflict: ConflictPolicy::ReplaceChanged,
        verify: true,
        skip: HashSet::new(),
    };
    let locked = snapshots::list(&config.destination, None).unwrap();
    let locked_split = locked.iter().find(|s| s.is_split()).unwrap();
    assert!(matches!(
        plan::plan_restore(
            locked_split,
            options(),
            None,
            &CancelToken::default(),
            &mut quiet()
        ),
        Err(crate::error::EngineError::Locked)
    ));
    let restore_plan = plan::plan_restore(
        &split,
        options(),
        Some(&key),
        &CancelToken::default(),
        &mut quiet(),
    )
    .unwrap();
    let result = restore::run_restore(
        &restore_plan,
        Some(&key),
        &CancelToken::default(),
        &mut quiet(),
    )
    .unwrap();
    assert_eq!(result.restored_files, 2);
    let restored = tmp.path().join("Restored").join("Documents");
    assert_eq!(
        fs::read_to_string(restored.join("photos/summer.jpg")).unwrap(),
        "not really a jpeg"
    );
    let _ = source;
}

#[test]
fn delete_verify_and_move_backups() {
    use super::{manage, verify};
    let (tmp, _source, mut config) = setup();
    // Without hard links, the second backup points into the first one.
    config.advanced.hardlink_unchanged = false;
    let first = run(&plan(&config, None), None);
    let second = run(&plan(&config, None), None);
    assert_eq!(second.header.stats.referenced_files, 2);

    let delete_report = manage::delete(
        &config.destination,
        std::slice::from_ref(&first.header.id),
        None,
        &CancelToken::default(),
        &mut quiet(),
    )
    .unwrap();
    assert_eq!(delete_report.rehomed_files, 2);
    assert!(!first.snapshot_dir.exists());
    let listed = snapshots::list(&config.destination, None).unwrap();
    assert_eq!(listed.len(), 1);
    let check = verify::verify(
        &listed[0],
        None,
        true,
        &CancelToken::default(),
        &mut quiet(),
    )
    .unwrap();
    assert!(check.is_ok(), "{check:?}");
    assert!(second.snapshot_dir.join("Documents/letter.txt").is_file());

    // Damage is found without restoring.
    tamper(&second.snapshot_dir.join("Documents/letter.txt"));
    let check = verify::verify(
        &listed[0],
        None,
        true,
        &CancelToken::default(),
        &mut quiet(),
    )
    .unwrap();
    assert_eq!(check.damaged, vec!["Documents/letter.txt".to_string()]);
    fs::write(
        second.snapshot_dir.join("Documents/letter.txt"),
        "Dear archive,",
    )
    .unwrap();

    // An encrypted backup moves together with the vault header; its content
    // is removed from the old place.
    let created = vault::create_with(
        &config.destination,
        "secret words",
        vault::VaultOptions {
            cipher: Cipher::XChaCha20Poly1305,
            kdf: KdfParams::TEST,
        },
        KdfParams::TEST,
    )
    .unwrap();
    let key = created.key;
    config.encryption.enabled = true;
    run(&plan(&config, Some(&key)), Some(&key));
    let other = tmp.path().join("Other place");
    let listed = snapshots::list(&config.destination, Some(&key)).unwrap();
    let encrypted = listed.iter().find(|s| s.is_encrypted()).unwrap();
    let moved = manage::transfer(
        encrypted,
        &other,
        Some(&key),
        &CancelToken::default(),
        &mut quiet(),
    )
    .unwrap();
    assert_eq!(moved.files, 2);
    assert_eq!(moved.delete.pruned_blobs, 2);
    let there = snapshots::list(&other, Some(&key)).unwrap();
    assert_eq!(there.len(), 1);
    assert!(there[0].header.is_some(), "readable with the same key");
    assert!(vault::unlock(&other, "secret words").is_ok());

    // The plain backup moves as well and stays complete.
    let listed = snapshots::list(&config.destination, None).unwrap();
    let plain = listed.iter().find(|s| !s.is_encrypted()).unwrap();
    manage::transfer(plain, &other, None, &CancelToken::default(), &mut quiet()).unwrap();
    let there = snapshots::list(&other, Some(&key)).unwrap();
    let plain_there = there.iter().find(|s| !s.is_encrypted()).unwrap();
    let check = verify::verify(
        plain_there,
        None,
        true,
        &CancelToken::default(),
        &mut quiet(),
    )
    .unwrap();
    assert!(check.is_ok() && check.files == 2, "{check:?}");
    assert!(
        snapshots::list(&config.destination, Some(&key))
            .unwrap()
            .is_empty()
    );
}

/// Writes encrypted sample backups for checking `tools/aeterna-decrypt.py`:
///
/// ```text
/// $env:AETERNAVAULT_REFERENCE_DIR = "$env:TEMP\aeterna-reference"
/// cargo test write_reference_vaults -- --ignored
/// python tools/aeterna-decrypt.py "$env:TEMP\aeterna-reference\xchacha20poly1305\Vault" extract latest out
/// ```
#[test]
#[ignore = "writes sample vaults for the Python decryptor"]
fn write_reference_vaults() {
    let root = PathBuf::from(
        std::env::var_os("AETERNAVAULT_REFERENCE_DIR").expect("set AETERNAVAULT_REFERENCE_DIR"),
    );
    for (name, cipher) in [
        ("xchacha20poly1305", Cipher::XChaCha20Poly1305),
        ("aes256gcm", Cipher::Aes256Gcm),
    ] {
        let base = root.join(name);
        let _ = fs::remove_dir_all(&base);
        let source = base.join("Documents");
        write(&source.join("letter.txt"), "Dear archive,");
        write(&source.join("empty.txt"), "");
        let big: String = (0..(3 * 1024 * 1024 + 123))
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        write(&source.join("sub folder/big.txt"), &big);
        let mut config = Config {
            destination: base.join("Vault"),
            sources: vec![Source::new("Documents", &source, true)],
            ..Config::default()
        };
        let created = vault::create_with(
            &config.destination,
            "reference passphrase",
            vault::VaultOptions {
                cipher,
                kdf: KdfParams::TEST,
            },
            KdfParams::TEST,
        )
        .unwrap();
        fs::write(base.join("recovery-key.txt"), &created.recovery_key).unwrap();
        config.encryption.enabled = true;
        run(&plan(&config, Some(&created.key)), Some(&created.key));
    }
}

#[test]
fn folders_under_known_places_are_stored_portably() {
    let (tmp, _source, mut config) = setup();
    let profile = tmp.path().join("Profile");
    let settings = profile.join("AppData").join("Roaming").join("TestApp");
    write(&settings.join("settings.json"), "{\"theme\":\"dark\"}");
    config.sources.push(Source::new("TestApp", &settings, true));

    let known = KnownPaths {
        values: vec![
            ("APPDATA".into(), profile.join("AppData").join("Roaming")),
            ("USERPROFILE".into(), profile.clone()),
        ],
    };
    let sources = Sources::collect(&config, &known);
    assert_eq!(sources.folders.len(), 2);
    let expected = format!("{{APPDATA}}{}TestApp", std::path::MAIN_SEPARATOR);
    assert_eq!(
        sources.folders[1].portable.as_deref(),
        Some(expected.as_str())
    );

    let report = run(&plan_with(&config, &sources, None), None);
    assert!(report.snapshot_dir.join("TestApp/settings.json").is_file());
    let record = report
        .header
        .sources
        .iter()
        .find(|s| s.key == "TestApp")
        .unwrap();
    assert_eq!(record.portable.as_deref(), Some(expected.as_str()));
}

#[test]
fn destination_lock_blocks_until_its_process_ends() {
    use super::fsops::DestinationLock;
    let tmp = tempfile::tempdir().unwrap();
    let first = DestinationLock::acquire(tmp.path()).unwrap();
    assert!(first.is_some());
    // Held by a running process (this one): a second backup must wait.
    assert!(DestinationLock::acquire(tmp.path()).unwrap().is_none());
    drop(first);

    // Left over by a process of this computer that has ended: taken over at once.
    let computer = crate::platform::computer_name();
    write(
        &tmp.path().join(".aeternavault.lock"),
        &format!("99999999 {computer}\n"),
    );
    assert!(DestinationLock::acquire(tmp.path()).unwrap().is_some());

    // Held by another computer and recent: still respected.
    write(&tmp.path().join(".aeternavault.lock"), "1234 OTHER-PC\n");
    assert!(DestinationLock::acquire(tmp.path()).unwrap().is_none());
}

#[test]
fn same_drive_is_recognised() {
    use crate::platform::same_drive;
    let tmp = tempfile::tempdir().unwrap();
    // A destination that does not exist yet is judged by its parent.
    assert!(same_drive(tmp.path(), &tmp.path().join("not/yet/there")));
    #[cfg(windows)]
    {
        assert!(same_drive(
            Path::new(r"C:\Users\a"),
            Path::new(r"c:\Backups")
        ));
        assert!(!same_drive(
            Path::new(r"C:\Users\a"),
            Path::new(r"E:\Backups")
        ));
    }
}

#[test]
fn damaged_earlier_copy_is_not_reused() {
    let (_tmp, source, config) = setup();
    let first = run(&plan(&config, None), None);
    // The copy in the first backup is cut short (e.g. by a power failure).
    tamper(&first.snapshot_dir.join("Documents/letter.txt"));

    std::thread::sleep(std::time::Duration::from_millis(1100));
    let second = run(&plan(&config, None), None);
    assert_ne!(first.snapshot_dir, second.snapshot_dir);
    assert_eq!(
        fs::read_to_string(second.snapshot_dir.join("Documents/letter.txt")).unwrap(),
        fs::read_to_string(source.join("letter.txt")).unwrap(),
        "the file is copied again instead of linked to the damaged copy"
    );
}

#[test]
fn encrypted_stream_size_is_exact() {
    use super::crypto::{BLOB_MAGIC, GCM_MAGIC, stream_size};
    let key = VaultKey::from_master([7u8; 32], Cipher::XChaCha20Poly1305);
    for plain in [0usize, 1, 1024 * 1024, 1024 * 1024 + 1, 3 * 1024 * 1024] {
        let data = vec![1u8; plain];
        let mut out = Vec::new();
        key.encrypt_stream(
            &mut data.as_slice(),
            &mut out,
            &CancelToken::default(),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(
            stream_size(BLOB_MAGIC, plain as u64),
            Some(out.len() as u64),
            "{plain}"
        );
    }
    let key = VaultKey::from_master([7u8; 32], Cipher::Aes256Gcm);
    let mut out = Vec::new();
    key.encrypt_stream(
        &mut [1u8; 10].as_slice(),
        &mut out,
        &CancelToken::default(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(&out[..4], GCM_MAGIC);
    assert_eq!(stream_size(GCM_MAGIC, 10), Some(out.len() as u64));
}

fn test_vault(destination: &Path, passphrase: &str) -> VaultKey {
    vault::create_with(
        destination,
        passphrase,
        vault::VaultOptions {
            cipher: Cipher::XChaCha20Poly1305,
            kdf: KdfParams::TEST,
        },
        KdfParams::TEST,
    )
    .unwrap()
    .key
}

/// Audit F03 / R01: the readable header of a partly encrypted backup is
/// checked against the encrypted one; an edited header stops the restore
/// before anything is written.
#[test]
fn edited_plain_header_cannot_redirect_encrypted_files() {
    let (tmp, source, mut config) = setup();
    let key = test_vault(&config.destination, "secret words");
    config.encryption.enabled = true;
    config.encryption.scope = crate::config::EncryptionScope::Selected;
    config.sources[0].encrypt_paths = vec!["photos".into()];
    let report = run(&plan(&config, Some(&key)), Some(&key));
    assert!(report.header.split);

    // Unchanged, the backup restores and verifies.
    fs::remove_dir_all(source.join("photos")).unwrap();
    let good = restore_plan(&config.destination, RestoreTarget::Original, Some(&key));
    restore::run_restore(&good, Some(&key), &CancelToken::default(), &mut quiet()).unwrap();
    assert!(source.join("photos/summer.jpg").is_file());

    // Someone with access to the storage points the folder elsewhere.
    let evil = tmp.path().join("Evil");
    let header_file = report.snapshot_dir.join(META_DIR).join("snapshot.json");
    let mut header: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&header_file).unwrap()).unwrap();
    for record in header["sources"].as_array_mut().unwrap() {
        record["path"] = serde_json::json!(evil);
        record["portable"] = serde_json::Value::Null;
    }
    fs::write(&header_file, serde_json::to_string(&header).unwrap()).unwrap();

    let latest = snapshots::find(&config.destination, "latest", COMPUTER, Some(&key)).unwrap();
    let options = RestoreOptions {
        target: RestoreTarget::Original,
        conflict: ConflictPolicy::ReplaceChanged,
        verify: true,
        skip: HashSet::new(),
    };
    let refused = plan::plan_restore(
        &latest,
        options,
        Some(&key),
        &CancelToken::default(),
        &mut quiet(),
    );
    assert!(
        matches!(refused, Err(crate::error::EngineError::Manifest { .. })),
        "{refused:?}"
    );
    let check = super::verify::verify(
        &latest,
        Some(&key),
        true,
        &CancelToken::default(),
        &mut quiet(),
    );
    assert!(check.is_err(), "verify reports the edited part");
    assert!(!evil.exists(), "nothing written to the edited path");
}

/// Places a readable part with one file (`Evil/planted.txt`, content
/// `planted`) next to an encrypted backup, as someone with write access to
/// the storage but without the key could. Returns the folder it names.
fn plant_readable_part(tmp: &Path, report: &backup::BackupReport) -> PathBuf {
    use super::manifest::{FileEntry, FileIndex, SourceRecord};
    let evil = tmp.join("Evil");
    let mut header = report.header.clone();
    header.encrypted = false;
    header.split = true;
    header.sources = vec![SourceRecord {
        key: "Evil".into(),
        name: "Evil".into(),
        path: evil.clone(),
        portable: None,
    }];
    let content = b"planted";
    write(&report.snapshot_dir.join("Evil/planted.txt"), "planted");
    let index = FileIndex {
        format: header.format,
        files: vec![FileEntry {
            mode: None,
            source: "Evil".into(),
            path: "planted.txt".into(),
            size: content.len() as u64,
            modified: 0,
            sha256: super::format_sha256(&<sha2::Sha256 as sha2::Digest>::digest(content)),
            blob: format!("{}/Evil/planted.txt", report.header.id),
            link: None,
        }],
        warnings: Vec::new(),
    };
    let meta = report.snapshot_dir.join(META_DIR);
    fs::write(meta.join("files.json"), serde_json::to_vec(&index).unwrap()).unwrap();
    fs::write(
        meta.join("snapshot.json"),
        serde_json::to_vec(&header).unwrap(),
    )
    .unwrap();
    evil
}

/// Audit R01: readable files placed next to a fully encrypted backup are
/// never restored.
#[test]
fn readable_part_added_to_an_encrypted_backup_is_refused() {
    let (tmp, _source, mut config) = setup();
    let key = test_vault(&config.destination, "secret words");
    config.encryption.enabled = true;
    let report = run(&plan(&config, Some(&key)), Some(&key));
    assert!(!report.header.split);
    let evil = plant_readable_part(tmp.path(), &report);

    let latest = snapshots::find(&config.destination, "latest", COMPUTER, Some(&key)).unwrap();
    assert!(latest.is_split(), "the planted part is seen");
    let options = RestoreOptions {
        target: RestoreTarget::Original,
        conflict: ConflictPolicy::ReplaceChanged,
        verify: true,
        skip: HashSet::new(),
    };
    let refused = plan::plan_restore(
        &latest,
        options,
        Some(&key),
        &CancelToken::default(),
        &mut quiet(),
    );
    assert!(
        matches!(refused, Err(crate::error::EngineError::Manifest { .. })),
        "{refused:?}"
    );
    assert!(!evil.exists(), "nothing planted");
}

/// Audit R06: on Linux, `A.txt` and `a.txt` are two files, also in an
/// incremental backup.
#[cfg(unix)]
#[test]
fn names_that_differ_only_in_case_stay_apart() {
    let (tmp, source, mut config) = setup();
    write(&source.join("A.txt"), "AAAA");
    write(&source.join("a.txt"), "BBBB");
    let time = super::to_unix_nanos(
        fs::metadata(source.join("A.txt"))
            .unwrap()
            .modified()
            .unwrap(),
    );
    super::fsops::set_modified(&source.join("a.txt"), time).unwrap();
    config.mode = BackupMode::Full;
    run(&plan(&config, None), None);
    config.mode = BackupMode::Incremental;
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let second = plan(&config, None);
    assert_eq!(second.summary.count(ItemKind::Unchanged), 4);
    run(&second, None);

    let target = tmp.path().join("Restored");
    let plan = restore_plan(
        &config.destination,
        RestoreTarget::Folder(target.clone()),
        None,
    );
    restore::run_restore(&plan, None, &CancelToken::default(), &mut quiet()).unwrap();
    assert_eq!(
        fs::read_to_string(target.join("Documents/A.txt")).unwrap(),
        "AAAA"
    );
    assert_eq!(
        fs::read_to_string(target.join("Documents/a.txt")).unwrap(),
        "BBBB"
    );
}

/// Audit F05: content is never encrypted with the key of another vault.
#[test]
fn backup_with_the_key_of_another_vault_is_refused() {
    let (tmp, _source, mut config) = setup();
    let key_a = test_vault(&tmp.path().join("VaultA"), "a");
    let _key_b = test_vault(&config.destination, "b");
    config.encryption.enabled = true;
    let plan = plan(&config, Some(&key_a));
    let result = backup::run_backup(
        &plan,
        Some(&key_a),
        &LiveFiles,
        &CancelToken::default(),
        &mut quiet(),
    );
    assert!(
        matches!(result, Err(crate::error::EngineError::Locked)),
        "{result:?}"
    );
    assert!(
        snapshots::list(&config.destination, None)
            .unwrap()
            .is_empty()
    );
}

/// Audit F07: a stored blob damaged without changing its size is replaced
/// by the next full backup instead of being reused.
#[test]
fn damaged_encrypted_content_is_repaired_by_a_full_backup() {
    let (_tmp, _source, mut config) = setup();
    let key = test_vault(&config.destination, "secret");
    config.encryption.enabled = true;
    run(&plan(&config, Some(&key)), Some(&key));
    for blob in list_all_files(&vault::vault_dir(&config.destination).join(vault::BLOB_DIR)) {
        let mut bytes = fs::read(&blob).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x55;
        fs::write(&blob, bytes).unwrap();
    }
    config.mode = BackupMode::Full;
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let second = run(&plan(&config, Some(&key)), Some(&key));
    let newest = snapshots::list(&config.destination, Some(&key))
        .unwrap()
        .into_iter()
        .find(|s| s.id == second.header.id)
        .unwrap();
    let check = super::verify::verify(
        &newest,
        Some(&key),
        true,
        &CancelToken::default(),
        &mut quiet(),
    )
    .unwrap();
    assert!(check.is_ok(), "{check:?}");
}

/// Audit F08: implausible key settings in vault.json are refused before any
/// key derivation.
#[test]
fn implausible_key_settings_are_refused() {
    let (_tmp, _source, config) = setup();
    test_vault(&config.destination, "secret");
    let file = vault::vault_dir(&config.destination).join(vault::VAULT_FILE);
    let mut header: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    header["slots"][0]["kdf"]["memory_kib"] = serde_json::json!(u32::MAX);
    fs::write(&file, serde_json::to_string(&header).unwrap()).unwrap();
    assert!(vault::unlock(&config.destination, "secret").is_err());
}

/// Audit F04: nothing is written through a link inside a chosen folder.
#[test]
fn restore_into_a_folder_does_not_follow_links() {
    let (tmp, _source, config) = setup();
    run(&plan(&config, None), None);
    let target = tmp.path().join("Restored");
    let outside = tmp.path().join("Outside");
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(&outside).unwrap();
    make_link(&outside, &target.join("Documents"));
    let plan = restore_plan(&config.destination, RestoreTarget::Folder(target), None);
    let report = restore::run_restore(&plan, None, &CancelToken::default(), &mut quiet()).unwrap();
    assert!(report.failed > 0);
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        0,
        "nothing written outside"
    );
}

/// Audit R04: an incremental backup with a health check reads the encrypted
/// content it reuses and stores a damaged blob again.
#[test]
fn health_check_repairs_damaged_content_in_an_incremental_backup() {
    let (_tmp, _source, mut config) = setup();
    let key = test_vault(&config.destination, "secret");
    config.encryption.enabled = true;
    config.mode = BackupMode::Incremental;
    run(&plan(&config, Some(&key)), Some(&key));
    for blob in list_all_files(&vault::vault_dir(&config.destination).join(vault::BLOB_DIR)) {
        let mut bytes = fs::read(&blob).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x55;
        fs::write(&blob, bytes).unwrap();
    }
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let mut second = plan(&config, Some(&key));
    assert!(!second.health_check, "the first backup read everything");
    second.health_check = true;
    let report = run(&second, Some(&key));
    assert!(report.header.health_check);
    assert_eq!(report.warnings.len(), 2, "{:?}", report.warnings);
    let newest = snapshots::list(&config.destination, Some(&key))
        .unwrap()
        .into_iter()
        .find(|s| s.id == report.header.id)
        .unwrap();
    let check = super::verify::verify(
        &newest,
        Some(&key),
        true,
        &CancelToken::default(),
        &mut quiet(),
    )
    .unwrap();
    assert!(check.is_ok(), "{check:?}");
    // The next backup does not need another health check.
    assert!(!plan(&config, Some(&key)).health_check);
}

/// Audit R04: the same for a plain copy that changed without changing its
/// size or time.
#[test]
fn health_check_replaces_a_silently_damaged_plain_copy() {
    let (_tmp, _source, mut config) = setup();
    config.mode = BackupMode::Incremental;
    let first = run(&plan(&config, None), None);
    let copy = first.snapshot_dir.join("Documents/letter.txt");
    let modified = super::to_unix_nanos(fs::metadata(&copy).unwrap().modified().unwrap());
    let mut permissions = fs::metadata(&copy).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&copy, permissions).unwrap();
    fs::write(&copy, "Xear archive,").unwrap();
    super::fsops::set_modified(&copy, modified).unwrap();

    std::thread::sleep(std::time::Duration::from_millis(1100));
    let mut second = plan(&config, None);
    second.health_check = true;
    let report = run(&second, None);
    assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
    assert_eq!(
        fs::read_to_string(report.snapshot_dir.join("Documents/letter.txt")).unwrap(),
        "Dear archive,"
    );
}

/// A link (Linux) or junction (Windows) to `target` at `link`.
fn make_link(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).unwrap();
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }
}

/// Audit R02: restoring to the original place does not write through a link
/// that was put inside the backed-up folder; the folder itself may be a link.
#[test]
fn restore_to_the_original_place_does_not_follow_links() {
    let (tmp, source, config) = setup();
    run(&plan(&config, None), None);
    let outside = tmp.path().join("Outside");
    fs::create_dir_all(&outside).unwrap();
    fs::remove_dir_all(source.join("photos")).unwrap();
    make_link(&outside, &source.join("photos"));
    fs::remove_file(source.join("letter.txt")).unwrap();

    let plan = restore_plan(&config.destination, RestoreTarget::Original, None);
    let report = restore::run_restore(&plan, None, &CancelToken::default(), &mut quiet()).unwrap();
    assert_eq!(report.failed, 1, "{:?}", report.warnings);
    assert_eq!(report.restored_files, 1);
    assert!(source.join("letter.txt").is_file());
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        0,
        "nothing written outside"
    );
}

/// Audit R03: on Linux, copies are never more readable than the original.
#[cfg(unix)]
#[test]
fn copies_keep_private_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let (tmp, source, config) = setup();
    fs::set_permissions(source.join("letter.txt"), fs::Permissions::from_mode(0o600)).unwrap();
    write(&source.join("tool.sh"), "#!/bin/sh");
    fs::set_permissions(source.join("tool.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    let report = run(&plan(&config, None), None);
    assert_eq!(
        mode(&report.snapshot_dir.join("Documents/letter.txt")),
        0o400
    );
    assert_eq!(mode(&report.snapshot_dir.join("Documents/tool.sh")), 0o555);

    let target = tmp.path().join("Restored");
    let plan = restore_plan(
        &config.destination,
        RestoreTarget::Folder(target.clone()),
        None,
    );
    restore::run_restore(&plan, None, &CancelToken::default(), &mut quiet()).unwrap();
    assert_eq!(mode(&target.join("Documents/letter.txt")), 0o600);
    assert_eq!(mode(&target.join("Documents/tool.sh")), 0o755);
    assert_eq!(mode(&target.join("Documents")), 0o700);
}

/// Audit F06: a backup is only removed after a move when every file at the
/// new place matched; a damaged copy already there keeps it where it was.
#[test]
fn move_onto_damaged_content_keeps_the_backup() {
    let (tmp, _source, mut config) = setup();
    let key = test_vault(&config.destination, "secret");
    config.encryption.enabled = true;
    let other = tmp.path().join("Other place");
    let first = run(&plan(&config, Some(&key)), Some(&key));
    let listed = snapshots::list(&config.destination, Some(&key)).unwrap();
    let snapshot = listed.iter().find(|s| s.id == first.header.id).unwrap();
    super::manage::transfer(
        snapshot,
        &other,
        Some(&key),
        &CancelToken::default(),
        &mut quiet(),
    )
    .unwrap();

    // The same content once more, while the copy at the new place is damaged.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let second = run(&plan(&config, Some(&key)), Some(&key));
    for blob in list_all_files(&vault::vault_dir(&other).join(vault::BLOB_DIR)) {
        let mut bytes = fs::read(&blob).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x55;
        fs::write(&blob, bytes).unwrap();
    }
    let listed = snapshots::list(&config.destination, Some(&key)).unwrap();
    let snapshot = listed.iter().find(|s| s.id == second.header.id).unwrap();
    let moved = super::manage::transfer(
        snapshot,
        &other,
        Some(&key),
        &CancelToken::default(),
        &mut quiet(),
    );
    assert!(moved.is_err());
    let still_here = snapshots::list(&config.destination, Some(&key)).unwrap();
    let kept = still_here
        .iter()
        .find(|s| s.id == second.header.id)
        .unwrap();
    let check = super::verify::verify(
        kept,
        Some(&key),
        true,
        &CancelToken::default(),
        &mut quiet(),
    )
    .unwrap();
    assert!(check.is_ok(), "the original stays complete: {check:?}");
}

/// Adds every kind of symbolic link to the folder made by `setup`: relative,
/// absolute, dangling, to a folder, and one inside a sub-folder.
#[cfg(unix)]
fn add_links(tmp: &Path, source: &Path) {
    use std::os::unix::fs::symlink;
    let elsewhere = tmp.join("Elsewhere");
    write(&elsewhere.join("far.txt"), "far away");
    symlink("letter.txt", source.join("relative.lnk")).unwrap();
    symlink(elsewhere.join("far.txt"), source.join("absolute.lnk")).unwrap();
    symlink("nowhere/gone.txt", source.join("dangling.lnk")).unwrap();
    symlink("photos", source.join("folder.lnk")).unwrap();
    symlink("../letter.txt", source.join("photos/up.lnk")).unwrap();
}

/// Every symbolic link below `root` with the target it names.
#[cfg(unix)]
fn link_targets(root: &Path) -> std::collections::BTreeMap<String, String> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .flatten()
        .filter(|e| e.path_is_symlink())
        .map(|e| {
            let rel = e.path().strip_prefix(root).unwrap();
            let target = fs::read_link(e.path()).unwrap();
            (
                rel.to_string_lossy().into_owned(),
                target.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

#[cfg(unix)]
fn restore_all(plan: &RestorePlan, key: Option<&VaultKey>) -> restore::RestoreReport {
    restore::run_restore(plan, key, &CancelToken::default(), &mut quiet()).unwrap()
}

/// Links are backed up as links: only an entry in the index, never followed,
/// and restored with the target they named.
#[cfg(unix)]
#[test]
fn links_are_backed_up_and_restored_as_links() {
    use super::{manage, verify};
    use std::os::unix::fs::symlink;
    let (tmp, source, config) = setup();
    add_links(tmp.path(), &source);
    assert_eq!(link_targets(&source).len(), 5);

    let first = plan(&config, None);
    assert_eq!(first.summary.count(ItemKind::Skipped), 0);
    assert_eq!(first.copy_totals().0, 2, "nothing is copied for a link");
    let report = run(&first, None);
    assert_eq!(report.header.stats.files, 7);
    assert_eq!(report.header.stats.failed, 0);
    // In the backup folder a link is neither a link nor a file, and the
    // folder it names is not copied a second time.
    assert!(link_targets(&config.destination).is_empty());
    let stored = list_all_files(&report.snapshot_dir.join("Documents"));
    assert_eq!(stored.len(), 2, "{stored:?}");

    // Unchanged while a link names the same target.
    let second = plan(&config, None);
    assert_eq!(second.summary.count(ItemKind::Unchanged), 7);
    fs::remove_file(source.join("relative.lnk")).unwrap();
    symlink("photos/summer.jpg", source.join("relative.lnk")).unwrap();
    let third = plan(&config, None);
    assert_eq!(third.summary.count(ItemKind::Changed), 1);
    assert_eq!(third.summary.count(ItemKind::Unchanged), 6);
    let report = run(&third, None);
    assert_eq!(report.header.stats.files, 7);
    assert_eq!(report.header.stats.copied_files, 1);
    assert_eq!(report.header.stats.referenced_files, 4);
    assert_eq!(report.header.stats.linked_files, 2);

    let latest = snapshots::find(&config.destination, "latest", COMPUTER, None).unwrap();
    let check = verify::verify(&latest, None, true, &CancelToken::default(), &mut quiet()).unwrap();
    assert!(check.is_ok() && check.files == 7, "{check:?}");
    // Browsing lists what can be opened or copied.
    assert_eq!(snapshots::contents(&latest, None).unwrap().len(), 2);

    // Into an empty folder.
    let target = tmp.path().join("Restored");
    let restored = target.join("Documents");
    let into = || {
        restore_plan(
            &config.destination,
            RestoreTarget::Folder(target.clone()),
            None,
        )
    };
    let plan1 = into();
    assert_eq!(plan1.summary.count(ItemKind::New), 7);
    let done = restore_all(&plan1, None);
    assert_eq!((done.restored_files, done.failed), (7, 0), "{done:?}");
    assert_eq!(link_targets(&restored), link_targets(&source));
    assert_eq!(
        fs::read_to_string(restored.join("relative.lnk")).unwrap(),
        "not really a jpeg"
    );
    assert_eq!(list_all_files(&restored).len(), 2);

    // Again: nothing to do.
    let plan2 = into();
    assert_eq!(plan2.summary.count(ItemKind::Unchanged), 7);
    assert_eq!(plan2.write_totals(), (0, 0));

    // Onto a folder in use: another link, a file and a folder in the way.
    fs::remove_file(restored.join("relative.lnk")).unwrap();
    symlink("letter.txt", restored.join("relative.lnk")).unwrap();
    fs::remove_file(restored.join("absolute.lnk")).unwrap();
    write(&restored.join("absolute.lnk"), "a file in the way");
    fs::remove_file(restored.join("dangling.lnk")).unwrap();
    write(&restored.join("dangling.lnk/kept.txt"), "kept");

    let keep = plan::plan_restore(
        &latest,
        RestoreOptions {
            target: RestoreTarget::Folder(target.clone()),
            conflict: ConflictPolicy::KeepExisting,
            verify: true,
            skip: HashSet::new(),
        },
        None,
        &CancelToken::default(),
        &mut quiet(),
    )
    .unwrap();
    assert_eq!(keep.summary.count(ItemKind::Skipped), 3);
    assert_eq!(keep.write_totals().0, 0);

    let plan3 = into();
    assert_eq!(plan3.summary.count(ItemKind::Changed), 2);
    assert_eq!(plan3.summary.count(ItemKind::Skipped), 1);
    assert_eq!(plan3.summary.count(ItemKind::Unchanged), 4);
    let done = restore_all(&plan3, None);
    assert_eq!((done.restored_files, done.failed), (2, 0), "{done:?}");
    assert!(
        restored.join("dangling.lnk/kept.txt").is_file(),
        "a folder is never replaced"
    );
    let mut expected = link_targets(&source);
    expected.remove("dangling.lnk");
    assert_eq!(link_targets(&restored), expected);
    assert_eq!(
        fs::read_to_string(restored.join("letter.txt")).unwrap(),
        "Dear archive,",
        "the file the replaced link named is untouched"
    );

    // A moved backup keeps its links.
    let other = tmp.path().join("Other place");
    manage::transfer(&latest, &other, None, &CancelToken::default(), &mut quiet()).unwrap();
    let moved = snapshots::find(&other, "latest", COMPUTER, None).unwrap();
    let check = verify::verify(&moved, None, true, &CancelToken::default(), &mut quiet()).unwrap();
    assert!(check.is_ok() && check.files == 7, "{check:?}");
    assert!(link_targets(&other).is_empty());
}

/// In an encrypted backup, links and their targets are in the encrypted index
/// only.
#[cfg(unix)]
#[test]
fn links_in_an_encrypted_backup_stay_in_the_encrypted_index() {
    use super::{manage, verify};
    let (tmp, source, mut config) = setup();
    add_links(tmp.path(), &source);
    let key = test_vault(&config.destination, "secret words");
    config.encryption.enabled = true;

    let report = run(&plan(&config, Some(&key)), Some(&key));
    assert!(report.header.encrypted);
    assert_eq!(report.header.stats.files, 7);
    assert_eq!(report.header.stats.copied_files, 7);
    assert!(link_targets(&config.destination).is_empty());
    for file in list_all_files(&config.destination) {
        let name = file.to_string_lossy().into_owned();
        let content = String::from_utf8_lossy(&fs::read(&file).unwrap()).into_owned();
        for secret in ["dangling", "nowhere", "Elsewhere", ".lnk"] {
            assert!(!name.contains(secret), "{name}");
            assert!(!content.contains(secret), "{name} names {secret}");
        }
    }
    let blobs = config.destination.join(META_DIR).join("blobs");
    assert_eq!(list_all_files(&blobs).len(), 2, "no content for links");

    let second = plan(&config, Some(&key));
    assert_eq!(second.summary.count(ItemKind::Unchanged), 7);
    let report = run(&second, Some(&key));
    assert_eq!(report.header.stats.referenced_files, 7);

    let latest = snapshots::find(&config.destination, "latest", COMPUTER, Some(&key)).unwrap();
    let cancel = CancelToken::default();
    let check = verify::verify(&latest, Some(&key), true, &cancel, &mut quiet()).unwrap();
    assert!(check.is_ok() && check.files == 7, "{check:?}");

    let target = tmp.path().join("Restored");
    let restore = restore_plan(
        &config.destination,
        RestoreTarget::Folder(target.clone()),
        Some(&key),
    );
    let done = restore_all(&restore, Some(&key));
    assert_eq!((done.restored_files, done.failed), (7, 0), "{done:?}");
    assert_eq!(
        link_targets(&target.join("Documents")),
        link_targets(&source)
    );
    // Onto the same folder again: nothing to do.
    let again = restore_plan(
        &config.destination,
        RestoreTarget::Folder(target.clone()),
        Some(&key),
    );
    assert_eq!(again.summary.count(ItemKind::Unchanged), 7);

    // Moving and deleting leave link entries alone.
    let other = tmp.path().join("Other place");
    manage::transfer(&latest, &other, Some(&key), &cancel, &mut quiet()).unwrap();
    let moved = snapshots::find(&other, "latest", COMPUTER, Some(&key)).unwrap();
    let check = verify::verify(&moved, Some(&key), true, &cancel, &mut quiet()).unwrap();
    assert!(check.is_ok() && check.files == 7, "{check:?}");
    let left = snapshots::find(&config.destination, "latest", COMPUTER, Some(&key)).unwrap();
    let check = verify::verify(&left, Some(&key), true, &cancel, &mut quiet()).unwrap();
    assert!(check.is_ok() && check.files == 7, "{check:?}");
}

/// A link in the readable part of a partly encrypted backup is covered by the
/// checksum in the encrypted header: its target cannot be edited.
#[cfg(unix)]
#[test]
fn link_in_the_readable_part_cannot_be_redirected() {
    use super::manifest::{self, link_digest};
    let (tmp, source, mut config) = setup();
    add_links(tmp.path(), &source);
    let key = test_vault(&config.destination, "secret words");
    config.encryption.enabled = true;
    config.encryption.scope = crate::config::EncryptionScope::Selected;
    config.sources[0].encrypt_paths = vec!["photos".into()];

    let report = run(&plan(&config, Some(&key)), Some(&key));
    assert!(report.header.split);
    assert_eq!(report.header.stats.files, 5, "a file and four links");
    assert_eq!(report.encrypted_part.as_ref().unwrap().stats.files, 2);

    let target = tmp.path().join("Restored");
    let restore = restore_plan(
        &config.destination,
        RestoreTarget::Folder(target.clone()),
        Some(&key),
    );
    let done = restore_all(&restore, Some(&key));
    assert_eq!((done.restored_files, done.failed), (7, 0), "{done:?}");
    assert_eq!(
        link_targets(&target.join("Documents")),
        link_targets(&source)
    );

    let meta = report.snapshot_dir.join(META_DIR);
    let mut index = manifest::read_index(&meta).unwrap();
    let entry = index
        .files
        .iter_mut()
        .find(|f| f.path == "relative.lnk")
        .unwrap();
    entry.link = Some("/etc/passwd".into());
    entry.sha256 = link_digest("/etc/passwd");
    fs::write(meta.join("files.json"), serde_json::to_vec(&index).unwrap()).unwrap();

    let latest = snapshots::find(&config.destination, "latest", COMPUTER, Some(&key)).unwrap();
    let options = RestoreOptions {
        target: RestoreTarget::Folder(tmp.path().join("Second")),
        conflict: ConflictPolicy::ReplaceChanged,
        verify: true,
        skip: HashSet::new(),
    };
    let refused = plan::plan_restore(
        &latest,
        options,
        Some(&key),
        &CancelToken::default(),
        &mut quiet(),
    );
    assert!(refused.is_err(), "an edited link target is refused");
}

/// An edited index that names a link and a file below it: the file goes into
/// a real folder and the link is not made. An entry whose checksum does not
/// belong to its target is not restored either.
#[cfg(unix)]
#[test]
fn edited_index_cannot_write_through_a_link_it_names() {
    use super::manifest::{self, FileEntry, link_digest};
    use super::verify;
    let (tmp, _source, config) = setup();
    let report = run(&plan(&config, None), None);
    let outside = tmp.path().join("Outside");
    fs::create_dir_all(&outside).unwrap();
    let outside_text = outside.to_string_lossy().into_owned();

    let meta = report.snapshot_dir.join(META_DIR);
    let mut index = manifest::read_index(&meta).unwrap();
    let link = |path: &str, sha256: String| FileEntry {
        source: "Documents".into(),
        path: path.into(),
        size: 0,
        modified: 0,
        sha256,
        mode: None,
        blob: String::new(),
        link: Some(outside_text.clone()),
    };
    index
        .files
        .insert(0, link("photos", link_digest(&outside_text)));
    index
        .files
        .push(link("other.lnk", link_digest("something else")));
    fs::write(meta.join("files.json"), serde_json::to_vec(&index).unwrap()).unwrap();

    let target = tmp.path().join("Restored");
    let restore = restore_plan(
        &config.destination,
        RestoreTarget::Folder(target.clone()),
        None,
    );
    let done = restore_all(&restore, None);
    assert_eq!((done.restored_files, done.failed), (2, 2), "{done:?}");
    assert!(target.join("Documents/photos/summer.jpg").is_file());
    assert!(link_targets(&target).is_empty());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);

    let latest = snapshots::find(&config.destination, "latest", COMPUTER, None).unwrap();
    let check = verify::verify(&latest, None, true, &CancelToken::default(), &mut quiet()).unwrap();
    assert_eq!(check.damaged, vec!["Documents/other.lnk".to_string()]);
}

/// Pipes, sockets and device files have no content to back up, and opening a
/// pipe waits for a writer: they are listed as skipped, never read.
#[cfg(unix)]
#[test]
fn pipes_and_sockets_are_skipped_not_read() {
    let (_tmp, source, config) = setup();
    let pipe = source.join("photos/app.pipe");
    let name = std::ffi::CString::new(pipe.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let _socket = std::os::unix::net::UnixListener::bind(source.join("agent.sock")).unwrap();

    let plan = plan(&config, None);
    let stored: Vec<&str> = plan.stored_files().map(|i| i.rel.as_str()).collect();
    assert!(!stored.contains(&"photos/app.pipe"), "{stored:?}");
    assert!(!stored.contains(&"agent.sock"), "{stored:?}");
    let skipped: Vec<&str> = plan
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Skipped && i.note == Some(plan::Note::SpecialFile))
        .map(|i| i.rel.as_str())
        .collect();
    assert_eq!(skipped.len(), 2, "{skipped:?}");

    // The backup finishes (reading the pipe would wait forever) and is complete.
    let report = run(&plan, None);
    assert_eq!(
        report.header.status,
        super::manifest::SnapshotStatus::Complete
    );
    assert_eq!(report.header.stats.files, 2);
    assert_eq!(report.header.stats.skipped, 2);
}

/// A file whose stored copy is gone was not restored: that is a failure of the
/// restore, not something left out on purpose.
#[test]
fn missing_content_counts_as_failed_on_restore() {
    let (tmp, _source, config) = setup();
    let report = run(&plan(&config, None), None);
    fs::remove_file(report.snapshot_dir.join("Documents/letter.txt")).unwrap();

    let target = tmp.path().join("Restored");
    let restore_plan = restore_plan(
        &config.destination,
        RestoreTarget::Folder(target.clone()),
        None,
    );
    let result =
        restore::run_restore(&restore_plan, None, &CancelToken::default(), &mut quiet()).unwrap();
    assert_eq!(result.restored_files, 1);
    assert_eq!(result.failed, 1, "{:?}", result.warnings);
    assert!(result.warnings.iter().any(|w| w.contains("letter.txt")));
    assert!(!target.join("Documents/letter.txt").exists());
}

/// Backups of the same minute are numbered `-2`, `-3` …; digits in the
/// computer name must not change their order.
#[test]
fn backups_of_one_minute_are_listed_in_the_order_they_were_made() {
    let tmp = tempfile::tempdir().unwrap();
    let names = [
        "2026-11-11 13-14 (PC7)",
        "2026-11-11 13-15 (PC7)",
        "2026-11-11 13-15-2 (PC7)",
        "2026-11-11 13-15-10 (PC7)",
    ];
    for name in names {
        fs::create_dir_all(tmp.path().join(name).join(META_DIR)).unwrap();
    }
    let listed: Vec<String> = snapshots::list(tmp.path(), None)
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    let newest_first: Vec<&str> = names.iter().rev().copied().collect();
    assert_eq!(listed, newest_first);
}

fn split_config(config: &mut Config) -> VaultKey {
    let key = test_vault(&config.destination, "secret words");
    config.encryption.enabled = true;
    config.encryption.scope = crate::config::EncryptionScope::Selected;
    config.sources[0].encrypt_paths = vec!["photos".into()];
    key
}

fn extract_all(
    snapshot: &snapshots::SnapshotInfo,
    files: &[snapshots::StoredFile],
    target: &Path,
    key: Option<&VaultKey>,
) -> crate::error::EngineResult<super::manage::ExtractReport> {
    super::manage::extract(
        snapshot,
        files,
        target,
        key,
        &CancelToken::default(),
        &mut quiet(),
    )
}

/// Audit 2026-11-12 AV-01: browsing, opening and extracting check the parts
/// of a backup like a restore does; a readable part planted next to an
/// encrypted backup is never listed or copied out.
#[test]
fn planted_readable_part_is_not_browsed_or_extracted() {
    let (tmp, _source, mut config) = setup();
    let key = test_vault(&config.destination, "secret words");
    config.encryption.enabled = true;
    let report = run(&plan(&config, Some(&key)), Some(&key));
    plant_readable_part(tmp.path(), &report);

    let latest = snapshots::find(&config.destination, "latest", COMPUTER, Some(&key)).unwrap();
    let listed = snapshots::contents(&latest, Some(&key));
    assert!(
        matches!(listed, Err(crate::error::EngineError::Manifest { .. })),
        "{listed:?}"
    );

    // A list of files made by someone else is not trusted either.
    let planted = snapshots::StoredFile {
        source: "Evil".into(),
        path: "planted.txt".into(),
        size: 7,
        modified: 0,
        sha256: super::format_sha256(&<sha2::Sha256 as sha2::Digest>::digest(b"planted")),
        blob: format!("{}/Evil/planted.txt", report.header.id),
        encrypted: false,
    };
    let out = tmp.path().join("Extracted");
    assert!(extract_all(&latest, &[planted], &out, Some(&key)).is_err());
    assert!(list_all_files(&out).is_empty(), "nothing extracted");
    assert!(
        super::verify::verify(
            &latest,
            Some(&key),
            true,
            &CancelToken::default(),
            &mut quiet()
        )
        .is_err()
    );
}

/// Extracting copies only files that the backup, read again, contains.
#[test]
fn extract_refuses_files_the_backup_does_not_contain() {
    let (tmp, _source, config) = setup();
    let report = run(&plan(&config, None), None);
    let latest = snapshots::find(&config.destination, "latest", COMPUTER, None).unwrap();
    let files = snapshots::contents(&latest, None).unwrap();
    let letter = files.iter().find(|f| f.path == "letter.txt").unwrap();

    // Same stored copy, but pretending to be another file.
    let mut forged = letter.clone();
    forged.path = "startup.bat".into();
    let out = tmp.path().join("Extracted");
    let result = extract_all(&latest, &[letter.clone(), forged], &out, None).unwrap();
    assert_eq!(result.files, 1);
    assert_eq!(result.failed.len(), 1, "{:?}", result.failed);
    assert!(result.failed[0].contains("not part of this backup"));
    assert!(out.join("Documents/letter.txt").is_file());
    assert!(!out.join("Documents/startup.bat").exists());
    let _ = report;
}

/// Audit 2026-11-12 AV-03: a partly encrypted backup with one of its parts
/// gone is refused, never restored or checked as if it were complete.
#[test]
fn partly_encrypted_backup_with_a_missing_part_is_refused() {
    use crate::error::EngineError;
    for missing in ["encrypted.avs", "snapshot.json"] {
        let (tmp, source, mut config) = setup();
        let key = split_config(&mut config);
        let report = run(&plan(&config, Some(&key)), Some(&key));
        assert!(report.header.split);
        fs::remove_file(report.snapshot_dir.join(META_DIR).join(missing)).unwrap();

        let latest = snapshots::find(&config.destination, "latest", COMPUTER, Some(&key)).unwrap();
        assert!(!latest.is_split(), "only one part is left");
        let options = RestoreOptions {
            target: RestoreTarget::Folder(tmp.path().join("Restored")),
            conflict: ConflictPolicy::ReplaceChanged,
            verify: true,
            skip: HashSet::new(),
        };
        let refused = plan::plan_restore(
            &latest,
            options,
            Some(&key),
            &CancelToken::default(),
            &mut quiet(),
        );
        assert!(
            matches!(&refused, Err(EngineError::Manifest { message, .. }) if message.contains("is missing")),
            "{missing}: {refused:?}"
        );
        let check = super::verify::verify(
            &latest,
            Some(&key),
            true,
            &CancelToken::default(),
            &mut quiet(),
        );
        assert!(check.is_err(), "{missing}: verify reports it");
        assert!(snapshots::contents(&latest, Some(&key)).is_err());
        assert!(!tmp.path().join("Restored").exists());
        let _ = source;
    }
}

/// Audit 2026-11-12 AV-04: an index entry naming a folder the backup does
/// not have is reported as failed, not left out silently.
#[test]
fn index_entry_naming_an_unknown_folder_fails_the_restore() {
    let (tmp, _source, config) = setup();
    let report = run(&plan(&config, None), None);
    let index_file = report.snapshot_dir.join(META_DIR).join("files.json");
    let mut index: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&index_file).unwrap()).unwrap();
    for file in index["files"].as_array_mut().unwrap() {
        if file["path"] == "letter.txt" {
            file["source"] = serde_json::json!("Nowhere");
        }
    }
    fs::write(&index_file, serde_json::to_string(&index).unwrap()).unwrap();

    let target = tmp.path().join("Restored");
    let plan = restore_plan(
        &config.destination,
        RestoreTarget::Folder(target.clone()),
        None,
    );
    let result = restore::run_restore(&plan, None, &CancelToken::default(), &mut quiet()).unwrap();
    assert_eq!(result.restored_files, 1);
    assert_eq!(result.failed, 1, "{:?}", result.warnings);
    assert!(
        result
            .warnings
            .iter()
            .any(|w| w.contains("Nowhere/letter.txt"))
    );
}

/// Audit 2026-11-12 AV-02: a folder that is not there (network share not
/// connected) makes the backup incomplete; it is not named in the new
/// backup, so retention keeps the last backup that has its files.
#[test]
fn missing_folder_fails_the_backup_and_its_last_copy_stays() {
    use super::manifest::SnapshotStatus;
    let (tmp, source, mut config) = setup();
    let share = tmp.path().join("Share");
    write(&share.join("orders.db"), "orders");
    config.sources.push(Source::new("Share", &share, true));
    config.retention = crate::config::Retention {
        enabled: true,
        keep_last: 1,
        keep_daily: 0,
        keep_weekly: 0,
        keep_monthly: 0,
    };
    let first = run(&plan(&config, None), None);
    assert_eq!(first.header.status, SnapshotStatus::Complete);

    fs::rename(&share, tmp.path().join("Share (disconnected)")).unwrap();
    let second = run(&plan(&config, None), None);
    assert_eq!(second.header.status, SnapshotStatus::CompleteWithWarnings);
    assert_eq!(second.header.stats.failed, 1);
    assert!(
        second
            .warnings
            .iter()
            .any(|w| w.contains("Share") && w.contains("not connected")),
        "{:?}",
        second.warnings
    );
    assert_eq!(second.unread_folders, vec![share.display().to_string()]);
    assert!(
        second.header.sources.iter().all(|s| s.path != share),
        "the folder that was not read is not named"
    );

    let all = snapshots::list(&config.destination, None).unwrap();
    let doomed = super::retention::to_remove(&all, &config.retention, COMPUTER);
    assert!(
        doomed.is_empty(),
        "the only backup with the share stays: {doomed:?}"
    );
    let _ = source;
}

/// Audit 2026-11-12 AV-02: when no folder can be read, no (empty) backup is
/// made at all.
#[test]
fn no_readable_folder_makes_no_backup() {
    let (tmp, source, config) = setup();
    run(&plan(&config, None), None);
    fs::rename(&source, tmp.path().join("gone")).unwrap();
    let sources = Sources::collect(&config, &KnownPaths::current());
    let input = BackupInput {
        config: &config,
        sources: &sources,
        computer: COMPUTER,
        key: None,
    };
    let result = plan::plan_backup(&input, &CancelToken::default(), &mut quiet());
    assert!(
        matches!(
            result,
            Err(crate::error::EngineError::SourcesUnavailable(_))
        ),
        "{:?}",
        result.err()
    );
    assert_eq!(snapshots::list(&config.destination, None).unwrap().len(), 1);
}

/// A sub-folder without read access is a failure of the backup, not a skip.
#[cfg(unix)]
#[test]
fn unreadable_folder_counts_as_failed() {
    use super::manifest::SnapshotStatus;
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, source, config) = setup();
    let locked = source.join("locked");
    write(&locked.join("a.txt"), "a");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&locked).is_ok() {
        // Running as root: permissions are not enforced.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let report = run(&plan(&config, None), None);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(report.header.status, SnapshotStatus::CompleteWithWarnings);
    assert_eq!(report.header.stats.failed, 1);
    assert!(
        report.warnings.iter().any(|w| w.contains("locked")),
        "{:?}",
        report.warnings
    );
}

/// A stored copy that cannot be read is reported with the system's error,
/// apart from copies whose content is wrong.
#[cfg(unix)]
#[test]
fn check_tells_unreadable_copies_from_damaged_ones() {
    use std::os::unix::fs::PermissionsExt;
    let (_tmp, _source, config) = setup();
    let report = run(&plan(&config, None), None);
    let stored = report.snapshot_dir.join("Documents/letter.txt");
    fs::set_permissions(&stored, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&stored).is_ok() {
        return; // root
    }
    let latest = snapshots::find(&config.destination, "latest", COMPUTER, None).unwrap();
    let check =
        super::verify::verify(&latest, None, true, &CancelToken::default(), &mut quiet()).unwrap();
    fs::set_permissions(&stored, fs::Permissions::from_mode(0o444)).unwrap();
    assert!(!check.is_ok());
    assert!(check.damaged.is_empty(), "{:?}", check.damaged);
    assert_eq!(check.unreadable.len(), 1);
    assert!(check.unreadable[0].starts_with("Documents/letter.txt: "));
    assert!(check.unreadable[0].to_lowercase().contains("permission"));
}
