//! Lets a keyboard volume wheel control the real volume of a chosen audio output, whatever the
//! Windows default output is.
//!
//! The main thread only carries the keyboard hook and the `audio` thread makes the COM calls.
//! A key is swallowed only if that thread can handle it: otherwise it continues to Windows.
//! The `tray` and `gg` threads add a status icon and the optional SteelSeries features, and
//! nothing else depends on them.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![deny(unsafe_code)]

mod config;
mod gg;
mod gg_protocol;
mod logfile;
mod mixer_ui;
#[allow(unsafe_code)]
mod win;

use std::cell::RefCell;
use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use config::Options;
use gg_protocol::{ChannelLevel, Control};
use win::mixer::{MixerHandlers, MixerView};
use win::tray::{TrayCommand, TrayHandlers, TrayStatus};
use win::{Endpoint, KeyEvent, RenderDevice, VolumeKey};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_STEP: i32 = 2;
const USAGE: &str = "usage: sonar-rollerbar (--device <partial name>)... [--auto] \
                     [--step <percent>] [--injected-only]\n       \
                     sonar-rollerbar --list-devices [--device <partial name>]... [--auto]";
const LEGAL_NOTE: &str = "Not affiliated with SteelSeries.\n\
                          MIT License, Copyright (c) 2026 Bastien Poupelin\n\
                          https://github.com/BastienPoP/sonar-rollerbar";
const RELEASES_URL: &str = "https://github.com/BastienPoP/sonar-rollerbar/releases";
const SHORTCUT_FILE: &str = "sonar-rollerbar.lnk";
const NO_OUTPUT: &str = "No output available";

// Bounded so the callback neither allocates nor waits: when full, the key passes through.
const JOB_QUEUE_SIZE: usize = 32;
const GG_QUEUE_SIZE: usize = 64;
// Follows an output being turned off or back on between two wheel notches.
const REFRESH_PERIOD: Duration = Duration::from_secs(2);
// Two clicks of the wheel measured 268 ms apart. Only delays Mute while the option is on.
const DOUBLE_CLICK: Duration = Duration::from_millis(350);
// Buses of software endpoints, as reported by PKEY_Device_EnumeratorName.
const VIRTUAL_BUSES: [&str; 3] = ["ROOT", "SW", "SWD"];

enum Job {
    Key(VolumeKey),
    Refresh,
    Seen(KeyEvent, bool),
    SetVolume(i32),
    ToggleMute,
}

static INJECTED_ONLY: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);
// True while the audio thread holds a target output: otherwise nothing is swallowed.
static DEVICE_READY: AtomicBool = AtomicBool::new(false);
static JOBS: OnceLock<SyncSender<Job>> = OnceLock::new();
// Indexed by VolumeKey: a key release always shares the fate of its press.
static PRESS_SWALLOWED: [AtomicBool; VolumeKey::COUNT] =
    [const { AtomicBool::new(false) }; VolumeKey::COUNT];

// None of what follows is reachable from the hook callback.
static DEFAULT_OUTPUT_STEP: AtomicI32 = AtomicI32::new(DEFAULT_STEP);
static STATUS: Mutex<Option<Status>> = Mutex::new(None);
static OPTIONS: Mutex<Options> = Mutex::new(Options::new());
static DATA_DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
static GG_EVENTS: OnceLock<SyncSender<gg::Event>> = OnceLock::new();
// The Windows ID of the output Sonar plays to, while the option to follow it is on.
static FOLLOWED_OUTPUT: Mutex<Option<String>> = Mutex::new(None);
static MIXER_OPEN: AtomicBool = AtomicBool::new(false);
static MIXER_LEVELS: Mutex<gg::Mixer> = Mutex::new(gg::Mixer::EMPTY);
static WHEEL_TARGET: Mutex<Control> = Mutex::new(Control::Output);

thread_local! {
    // COM interfaces are not Send: they stay on the audio thread.
    static TARGET: RefCell<Option<Target>> = const { RefCell::new(None) };
}

// Console only, in debug builds. The only place keys may be printed.
macro_rules! log {
    ($($arg:tt)*) => {
        if cfg!(debug_assertions) {
            let _ = writeln!(io::stdout(), $($arg)*);
        }
    };
}

// Program events for the log file. Must never receive key data.
macro_rules! record {
    ($($arg:tt)*) => {
        record_event(&format!($($arg)*))
    };
}

