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
