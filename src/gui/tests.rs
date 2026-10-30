//! Interface tests without a visible window (egui_kittest + AccessKit):
//! buttons are found by their accessible label and clicked like a user would.

use std::path::{Path, PathBuf};
use std::time::Duration;

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::{AeternaApp, Done, Screen, VaultDialog, View};
use crate::config::{Config, LanguageSetting, Loaded, Source};
use crate::logging::LogBuffer;
use crate::paths::AppPaths;

fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn harness(tmp: &Path, configure: impl FnOnce(&mut Config)) -> Harness<'static, AeternaApp> {
    let docs = tmp.join("Docs");
    write(&docs.join("letter.txt"), "Dear archive,");
    write(&docs.join("photos/summer.jpg"), "not a jpeg");

    let mut config = Config {
        language: LanguageSetting::En,
        destination: tmp.join("Vault"),
        sources: vec![Source::new("Docs", &docs, true)],
        ..Config::default()
    };
    configure(&mut config);

    let paths = AppPaths {
        config_file: tmp.join("home").join("config.toml"),
        log_dir: tmp.join("home").join("logs"),
        portable: false,
    };
    let loaded = Loaded {
        config,
        notice: None,
    };

    let mut harness = Harness::builder()
        .with_size([1000.0, 900.0])
        .with_max_steps(20)
        .build_eframe(move |cc| {
            super::theme::install_fonts(&cc.egui_ctx, &crate::config::Fonts::default());
            super::theme::apply(&cc.egui_ctx, crate::config::Appearance::Dark);
            AeternaApp::new(&cc.egui_ctx, paths, loaded, LogBuffer::default())
        });
    harness.step();
    harness.step();
    harness
}

/// Switches the view like a click on its tab (loads what the view needs).
fn goto(harness: &mut Harness<'static, AeternaApp>, view: View) {
    let ctx = harness.ctx.clone();
    harness.state_mut().navigate(&ctx, view);
}

