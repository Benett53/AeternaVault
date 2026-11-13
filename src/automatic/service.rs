//! The background thread that runs due schedules while AeternaVault is open
//! or waiting in the notification area.
//!
//! It works independently of the window: egui does not draw while the window
//! is hidden, so nothing here may depend on frames. The window talks to the
//! service through [`Service`] (configuration, unlocked key, "run now") and
//! learns about progress through a callback.

use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};

use super::timing;
use crate::config::{Config, Schedule};
use crate::engine::crypto::VaultKey;
use crate::engine::{CancelToken, Progress};
use crate::i18n::Lang;
use crate::paths::AppPaths;
use crate::platform;
use crate::state::{AutomaticRun, State};

/// How often the thread looks at the clock when nothing wakes it earlier.
const TICK: Duration = Duration::from_secs(20);
/// How often the service looks for jobs without a recent successful backup.
const OVERDUE_CHECK: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone)]
pub enum Event {
    Started,
    Progress,
    Finished(Box<AutomaticRun>),
}

#[derive(Debug, Clone)]
pub struct Running {
    pub label: String,
    pub progress: Progress,
    pub cancel: CancelToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Request {
    Schedule(String),
    Everything,
}

type Notify = Box<dyn Fn(Event) + Send + Sync>;

struct Shared {
    paths: AppPaths,
    config: Mutex<Config>,
    key: Mutex<Option<VaultKey>>,
    requests: Mutex<Vec<Request>>,
    running: Mutex<Option<Running>>,
    /// A backup or restore started from the window: automatic runs wait.
    window_busy: AtomicBool,
    stop: AtomicBool,
    wake: (Mutex<bool>, Condvar),
    notify: Notify,
    process_start: DateTime<Local>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Service {
    shared: Arc<Shared>,
}

impl Service {
    pub fn start(paths: AppPaths, config: Config, notify: Notify) -> Self {
        let shared = Arc::new(Shared {
            paths,
            config: Mutex::new(config),
            key: Mutex::new(None),
            requests: Mutex::new(Vec::new()),
            running: Mutex::new(None),
            window_busy: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            wake: (Mutex::new(false), Condvar::new()),
            notify,
            process_start: Local::now(),
        });
        let worker = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("aeterna-automatic".into())
            .spawn(move || worker_loop(&worker));
        if let Err(err) = spawned {
            tracing::error!("automatic backups could not be started: {err}");
        }
        Self { shared }
    }

    fn wake(&self) {
        let (flag, condvar) = &self.shared.wake;
        *lock(flag) = true;
        condvar.notify_all();
    }

    pub fn set_config(&self, config: &Config) {
        *lock(&self.shared.config) = config.clone();
        self.wake();
    }

    /// The key of an unlocked vault, so encrypted automatic backups also work
    /// while the key is not remembered on this computer.
    pub fn set_key(&self, key: Option<VaultKey>) {
        *lock(&self.shared.key) = key;
    }

    pub fn set_window_busy(&self, busy: bool) {
        if self.shared.window_busy.swap(busy, Ordering::Relaxed) && !busy {
            self.wake();
        }
    }

    pub fn run_schedule_now(&self, id: &str) {
        lock(&self.shared.requests).push(Request::Schedule(id.to_string()));
        self.wake();
    }

    pub fn run_everything_now(&self) {
        lock(&self.shared.requests).push(Request::Everything);
        self.wake();
    }

    pub fn running(&self) -> Option<Running> {
        lock(&self.shared.running).clone()
    }

    pub fn is_running(&self) -> bool {
        lock(&self.shared.running).is_some()
    }

    pub fn cancel_running(&self) {
        if let Some(running) = lock(&self.shared.running).as_ref() {
            running.cancel.cancel();
        }
    }

    /// Records that a schedule was switched on (or created) now, so earlier
    /// occurrences are not made up.
    pub fn arm(&self, id: &str) {
        let id = id.to_string();
        State::update(&self.shared.paths.config_file, |state| {
            state.schedules.entry(id).or_default().armed_at = Some(Utc::now());
        });
        self.wake();
    }