fn record_event(message: &str) {
    log!("{message}");
    if let Some(directory) = DATA_DIRECTORY.get() {
        let line = format!("{} {message}", win::local_timestamp());
        let _ = logfile::append(&directory.join("logs"), &line);
    }
}

fn config_path() -> Option<PathBuf> {
    DATA_DIRECTORY.get().map(|directory| directory.join("config.ini"))
}

/// The outputs that may be controlled, best first.
#[derive(Default)]
struct Selection {
    names: Vec<String>,
    auto: bool,
}

impl Selection {
    fn is_empty(&self) -> bool {
        self.names.is_empty() && !self.auto
    }

    /// Position of `device` in the preference order, `None` if it is not wanted. The output Sonar
    /// plays to comes first, then named outputs in command-line order, then `--auto`.
    fn rank(&self, device: &RenderDevice, followed: Option<&str>) -> Option<usize> {
        if followed == Some(device.id.as_str()) && !is_sonar(&device.name) {
            return Some(0);
        }
        self.names
            .iter()
            .position(|wanted| matches_name(&device.name, wanted))
            .or_else(|| (self.auto && is_physical(device)).then_some(self.names.len()))
            .map(|rank| rank + 1)
    }

    /// Returns the rank and index of the best output in `devices`.
    fn best(&self, devices: &[RenderDevice], followed: Option<&str>) -> Option<(usize, usize)> {
        devices
            .iter()
            .enumerate()
            .filter_map(|(index, device)| Some((self.rank(device, followed)?, index)))
            .min()
    }
}

fn is_sonar(name: &str) -> bool {
    name.to_lowercase().contains("sonar")
}

fn matches_name(name: &str, wanted: &str) -> bool {
    !is_sonar(name) && name.to_lowercase().contains(&wanted.to_lowercase())
}

fn is_physical(device: &RenderDevice) -> bool {
    let is_virtual_bus = device.bus.is_empty()
        || VIRTUAL_BUSES
            .iter()
            .any(|bus| device.bus.eq_ignore_ascii_case(bus));
    !is_virtual_bus && !is_sonar(&device.name)
}

struct Config {
    selection: Selection,
    injected_only: bool,
}

enum Command {
    ListDevices(Selection),
    Run(Config),
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut selection = Selection::default();
    let mut injected_only = false;
    let mut list_devices = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--device" => {
                // An empty name would match any output.
                let name = args.next().filter(|name| !name.trim().is_empty());
                selection.names.push(name.ok_or("--device expects a name")?);
            }
            "--auto" => selection.auto = true,
            "--step" => {
                let step = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .filter(|step| (1..=100).contains(step))
                    .ok_or("--step expects an integer from 1 to 100")?;
                DEFAULT_OUTPUT_STEP.store(step, Ordering::Relaxed);
            }
            "--injected-only" => injected_only = true,
            "--list-devices" => list_devices = true,
            other => return Err(format!("unknown option: {other}\n{USAGE}")),
        }
    }
    if list_devices {
        return Ok(Command::ListDevices(selection));
    }
    if selection.is_empty() {
        return Err(format!(
            "--device or --auto is required, --list-devices shows the available outputs\n{USAGE}"
        ));
    }
    Ok(Command::Run(Config {
        selection,
        injected_only,
    }))
}

fn submit(job: Job) -> bool {
    JOBS.get().is_some_and(|jobs| jobs.try_send(job).is_ok())
}

/// Called from the hook callback: it only decides and queues a message.
fn on_volume_key(event: KeyEvent) -> bool {
    let press_swallowed = &PRESS_SWALLOWED[event.key as usize];
    let swallow = if event.down {
        let eligible = !PAUSED.load(Ordering::Relaxed)
            && (event.injected || !INJECTED_ONLY.load(Ordering::Relaxed));
        let ready = DEVICE_READY.load(Ordering::Relaxed);
        if eligible && !ready {
            submit(Job::Refresh);
        }
        let swallow = eligible && ready && submit(Job::Key(event.key));
        press_swallowed.store(swallow, Ordering::Relaxed);
        swallow
    } else {
        press_swallowed.swap(false, Ordering::Relaxed)
    };
    if cfg!(debug_assertions) {
        submit(Job::Seen(event, swallow));
    }
    swallow
}

struct Target {
    endpoint: Endpoint,
    id: String,
    name: String,
    rank: usize,
}

#[derive(PartialEq)]
struct Status {
    output: String,
    volume: Option<i32>,
    muted: bool,
}