/// Runs frames until `condition` holds (background work happens on threads).
fn wait_until(
    harness: &mut Harness<'static, AeternaApp>,
    what: &str,
    condition: impl Fn(&AeternaApp) -> bool,
) {
    for _ in 0..1500 {
        harness.step();
        if condition(harness.state()) {
            harness.step();
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

fn click(harness: &mut Harness<'static, AeternaApp>, label: &str) {
    harness.get_by_label(label).click();
    harness.step();
    harness.step();
}

#[test]
fn folder_tree_backup_and_restore_through_the_interface() {
    let tmp = tempfile::tempdir().unwrap();
    let mut harness = harness(tmp.path(), |_| {});

    // Open the contents tree and leave out the photos folder.
    click(&mut harness, "Choose what to keep from this folder: Docs");
    wait_until(&mut harness, "tree entries", |app| {
        !app.tree.listings.is_empty()
    });
    click(&mut harness, "photos");
    assert_eq!(
        harness.state().config.sources[0].exclude_paths,
        vec!["photos".to_string()]
    );

    // Back up: plan, confirm, run.
    click(&mut harness, "Back up now");
    wait_until(&mut harness, "confirmation", |app| app.pending.is_some());
    click(&mut harness, "Start");
    wait_until(&mut harness, "backup result", |app| {
        matches!(app.screen, Screen::Done(_))
    });
    match &harness.state().screen {
        Screen::Done(done) => match done.as_ref() {
            Done::Backup(Ok(report)) => {
                assert_eq!(report.header.stats.files, 1, "photos were deselected");
                assert!(
                    report
                        .snapshot_dir
                        .join("Docs")
                        .join("letter.txt")
                        .is_file()
                );
            }
            _ => panic!("backup did not succeed"),
        },
        _ => unreachable!(),
    }
    click(&mut harness, "Back to overview");

    // Restore into a separate folder.
    let target = tmp.path().join("Restored");
    {
        let app = harness.state_mut();
        app.view = View::Restore;
        app.restore.to_folder = true;
        app.restore.folder = Some(target.clone());
    }
    let ctx = harness.ctx.clone();
    harness.state_mut().refresh_snapshots(&ctx);
    wait_until(&mut harness, "backup list", |app| {
        app.restore.selected.is_some()
    });
    // "Restore" is both a navigation tab and the action button; the button comes last.
    harness
        .get_all_by_label("Restore")
        .last()
        .expect("restore button")
        .click();
    harness.step();
    wait_until(&mut harness, "restore confirmation or result", |app| {
        app.pending.is_some() || matches!(app.screen, Screen::Done(_))
    });
    if harness.state().pending.is_some() {
        click(&mut harness, "Start");
    }
    wait_until(&mut harness, "restore result", |app| {
        matches!(app.screen, Screen::Done(_))
    });
    assert_eq!(
        std::fs::read_to_string(target.join("Docs").join("letter.txt")).unwrap(),
        "Dear archive,"
    );
    assert!(!target.join("Docs").join("photos").exists());

    // Settings were saved.
    assert!(tmp.path().join("home").join("config.toml").is_file());
}

#[test]
fn set_up_encryption_and_back_up_encrypted() {
    let tmp = tempfile::tempdir().unwrap();
    let mut harness = harness(tmp.path(), |_| {});

    harness
        .get_by_role_and_label(eframe::egui::accesskit::Role::CheckBox, "Encrypt backups")
        .click();
    harness.step();
    harness.step();
    assert!(matches!(
        harness.state().vault.dialog,
        Some(VaultDialog::Create { .. })
    ));

    // Empty first: the dialog explains and stays open. (Any other passphrase
    // is accepted; its rating is only a hint.)
    if let Some(VaultDialog::Create {
        passphrase, repeat, ..
    }) = &mut harness.state_mut().vault.dialog
    {
        passphrase.clear();
        repeat.clear();
    }
    click(&mut harness, "Set up");
    assert!(matches!(
        harness.state().vault.dialog,
        Some(VaultDialog::Create { error: Some(_), .. })
    ));

    if let Some(VaultDialog::Create {
        passphrase, repeat, ..
    }) = &mut harness.state_mut().vault.dialog
    {
        *passphrase = "quiet archive of many summers".into();
        *repeat = "quiet archive of many summers".into();
    }
    click(&mut harness, "Set up");
    wait_until(&mut harness, "recovery key", |app| {
        matches!(app.vault.dialog, Some(VaultDialog::ShowRecovery { .. }))
    });
    click(&mut harness, "I have kept the recovery key in a safe place");
    click(&mut harness, "Done");
    assert!(harness.state().vault.dialog.is_none());
    assert!(harness.state().config.encryption.enabled);
    assert!(crate::engine::vault::exists(&tmp.path().join("Vault")));

    click(&mut harness, "Back up now");
    wait_until(&mut harness, "confirmation", |app| app.pending.is_some());
    click(&mut harness, "Start");
    wait_until(&mut harness, "backup result", |app| {
        matches!(app.screen, Screen::Done(_))
    });
    match &harness.state().screen {
        Screen::Done(done) => match done.as_ref() {
            Done::Backup(Ok(report)) => {
                assert!(report.header.encrypted);
                assert_eq!(report.header.stats.files, 2);
            }
            Done::Backup(Err(message)) => panic!("backup failed: {message}"),
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }

    // No readable file names in the vault.
    let names: Vec<PathBuf> = walkdir::WalkDir::new(tmp.path().join("Vault"))
        .into_iter()
        .flatten()
        .map(|e| e.into_path())
        .collect();
    assert!(
        names
            .iter()
            .all(|p| !p.display().to_string().contains("letter"))
    );
    click(&mut harness, "Back to overview");

    // Forget and lock: the backup is listed, but locked.
    let ctx = harness.ctx.clone();
    {
        let app = harness.state_mut();
        app.set_remembered(false);
        app.lock_vault(&ctx);
        app.view = View::Restore;
        app.restore.to_folder = true;
        app.restore.folder = Some(tmp.path().join("Restored"));
    }
    wait_until(&mut harness, "locked backup", |app| {
        app.selected_snapshot().is_some_and(|s| s.is_locked())
    });

    click(&mut harness, "Unlock…");
    assert!(matches!(
        harness.state().vault.dialog,
        Some(VaultDialog::Unlock { .. })
    ));
    if let Some(VaultDialog::Unlock {
        secret, remember, ..
    }) = &mut harness.state_mut().vault.dialog
    {
        *secret = "wrong words".into();
        *remember = false;
    }
    click(&mut harness, "Unlock");
    assert!(matches!(
        harness.state().vault.dialog,
        Some(VaultDialog::Unlock { error: Some(_), .. })
    ));
    if let Some(VaultDialog::Unlock { secret, .. }) = &mut harness.state_mut().vault.dialog {
        *secret = "quiet archive of many summers".into();
    }
    click(&mut harness, "Unlock");
    wait_until(&mut harness, "unlocked backup", |app| {
        app.vault.dialog.is_none() && app.selected_snapshot().is_some_and(|s| s.header.is_some())
    });

    harness
        .get_all_by_label("Restore")
        .last()
        .expect("restore button")
        .click();
    harness.step();
    wait_until(&mut harness, "restore confirmation or result", |app| {
        app.pending.is_some() || matches!(app.screen, Screen::Done(_))
    });
    if harness.state().pending.is_some() {
        click(&mut harness, "Start");
    }
    wait_until(&mut harness, "restore result", |app| {
        matches!(app.screen, Screen::Done(_))
    });
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("Restored").join("Docs").join("letter.txt"))
            .unwrap(),
        "Dear archive,"
    );
}

#[test]
fn check_browse_and_delete_a_backup_in_the_backups_view() {
    let tmp = tempfile::tempdir().unwrap();
    let mut harness = harness(tmp.path(), |config| {
        config.advanced.confirm_before_start = false;
    });
    click(&mut harness, "Back up now");
    wait_until(&mut harness, "backup result", |app| {
        matches!(app.screen, Screen::Done(_))
    });
    click(&mut harness, "Back to overview");

    let ctx = harness.ctx.clone();
    harness.state_mut().view = View::Backups;
    harness.state_mut().refresh_snapshots(&ctx);
    wait_until(&mut harness, "backup list", |app| {
        !app.all_snapshots().is_empty()
    });
    let id = harness.state().all_snapshots()[0].qualified_id();
    harness.state_mut().manage.checked.insert(id);
    harness.step();

    // Check it.
    click(&mut harness, "Check");
    wait_until(&mut harness, "check result", |app| {
        matches!(app.screen, Screen::Done(_))
    });
    match &harness.state().screen {
        Screen::Done(done) => match done.as_ref() {
            Done::Verify(Ok(report)) => assert!(report.is_ok() && report.files == 2),
            _ => panic!("check did not succeed"),
        },
        _ => unreachable!(),
    }
    click(&mut harness, "Back to overview");

    // Browse it.
    click(&mut harness, "Browse…");
    wait_until(&mut harness, "contents", |app| {
        matches!(app.screen, Screen::Browse(_))
    });
    // Panics if the file is not listed.
    let _ = harness.get_by_label("Docs/letter.txt");
    click(&mut harness, "Back");

    // Delete it.
    click(&mut harness, "Delete…");
    assert!(harness.state().pending.is_some());
    harness
        .get_all_by_label("Delete…")
        .last()
        .expect("confirm button")
        .click();
    harness.step();
    wait_until(&mut harness, "delete result", |app| {
        matches!(app.screen, Screen::Done(_))
    });
    wait_until(&mut harness, "empty list", |app| {
        app.all_snapshots().is_empty()
    });
}

fn demo_schedules(config: &Config) -> Vec<crate::config::Schedule> {
    use crate::config::{Frequency, Schedule};
    let mut documents = Schedule {
        id: "demo2".into(),
        frequency: Frequency::Hourly,
        every_hours: 2,
        time: "08:00".into(),
        all_folders: false,
        ..Schedule::default()
    };
    documents.folders = config
        .sources
        .iter()
        .filter(|s| s.enabled)
        .take(1)
        .map(|s| s.path.clone())
        .collect();
    vec![
        Schedule {
            id: "demo1".into(),
            ..Schedule::default()
        },
        documents,
    ]
}

/// Renders the README screenshots from a prepared demo configuration.
///
/// ```text
/// $env:AETERNAVAULT_HOME = '<folder with a demo config.toml>'
/// $env:AETERNAVAULT_PROFILE_ROOT = '<fake user profile>'
/// $env:AETERNAVAULT_SCREENSHOTS = 'docs\screenshots'
/// cargo test render_readme_screenshots -- --ignored
/// ```
#[test]
#[ignore = "needs demo data; run manually to refresh docs/screenshots"]
fn render_readme_screenshots() {
    let Some(out) = std::env::var_os("AETERNAVAULT_SCREENSHOTS").map(PathBuf::from) else {
        panic!("set AETERNAVAULT_SCREENSHOTS, AETERNAVAULT_HOME and AETERNAVAULT_PROFILE_ROOT");
    };
    std::fs::create_dir_all(&out).unwrap();
    let paths = AppPaths::resolve();

    let build = |height: f32, configure: &dyn Fn(&mut Config)| {
        let mut loaded = crate::config::load_or_create(&paths);
        configure(&mut loaded.config);
        let appearance = loaded.config.appearance;
        let paths = paths.clone();
        let theme = match appearance {
            crate::config::Appearance::Light => eframe::egui::Theme::Light,
            _ => eframe::egui::Theme::Dark,
        };
        let mut harness = Harness::builder()
            .with_size([1000.0, height])
            .with_max_steps(20)
            .with_theme(theme)
            .wgpu()
            .build_eframe(move |cc| {
                super::theme::install_fonts(&cc.egui_ctx, &crate::config::Fonts::default());
                super::theme::apply(&cc.egui_ctx, appearance);
                // No background service or tray icon while taking pictures.
                AeternaApp::new(&cc.egui_ctx, paths, loaded, LogBuffer::default())
            });
        harness.step();
        harness
    };
    let settle = |harness: &mut Harness<'static, AeternaApp>| {
        wait_until(harness, "background loaders", |app| {
            app.snapshots.is_some()
                && (app.view != View::Apps || app.apps.detected.is_some())
                && app.sizes.len() >= app.config.sources.len()
        });
        for _ in 0..40 {
            harness.step();
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let save = |harness: &mut Harness<'static, AeternaApp>, name: &str| {
        // Move the pointer out of the way so no hover state is photographed.
        harness.event(eframe::egui::Event::PointerGone);
        for _ in 0..10 {
            harness.step();
        }
        let image = harness.render().expect("rendering works");
        image.save(out.join(format!("{name}.png"))).unwrap();
    };

    // 1. Backup view, full height, with the Desktop contents tree open.
    let mut h = build(1300.0, &|config| {
        config.language = LanguageSetting::En;
        config.appearance = crate::config::Appearance::Dark;
        if config.schedules.is_empty() {
            config.schedules = demo_schedules(config);
        }
    });
    settle(&mut h);
    click(&mut h, "Choose what to keep from this folder: Desktop");
    settle(&mut h);
    save(&mut h, "backup-view");

    // 2. The same in the light and the black (OLED) appearance.
    for (appearance, name) in [
        (crate::config::Appearance::Light, "backup-view-light"),
        (crate::config::Appearance::Black, "backup-view-black"),
    ] {
        let mut h = build(900.0, &|config| {
            config.language = LanguageSetting::En;
            config.appearance = appearance;
        });
        settle(&mut h);
        save(&mut h, name);
    }

    // 3. Applications.
    let mut h = build(1000.0, &|config| config.language = LanguageSetting::En);
    goto(&mut h, View::Apps);
    settle(&mut h);
    save(&mut h, "applications");

    // 4. Restore with the choice of parts.
    let mut h = build(1000.0, &|config| config.language = LanguageSetting::En);
    goto(&mut h, View::Restore);
    settle(&mut h);
    save(&mut h, "restore");

    // 4b. Backups view with one backup ticked.
    let mut h = build(1000.0, &|config| config.language = LanguageSetting::En);
    goto(&mut h, View::Backups);
    settle(&mut h);
    if let Some(first) = h.state().all_snapshots().first().map(|s| s.qualified_id()) {
        h.state_mut().manage.checked.insert(first);
    }
    settle(&mut h);
    save(&mut h, "backups");

    // Checks that are not part of the README (prefixed "check-").
    let mut h = build(1400.0, &|config| config.language = LanguageSetting::De);
    goto(&mut h, View::Settings);
    settle(&mut h);
    save(&mut h, "check-settings");

    let mut h = build(1500.0, &|config| config.language = LanguageSetting::De);
    goto(&mut h, View::Notifications);
    settle(&mut h);
    save(&mut h, "check-notifications");

    let mut h = build(800.0, &|config| config.language = LanguageSetting::De);
    settle(&mut h);
    h.state_mut().vault.dialog = Some(VaultDialog::ShowRecovery {
        key: "K7QM-2HXA-9WTC-4RBN-P8ZE-6JDF-3VYS-1MQK".into(),
        confirmed: false,
        copied_at: None,
    });
    settle(&mut h);
    save(&mut h, "check-recovery-dialog");

    let mut h = build(900.0, &|config| config.language = LanguageSetting::De);
    settle(&mut h);
    h.state_mut().new_schedule();
    settle(&mut h);
    save(&mut h, "check-schedule-dialog");

    // 4c. Backup jobs.
    let mut h = build(900.0, &|config| {
        config.language = LanguageSetting::En;
        if config.schedules.is_empty() {
            config.schedules = demo_schedules(config);
        }
    });
    goto(&mut h, View::Jobs);
    settle(&mut h);
    save(&mut h, "jobs");

    // 4c'. Retention forecast and the activity history.
    let mut h = build(1400.0, &|config| {
        config.language = LanguageSetting::En;
        config.retention.enabled = true;
        config.schedules = demo_schedules(config);
    });
    goto(&mut h, View::Backups);
    settle(&mut h);
    save(&mut h, "check-retention-forecast");

    let mut h = build(900.0, &|config| config.language = LanguageSetting::De);
    {
        use crate::history::{Entry, Event, Outcome};
        let at = |days: i64, hours: i64| {
            chrono::Utc::now() - chrono::Duration::days(days) - chrono::Duration::hours(hours)
        };
        h.state_mut().history = vec![
            Entry {
                at: at(2, 3),
                event: Event::JobCreated {
                    job: "Jeden Tag um 20:00 · Alles".into(),
                },
            },
            Entry {
                at: at(1, 5),
                event: Event::Backup {
                    job: "Jeden Tag um 20:00 · Alles".into(),
                    command_line: false,
                    outcome: Outcome::Complete,
                    files: 1284,
                    bytes: 3_400_000_000,
                    snapshot: "2026-09-15 20-00".into(),
                    message: String::new(),
                },
            },
            Entry {
                at: at(0, 4),
                event: Event::Backup {
                    job: "Jeden Tag um 20:00 · Alles".into(),
                    command_line: false,
                    outcome: Outcome::Skipped,
                    files: 0,
                    bytes: 0,
                    snapshot: String::new(),
                    message: String::new(),
                },
            },
            Entry {
                at: at(0, 2),
                event: Event::BackupsDeleted {
                    snapshots: vec!["2026-08-01 20-00".into(), "2026-08-02 20-00".into()],
                    by_rules: true,
                    message: String::new(),
                },
            },
            Entry {
                at: at(0, 1),
                event: Event::Verified {
                    snapshot: "2026-09-15 20-00".into(),
                    files: 1284,
                    damaged: 0,
                    missing: 0,
                },
            },
        ];
    }
    goto(&mut h, View::Activity);
    settle(&mut h);
    save(&mut h, "check-activity");

    // 4d. Settings.
    let mut h = build(2200.0, &|config| config.language = LanguageSetting::En);
    goto(&mut h, View::Settings);
    settle(&mut h);
    save(&mut h, "check-settings");

    // 5. Encryption set-up dialog.
    let mut h = build(800.0, &|config| config.language = LanguageSetting::En);
    settle(&mut h);
    h.state_mut().vault.dialog = Some(VaultDialog::Create {
        passphrase: "quiet archive of many summers".into(),
        repeat: "quiet archive of many summers".into(),
        remember: true,
        options: crate::engine::vault::VaultOptions::default(),
        error: None,
    });
    settle(&mut h);
    save(&mut h, "encryption");

    // 6. German, light appearance.
    let mut h = build(800.0, &|config| {
        config.language = LanguageSetting::De;
        config.appearance = crate::config::Appearance::Light;
    });
    settle(&mut h);
    save(&mut h, "overview-de-light");
}

#[test]
fn a_deleted_destination_is_created_again_and_the_history_remembers() {
    let tmp = tempfile::tempdir().unwrap();
    let destination = tmp.path().join("Vault");
    let mut harness = harness(tmp.path(), |_| {});

    let back_up = |harness: &mut Harness<'static, AeternaApp>| {
        click(harness, "Back up now");
        wait_until(harness, "confirmation", |app| app.pending.is_some());
        click(harness, "Start");
        wait_until(harness, "backup result", |app| {
            matches!(app.screen, Screen::Done(_))
        });
        match &harness.state().screen {
            Screen::Done(done) => {
                assert!(
                    matches!(done.as_ref(), Done::Backup(Ok(_))),
                    "backup failed"
                )
            }
            _ => unreachable!(),
        }
    };

    back_up(&mut harness);
    click(&mut harness, "Back to overview");
    std::fs::remove_dir_all(&destination).unwrap();
    back_up(&mut harness);
    assert!(
        destination.is_dir(),
        "the destination folder is created again"
    );

    // The done screen offers to turn this into a backup job.
    click(&mut harness, "Repeat automatically…");
    assert_eq!(harness.state().view, View::Jobs);
    assert!(harness.state().schedule.editor.is_some());
    click(&mut harness, "Save");
    assert_eq!(harness.state().config.schedules.len(), 1);

    // Everything is in the history, also for the next start.
    let history = crate::history::load(&harness.state().paths.config_file);
    let backups = history
        .iter()
        .filter(|e| matches!(e.event, crate::history::Event::Backup { .. }))
        .count();
    assert_eq!(backups, 2);
    assert!(
        history
            .iter()
            .any(|e| matches!(e.event, crate::history::Event::JobCreated { .. }))
    );
    assert_eq!(harness.state().history.len(), history.len());
}

#[test]
fn a_missing_encrypted_vault_asks_to_set_up_encryption_again() {
    let tmp = tempfile::tempdir().unwrap();
    let mut harness = harness(tmp.path(), |_| {});
    let options = crate::engine::vault::VaultOptions {
        kdf: crate::engine::crypto::KdfParams::TEST,
        ..Default::default()
    };
    harness
        .state_mut()
        .create_vault("123", false, options)
        .expect("a short passphrase is allowed");
    assert!(harness.state().config.encryption.enabled);

    std::fs::remove_dir_all(tmp.path().join("Vault")).unwrap();
    click(&mut harness, "Back up now");
    assert!(matches!(
        harness.state().vault.dialog,
        Some(VaultDialog::Create { .. })
    ));
    assert!(harness.state().task.is_none());
}

#[test]
fn notices_disappear_on_their_own() {
    let tmp = tempfile::tempdir().unwrap();
    let mut harness = harness(tmp.path(), |_| {});
    harness
        .state_mut()
        .notify(super::widgets::NoticeKind::Info, "Short message");
    harness.step();
    assert_eq!(harness.state().notices.len(), 1);
    harness.state_mut().notices[0].until = std::time::Instant::now();
    harness.step();
    assert!(harness.state().notices.is_empty());
}