    /// Stops the thread after the current run (which is cancelled). Waits a
    /// few seconds for a clean end.
    pub fn shut_down(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.cancel_running();
        self.wake();
        let deadline = Instant::now() + Duration::from_secs(8);
        while self.is_running() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.cancel_running();
        self.wake();
    }
}

fn worker_loop(shared: &Arc<Shared>) {
    // Schedules that exist without bookkeeping (new configuration, first start)
    // start counting from now.
    {
        let config = lock(&shared.config).clone();
        State::update(&shared.paths.config_file, |state| {
            for schedule in &config.schedules {
                let entry = state.schedules.entry(schedule.id.clone()).or_default();
                if entry.armed_at.is_none() {
                    entry.armed_at = Some(Utc::now());
                }
            }
        });
    }

    let mut overdue_checked: Option<Instant> = None;
    while !shared.stop.load(Ordering::Relaxed) {
        if overdue_checked.is_none_or(|at| at.elapsed() > OVERDUE_CHECK) {
            overdue_checked = Some(Instant::now());
            let config = lock(&shared.config).clone();
            crate::notify::check_overdue(&shared.paths.config_file, &config);
        }
        if !shared.window_busy.load(Ordering::Relaxed) && lock(&shared.running).is_none() {
            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| check_and_run(shared)));
            if outcome.is_err() {
                tracing::error!("an automatic backup stopped unexpectedly");
                *lock(&shared.running) = None;
            }
        }
        let (flag, condvar) = &shared.wake;
        let mut woken = lock(flag);
        if !*woken {
            woken = condvar
                .wait_timeout(woken, TICK)
                .map(|(guard, _)| guard)
                .unwrap_or_else(|e| e.into_inner().0);
        }
        *woken = false;
    }
}

/// Picks one request or due schedule and runs it.
fn check_and_run(shared: &Shared) {
    let config = lock(&shared.config).clone();
    let lang = Lang::resolve(config.language);
    let request = {
        let mut requests = lock(&shared.requests);
        (!requests.is_empty()).then(|| requests.remove(0))
    };

    let job: Option<(Option<Schedule>, bool)> = match request {
        Some(Request::Everything) => Some((None, true)),
        Some(Request::Schedule(id)) => config
            .schedules
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .map(|s| (Some(s), true)),
        None => {
            let state = State::load(&shared.paths.config_file);
            let now = Local::now();
            config
                .schedules
                .iter()
                .find(|s| {
                    let entry = state.schedules.get(&s.id).cloned().unwrap_or_default();
                    timing::is_due(s, &entry, now, shared.process_start)
                })
                .cloned()
                .map(|s| (Some(s), false))
        }
    };
    let Some((schedule, requested)) = job else {
        return;
    };

    if schedule.is_some()
        && config.background.only_on_ac_power
        && !requested
        && platform::on_battery()
    {
        tracing::debug!("automatic backup waits for mains power");
        return;
    }

    let label = super::job_label(schedule.as_ref(), &config, lang);
    let cancel = CancelToken::default();
    *lock(&shared.running) = Some(Running {
        label: label.clone(),
        progress: Progress::default(),
        cancel: cancel.clone(),
    });
    (shared.notify)(Event::Started);

    let key = lock(&shared.key).clone();
    let mut last_report = Instant::now();
    let _background = platform::BackgroundThread::enter();
    let (run, _) = super::run_job(
        &shared.paths,
        &config,
        schedule.as_ref(),
        key,
        &cancel,
        &mut |progress| {
            if let Some(running) = lock(&shared.running).as_mut() {
                running.progress = progress.clone();
            }
            if last_report.elapsed() > Duration::from_millis(250) {
                last_report = Instant::now();
                (shared.notify)(Event::Progress);
            }
        },
        false,
    );
    drop(_background);
    *lock(&shared.running) = None;
    (shared.notify)(Event::Finished(Box::new(run)));
}