fn notify_gg(event: gg::Event) {
    if let Some(events) = GG_EVENTS.get() {
        let _ = events.try_send(event);
    }
}

/// Shares the current target with the hook, the tray and the `gg` thread.
fn publish_status(slot: &Option<Target>) {
    DEVICE_READY.store(slot.is_some(), Ordering::Relaxed);
    let status = slot.as_ref().map(|target| Status {
        output: target.name.clone(),
        volume: target.endpoint.volume_percent().ok(),
        muted: target.endpoint.is_muted().unwrap_or(false),
    });
    if let Ok(mut current) = STATUS.lock()
        && *current != status
    {
        if current.is_some() && status.is_none() {
            record!("{NO_OUTPUT}");
        }
        *current = status;
        win::tray::refresh_tray();
        notify_gg(gg::Event::OutputChanged);
    }
}

fn wheel_target() -> Control {
    WHEEL_TARGET.lock().map_or(Control::Output, |target| *target)
}

fn set_wheel_target(target: Control) {
    if let Ok(mut current) = WHEEL_TARGET.lock() {
        *current = target;
    }
    win::tray::refresh_tray();
    win::mixer::refresh();
}

fn status_summary() -> String {
    let Ok(status) = STATUS.lock() else {
        return NO_OUTPUT.to_owned();
    };
    let summary = match status.as_ref() {
        Some(Status {
            output,
            muted: true,
            ..
        }) => format!("{output}: muted"),
        Some(Status {
            output,
            volume: Some(percent),
            ..
        }) => format!("{output}: {percent}%"),
        Some(Status { output, .. }) => output.clone(),
        None => NO_OUTPUT.to_owned(),
    };
    match wheel_target() {
        Control::Output => summary,
        target => format!("{summary}, wheel on {}", target.label()),
    }
}

fn step_for(output: &str) -> i32 {
    let step = OPTIONS.lock().ok().and_then(|options| options.step_for(output));
    step.unwrap_or_else(|| DEFAULT_OUTPUT_STEP.load(Ordering::Relaxed))
}

fn current_output() -> Option<String> {
    let status = STATUS.lock().ok()?;
    status.as_ref().map(|status| status.output.clone())
}

fn current_step() -> i32 {
    current_output().map_or_else(
        || DEFAULT_OUTPUT_STEP.load(Ordering::Relaxed),
        |output| step_for(&output),
    )
}

fn startup_shortcut() -> windows::core::Result<PathBuf> {
    Ok(win::startup_folder()?.join(SHORTCUT_FILE))
}

fn tray_status() -> TrayStatus {
    let paused = PAUSED.load(Ordering::Relaxed);
    let summary = if paused {
        "Paused".to_owned()
    } else {
        status_summary()
    };
    let features = gg_features();
    TrayStatus {
        summary,
        paused,
        run_at_startup: startup_shortcut().is_ok_and(|shortcut| shortcut.exists()),
        step: current_step(),
        oled: features.oled,
        sonar_mixer: features.mixer,
        follow_output: features.follow_output,
        wheel_select: features.wheel_select,
    }
}

fn about_text() -> String {
    format!(
        "sonar-rollerbar {VERSION}\n\n\
         Makes a keyboard volume wheel control a chosen audio output.\n\n\
         Only Volume Up, Volume Down and Mute are intercepted. No other key is read or stored.\n\n\
         The program only connects to SteelSeries software on this computer (127.0.0.1), \
         and only for the options you turn on.\n\n{LEGAL_NOTE}"
    )
}

/// Adds or removes the same shortcut as the installer, keeping the current options.
fn toggle_startup() -> Result<(), Box<dyn Error>> {
    let shortcut = startup_shortcut()?;
    if shortcut.exists() {
        fs::remove_file(&shortcut)?;
        record!("run at startup disabled");
        return Ok(());
    }
    let arguments: Vec<String> = env::args()
        .skip(1)
        .map(|argument| format!("\"{}\"", argument.replace('"', "")))
        .collect();
    win::create_shortcut(&shortcut, &env::current_exe()?, &arguments.join(" "))?;
    record!("run at startup enabled");
    Ok(())
}

fn open_logs() -> Result<(), Box<dyn Error>> {
    let directory = DATA_DIRECTORY.get().ok_or("LOCALAPPDATA is not set")?.join("logs");
    fs::create_dir_all(&directory)?;
    Ok(win::open_in_shell(directory.as_os_str())?)
}

fn update_options(change: impl FnOnce(&mut Options)) -> Result<(), Box<dyn Error>> {
    {
        let mut options = OPTIONS.lock().map_err(|_| "options unavailable")?;
        change(&mut options);
        if let Some(path) = config_path() {
            options.save(&path)?;
        }
    }
    notify_gg(gg::Event::OptionsChanged);
    Ok(())
}

fn on_tray_command(command: TrayCommand) {
    let result = match command {
        TrayCommand::TogglePause => {
            let was_paused = PAUSED.fetch_not(Ordering::Relaxed);
            record!("{}", if was_paused { "resumed" } else { "paused" });
            Ok(())
        }
        TrayCommand::ToggleStartup => toggle_startup(),
        TrayCommand::CheckForUpdates => {
            win::open_in_shell(OsStr::new(RELEASES_URL)).map_err(Into::into)
        }
        TrayCommand::OpenLogs => open_logs(),
        TrayCommand::SetStep(step) => match current_output() {
            Some(output) => update_options(|options| options.set_step(&output, step)),
            None => Ok(()),
        },
        TrayCommand::ToggleOled => update_options(|options| options.oled = !options.oled),
        TrayCommand::ToggleSonarMixer => {
            update_options(|options| options.sonar_mixer = !options.sonar_mixer)
        }
        TrayCommand::ToggleFollowOutput => {
            update_options(|options| options.follow_output = !options.follow_output)
        }
        TrayCommand::ToggleWheelSelect => {
            update_options(|options| options.wheel_select = !options.wheel_select)
        }
    };
    if let Err(error) = result {
        record!("tray command failed: {error}");
    }
}

fn mixer_view() -> MixerView {
    let levels = MIXER_LEVELS.lock().map_or(gg::Mixer::EMPTY, |levels| *levels);
    let output = gg_output();
    MixerView {
        output_name: output.as_ref().map_or_else(String::new, |output| output.name.clone()),
        output: output.map(|output| ChannelLevel {
            percent: output.volume,
            muted: output.muted,
        }),
        channels: levels.channels,
        chat_mix: levels.chat_mix,
        selected: wheel_target(),
        wheel_select: gg_features().wheel_select,
    }
}

fn on_mixer_change(control: Control, value: i32) {
    match control {
        Control::Output => {
            submit(Job::SetVolume(value));
        }
        Control::Channel(index) => {
            if let Ok(mut levels) = MIXER_LEVELS.lock()
                && let Some(level) = &mut levels.channels[index]
            {
                level.percent = value;
            }
            notify_gg(gg::Event::SetChannel(index, value));
        }
        Control::ChatMix => notify_gg(gg::Event::SetChatMix(value)),
    }
}

fn on_mixer_mute(control: Control) {
    match control {
        Control::Channel(index) => notify_gg(gg::Event::ToggleChannelMute(index)),
        _ => {
            submit(Job::ToggleMute);
        }
    }
}

fn on_mixer_visible(visible: bool) {
    MIXER_OPEN.store(visible, Ordering::Relaxed);
    notify_gg(gg::Event::OptionsChanged);
}

fn publish_mixer(mixer: gg::Mixer) {
    if let Ok(mut current) = MIXER_LEVELS.lock() {
        *current = mixer;
    }
    win::mixer::refresh();
}

fn gg_features() -> gg::Features {
    OPTIONS.lock().map_or(
        gg::Features {
            oled: false,
            mixer: false,
            follow_output: false,
            wheel_select: false,
        },
        |options| gg::Features {
            oled: options.oled,
            mixer: options.sonar_mixer,
            follow_output: options.follow_output,
            wheel_select: options.wheel_select,
        },
    )
}

fn gg_output() -> Option<gg::Output> {
    let status = STATUS.lock().ok()?;
    let status = status.as_ref()?;
    Some(gg::Output {
        name: status.output.clone(),
        volume: status.volume?,
        muted: status.muted,
    })
}

fn followed_output() -> Option<String> {
    FOLLOWED_OUTPUT.lock().ok()?.clone()
}

/// A lost refresh only delays the move, as the audio thread reads this value every 2 seconds.
fn follow_output(device: Option<&str>) {
    if let Ok(mut followed) = FOLLOWED_OUTPUT.lock()
        && followed.as_deref() != device
    {
        *followed = device.map(str::to_owned);
        submit(Job::Refresh);
    }
}

fn mixer_is_open() -> bool {
    MIXER_OPEN.load(Ordering::Relaxed)
}

/// Opens the best available output if it ranks better than `current_rank`.
fn open_better(
    selection: &Selection,
    followed: Option<&str>,
    current_rank: usize,
) -> windows::core::Result<Option<Target>> {
    let mut devices = win::active_render_devices()?;
    let best = selection.best(&devices, followed);
    let Some((rank, index)) = best.filter(|(rank, _)| *rank < current_rank) else {
        return Ok(None);
    };
    let device = devices.swap_remove(index);
    record!("target output: {}", device.name);
    let (id, name) = (device.id.clone(), device.name.clone());
    let endpoint = device.open()?;
    Ok(Some(Target {
        endpoint,
        id,
        name,
        rank,
    }))
}

/// Works in whole percents: the scalar read back would otherwise drift with each notch.
fn apply_key(target: &Target, key: VolumeKey) -> windows::core::Result<i32> {
    let endpoint = &target.endpoint;
    let delta = match key {
        VolumeKey::Up => step_for(&target.name),
        VolumeKey::Down => -step_for(&target.name),
        VolumeKey::Mute => {
            endpoint.set_muted(!endpoint.is_muted()?)?;
            return endpoint.volume_percent();
        }
    };
    let percent = (endpoint.volume_percent()? + delta).clamp(0, 100);
    endpoint.set_volume_percent(percent)?;
    if delta > 0 {
        endpoint.set_muted(false)?;
    }
    Ok(percent)
}

fn handle_key(
    slot: &mut Option<Target>,
    selection: &Selection,
    key: VolumeKey,
) -> windows::core::Result<Option<i32>> {
    if let Some(target) = slot.as_ref().filter(|target| target.endpoint.is_active())
        && let Ok(percent) = apply_key(target, key)
    {
        return Ok(Some(percent));
    }
    // The output was turned off or back on: the held endpoint can no longer be used.
    *slot = None;
    *slot = open_better(selection, followed_output().as_deref(), usize::MAX)?;
    slot.as_ref()
        .map(|target| apply_key(target, key))
        .transpose()
}

fn on_key(config: &Config, key: VolumeKey) {
    if wheel_target() == Control::Output {
        on_output_key(config, key);
        return;
    }
    notify_gg(match key {
        VolumeKey::Up => gg::Event::Turn(1),
        VolumeKey::Down => gg::Event::Turn(-1),
        VolumeKey::Mute => gg::Event::Click,
    });
}

fn on_output_key(config: &Config, key: VolumeKey) {
    TARGET.with_borrow_mut(|slot| {
        match handle_key(slot, &config.selection, key) {
            Ok(Some(percent)) => log!("volume: {percent}%"),
            Ok(None) => log!("{NO_OUTPUT}"),
            Err(error) => record!("audio error: {error}"),
        }
        publish_status(slot);
    });
}

/// Drops an output that went away or that Sonar left, and moves to a better one.
fn refresh_target(selection: &Selection) {
    let followed = followed_output();
    let followed = followed.as_deref();
    TARGET.with_borrow_mut(|slot| {
        let usable = slot.as_ref().is_some_and(|target| {
            target.endpoint.is_active() && (target.rank > 0 || followed == Some(target.id.as_str()))
        });
        if !usable {
            *slot = None;
        }
        let current_rank = slot.as_ref().map_or(usize::MAX, |target| target.rank);
        if current_rank > 0 {
            match open_better(selection, followed, current_rank) {
                Ok(Some(better)) => *slot = Some(better),
                Ok(None) => {}
                Err(error) => record!("audio error: {error}"),
            }
        }
        publish_status(slot);
    });
}

fn on_set_volume(percent: i32) {
    TARGET.with_borrow_mut(|slot| {
        if let Some(target) = slot.as_ref() {
            match target.endpoint.set_volume_percent(percent.clamp(0, 100)) {
                Ok(()) => log!("volume from the mixer: {percent}%"),
                Err(error) => record!("audio error: {error}"),
            }
        }
        publish_status(slot);
    });
}

fn log_key(event: KeyEvent, swallowed: bool) {
    let yes_no = |value| if value { "yes" } else { "no" };
    log!(
        "key 0x{:02X} {} injected={} swallowed={}",
        event.key.code(),
        if event.down { "press" } else { "release" },
        yes_no(event.injected),
        yes_no(swallowed),
    );
}

// If this thread stops or panics, the queue closes and the hook lets every key through.
fn audio_worker(config: Config, jobs: Receiver<Job>) {
    if let Err(error) = win::init_com() {
        record!("COM unavailable: {error}");
        return;
    }
    refresh_target(&config.selection);
    // Set between the first click of the wheel and the end of the wait for a second one.
    let mut click_deadline: Option<Instant> = None;
    loop {
        let wait = click_deadline.map_or(REFRESH_PERIOD, |deadline| {
            deadline.saturating_duration_since(Instant::now())
        });
        match jobs.recv_timeout(wait) {
            Ok(Job::Key(VolumeKey::Mute)) if gg_features().wheel_select => {
                if click_deadline.take().is_some() {
                    notify_gg(gg::Event::NextTarget);
                } else {
                    click_deadline = Some(Instant::now() + DOUBLE_CLICK);
                }
            }
            Ok(Job::Key(key)) => {
                if click_deadline.take().is_some() {
                    on_key(&config, VolumeKey::Mute);
                }
                on_key(&config, key);
            }
            Ok(Job::Seen(event, swallowed)) => log_key(event, swallowed),
            Ok(Job::SetVolume(percent)) => on_set_volume(percent),
            Ok(Job::ToggleMute) => on_output_key(&config, VolumeKey::Mute),
            Ok(Job::Refresh) => refresh_target(&config.selection),
            Err(RecvTimeoutError::Timeout) => {
                if click_deadline.take().is_some() {
                    on_key(&config, VolumeKey::Mute);
                } else {
                    refresh_target(&config.selection);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn list_devices(selection: &Selection) -> Result<(), Box<dyn Error>> {
    win::attach_parent_console();
    win::init_com()?;
    let devices = win::active_render_devices()?;
    let selected = selection.best(&devices, None).map(|(_, index)| index);
    let mut stdout = io::stdout();
    for (index, device) in devices.iter().enumerate() {
        let marker = if selected == Some(index) { '*' } else { ' ' };
        let kind = if is_physical(device) {
            "[physical]"
        } else {
            "[virtual]"
        };
        writeln!(stdout, "{marker} {kind:<10} {}", device.name)?;
    }
    Ok(())
}

fn run() -> Result<(), Box<dyn Error>> {
    let config = match parse_args(env::args().skip(1))? {
        Command::ListDevices(selection) => return list_devices(&selection),
        Command::Run(config) => config,
    };

    if !win::acquire_single_instance()? {
        return Err("another instance is already running".into());
    }

    if let Some(local_data) = env::var_os("LOCALAPPDATA") {
        let _ = DATA_DIRECTORY.set(PathBuf::from(local_data).join("sonar-rollerbar"));
    }
    record!("started version {VERSION}");
    if let (Some(path), Ok(mut options)) = (config_path(), OPTIONS.lock()) {
        *options = Options::load(&path);
    }

    INJECTED_ONLY.store(config.injected_only, Ordering::Relaxed);
    let (sender, receiver) = mpsc::sync_channel(JOB_QUEUE_SIZE);
    let _ = JOBS.set(sender);
    let (gg_sender, gg_receiver) = mpsc::sync_channel(GG_QUEUE_SIZE);
    let _ = GG_EVENTS.set(gg_sender);

    thread::Builder::new()
        .name("audio".to_owned())
        .spawn(move || audio_worker(config, receiver))?;
    thread::Builder::new().name("gg".to_owned()).spawn(move || {
        let hooks = gg::Hooks {
            features: gg_features,
            output: gg_output,
            follow_output,
            step: current_step,
            set_wheel_target,
            mixer_open: mixer_is_open,
            publish_mixer,
            report: record_event,
        };
        gg::run(gg_receiver, hooks);
    })?;
    thread::Builder::new().name("tray".to_owned()).spawn(|| {
        let handlers = TrayHandlers {
            status: tray_status,
            on_command: on_tray_command,
            about: about_text,
        };
        let mixer_handlers = MixerHandlers {
            view: mixer_view,
            step: current_step,
            on_change: on_mixer_change,
            on_mute: on_mixer_mute,
            on_visible: on_mixer_visible,
        };
        if let Err(error) = win::tray::run_tray(handlers, mixer_handlers) {
            record!("tray unavailable: {error}");
        }
    })?;

    win::run_keyboard_hook(on_volume_key)?;
    record!("stopped");
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            record!("failed: {error}");
            win::attach_parent_console();
            let _ = writeln!(io::stderr(), "sonar-rollerbar: {error}");
            ExitCode::FAILURE
        }
    }
}
