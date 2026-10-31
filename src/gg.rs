//! The `gg` thread: everything that talks to SteelSeries GG, Sonar and GameSense.
//!
//! It only sends requests for the options that are turned on: with all of them off, it opens no
//! connection. It runs apart so that a slow or missing GG can't delay the volume wheel, and every
//! request stays on 127.0.0.1 because `win::loopback` takes no host.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::gg_protocol::{
    CHANNELS, ChannelLevel, ChatMix, Control, Mode, channel_level, json_string, loopback_port,
    oled_balance, oled_binding, oled_frame, oled_level, oled_metadata, oled_stop, oled_title,
    routed_output, sonar_port, ui_action_message,
};
use crate::win::loopback::{Scheme, request, websocket_send};

const POLL_PERIOD: Duration = Duration::from_millis(500);
const RETRY_PERIOD: Duration = Duration::from_secs(5);
// Hands the OLED back to its usual view once the wheel stops moving.
const OLED_RELEASE_AFTER: Duration = Duration::from_secs(3);
const IDLE_WAIT: Duration = Duration::from_secs(3600);
// Sonar logs a warning above 3 ChatMix calls in 500 ms.
const CHAT_MIX_INTERVAL: Duration = Duration::from_millis(200);
const WHEEL_RETURN_AFTER: Duration = Duration::from_secs(10);

pub enum Event {
    OutputChanged,
    SetChannel(usize, i32),
    ToggleChannelMute(usize),
    SetChatMix(i32),
    OptionsChanged,
    NextTarget,
    Turn(i32),
    Click,
}

#[derive(Clone, Copy)]
pub struct Features {
    pub oled: bool,
    pub mixer: bool,
    pub follow_output: bool,
    pub wheel_select: bool,
}

pub struct Output {
    pub name: String,
    pub volume: i32,
    pub muted: bool,
}

#[derive(Clone, Copy)]
pub struct Mixer {
    pub channels: [Option<ChannelLevel>; CHANNELS.len()],
    pub chat_mix: Option<ChatMix>,
}

impl Mixer {
    pub const EMPTY: Self = Self {
        channels: [None; CHANNELS.len()],
        chat_mix: None,
    };
}

pub struct Hooks {
    pub features: fn() -> Features,
    pub output: fn() -> Option<Output>,
    pub follow_output: fn(Option<&str>),
    pub step: fn() -> i32,
    pub set_wheel_target: fn(Control),
    pub mixer_open: fn() -> bool,
    pub publish_mixer: fn(Mixer),
    pub report: fn(&str),
}

fn core_props() -> Result<String, String> {
    let program_data = env::var_os("ProgramData").ok_or("ProgramData is not set")?;
    let path = PathBuf::from(program_data).join("SteelSeries/SteelSeries Engine 3/coreProps.json");
    fs::read_to_string(path).map_err(|error| format!("coreProps.json: {error}"))
}

fn props_port(props: &str, key: &str) -> Result<u16, String> {
    json_string(props, key)
        .and_then(loopback_port)
        .ok_or_else(|| format!("coreProps.json has no local {key}"))
}

fn get(port: u16, path: &str) -> Result<String, String> {
    request("GET", Scheme::Http, port, path, None).map_err(|error| error.to_string())
}

fn put(port: u16, path: &str) -> Result<String, String> {
    request("PUT", Scheme::Http, port, path, None).map_err(|error| error.to_string())
}

fn post_json(port: u16, path: &str, body: &str) -> Result<(), String> {
    request("POST", Scheme::Http, port, path, Some(body)).map_err(|error| error.to_string())?;
    Ok(())
}

#[derive(Default)]
struct Oled {
    port: Option<u16>,
    release_at: Option<Instant>,
}

impl Oled {
    fn connect(&mut self) -> Result<u16, String> {
        if let Some(port) = self.port {
            return Ok(port);
        }
        let port = props_port(&core_props()?, "address")?;
        post_json(port, "/game_metadata", &oled_metadata())?;
        post_json(port, "/bind_game_event", &oled_binding())?;
        self.port = Some(port);
        Ok(port)
    }

    fn show(&mut self, title: &str, level: &str) -> Result<(), String> {
        let port = self.connect()?;
        let frame = oled_frame(title, level);
        post_json(port, "/game_event", &frame).inspect_err(|_| self.port = None)?;
        self.release_at = Some(Instant::now() + OLED_RELEASE_AFTER);
        Ok(())
    }

    fn release_if_due(&mut self) {
        if self.release_at.is_some_and(|at| Instant::now() >= at) {
            self.release_at = None;
            if let Some(port) = self.port {
                let _ = post_json(port, "/stop_game", &oled_stop());
            }
        }
    }
}

/// What the wheel controls. Pointing it away from the output has to end by itself, or a forgotten
/// selection would take a full cycle of double clicks to undo.
struct Wheel {
    target: Control,
    return_at: Option<Instant>,
}

impl Wheel {
    fn set(&mut self, target: Control, hooks: &Hooks) {
        if self.target != target {
            self.target = target;
            (hooks.set_wheel_target)(target);
        }
        self.return_at = None;
    }

    fn keep_target(&mut self) {
        if self.target != Control::Output {
            self.return_at = Some(Instant::now() + WHEEL_RETURN_AFTER);
        }
    }

    fn advance(&mut self, chat_mix_available: bool, hooks: &Hooks) {
        self.set(self.target.next(chat_mix_available), hooks);
        self.keep_target();
    }

    fn return_if_due(&mut self, hooks: &Hooks) {
        if self.return_at.is_some_and(|at| Instant::now() >= at) {
            self.set(Control::Output, hooks);
        }
    }
}

/// Everything asked for since the last pass, which keeps only the latest value of a fast wheel or
/// a dragged slider.
#[derive(Default)]
struct Requests {
    output_changed: bool,
    next_target: bool,
    turns: i32,
    click: bool,
    levels: [Option<i32>; CHANNELS.len()],
    mute_toggles: [bool; CHANNELS.len()],
    balance: Option<i32>,
}

impl Requests {
    fn add(&mut self, event: Event) {
        match event {
            Event::OutputChanged => self.output_changed = true,
            Event::SetChannel(index, percent) => self.levels[index] = Some(percent),
            Event::ToggleChannelMute(index) => self.mute_toggles[index] ^= true,
            Event::SetChatMix(balance) => self.balance = Some(balance),
            Event::OptionsChanged => {}
            Event::NextTarget => self.next_target = true,
            Event::Turn(notches) => self.turns += notches,
            Event::Click => self.click = true,
        }
    }

    fn moves_wheel(&self) -> bool {
        self.turns != 0 || self.click
    }

    fn needs_sonar(&self) -> bool {
        self.next_target
            || self.balance.is_some()
            || self.levels.iter().any(Option::is_some)
            || self.mute_toggles.contains(&true)
    }
}

#[derive(Default)]
struct Sonar {
    port: Option<u16>,
    gg_port: Option<u16>,
    retry_at: Option<Instant>,
    mixer: Option<Mixer>,
    pending_balance: Option<i32>,
    chat_mix_at: Option<Instant>,
}

impl Sonar {
    /// Sonar's port changes when GG restarts, so it has to be looked up.
    fn connect(&mut self) -> Result<u16, String> {
        if let Some(port) = self.port {
            return Ok(port);
        }
        let gg_port = props_port(&core_props()?, "ggEncryptedAddress")?;
        let sub_apps = request("GET", Scheme::Https, gg_port, "/subApps", None)
            .map_err(|error| format!("GG: {error}"))?;
        let port = sonar_port(&sub_apps).ok_or("GG reports no local Sonar address")?;
        self.gg_port = Some(gg_port);
        self.port = Some(port);
        Ok(port)
    }

    fn set_channel(&self, port: u16, mode: Mode, index: usize, percent: i32) -> Result<(), String> {
        let channel = &CHANNELS[index];
        put(port, &mode.set_volume_path(channel, percent))?;
        // Only Streamer mode has actions that make the GG window redraw, and it is only cosmetic.
        if let (Mode::Stream, Some(action), Some(gg_port)) =
            (mode, channel.ui_action, self.gg_port)
        {
            let _ = websocket_send(gg_port, "/eventing", &ui_action_message(action, percent));
        }
        Ok(())
    }

    /// Turns what the wheel did into changes for its target, from the last known values.
    fn apply_wheel(&mut self, target: Control, requests: &mut Requests, step: i32) {
        let mixer = self.mixer.unwrap_or(Mixer::EMPTY);
        match target {
            Control::Output => {}
            Control::Channel(index) => {
                if let (Some(level), true) = (mixer.channels[index], requests.turns != 0) {
                    let percent = level.percent + requests.turns * step;
                    requests.levels[index] = Some(percent.clamp(0, 100));
                }
                requests.mute_toggles[index] ^= requests.click;
            }
            Control::ChatMix => {
                let balance = self.pending_balance.or(mixer.chat_mix.map(|mix| mix.balance));
                if let (Some(balance), true) = (balance, requests.turns != 0) {
                    requests.balance = Some(ChatMix::turned(balance, requests.turns));
                }
                if requests.click {
                    requests.balance = Some(0);
                }
            }
        }
    }

    /// Without an answer the last output stays, so that a GG restart doesn't move the wheel.
    fn follow_route(&self, port: u16, mode: Mode, hooks: &Hooks) -> Result<(), String> {
        let routing = get(port, mode.routing_path())?;
        if let Some(device) = routed_output(&routing, mode) {
            (hooks.follow_output)(Some(device));
        }
        Ok(())
    }

    /// A failure here must not stop the rest of Sonar: an older GG may not have ChatMix at all.
    fn sync_chat_mix(
        &mut self,
        port: u16,
        wanted: bool,
        current: Option<ChatMix>,
    ) -> Option<ChatMix> {
        if !wanted && self.pending_balance.is_none() {
            return None;
        }
        if self.chat_mix_at.is_some_and(|at| Instant::now() < at) {
            return current;
        }
        self.chat_mix_at = Some(Instant::now() + CHAT_MIX_INTERVAL);
        let body = match self.pending_balance.take() {
            Some(balance) => put(port, &ChatMix::write_path(balance)),
            None => get(port, ChatMix::READ_PATH),
        };
        body.ok().and_then(|body| ChatMix::parse(&body))
    }

    fn tick(
        &mut self,
        requests: &Requests,
        target: Control,
        features: Features,
        hooks: &Hooks,
    ) -> Result<(), String> {
        let port = self.connect()?;
        let mode = Mode::parse(&get(port, "/mode/")?).ok_or("unexpected Sonar mode")?;
        let known = self.mixer.unwrap_or(Mixer::EMPTY);
        for (index, percent) in requests.levels.iter().enumerate() {
            if let Some(percent) = percent {
                self.set_channel(port, mode, index, *percent)?;
            }
        }
        for (index, _) in requests.mute_toggles.iter().enumerate().filter(|(_, toggle)| **toggle) {
            if let Some(level) = known.channels[index] {
                put(port, &mode.set_mute_path(&CHANNELS[index], !level.muted))?;
            }
        }
        if requests.balance.is_some() {
            self.pending_balance = requests.balance;
        }

        let volumes = get(port, mode.volumes_path())?;
        if features.follow_output {
            self.follow_route(port, mode, hooks)?;
        }

        let mixer_visible = features.mixer && (hooks.mixer_open)();
        let wants_chat_mix = mixer_visible || requests.next_target || target == Control::ChatMix;
        let mixer = Mixer {
            channels: std::array::from_fn(|index| channel_level(&volumes, mode, &CHANNELS[index])),
            chat_mix: self.sync_chat_mix(port, wants_chat_mix, known.chat_mix),
        };
        self.mixer = Some(mixer);
        if mixer_visible {
            (hooks.publish_mixer)(mixer);
        }
        Ok(())
    }
}

/// Reports a problem once rather than on every attempt, which keeps the log quiet.
#[derive(Default)]
struct Reporter {
    last: Option<String>,
}

impl Reporter {
    fn outcome(&mut self, label: &str, result: Result<(), String>, hooks: &Hooks) {
        match result {
            Ok(()) => {
                if self.last.take().is_some() {
                    (hooks.report)(&format!("{label} connected"));
                }
            }
            Err(error) => {
                if self.last.as_ref() != Some(&error) {
                    (hooks.report)(&format!("{label} unavailable: {error}"));
                    self.last = Some(error);
                }
            }
        }
    }
}

fn wait_time(polling: bool, oled: &Oled, wheel: &Wheel, sonar: &Sonar) -> Duration {
    let chat_mix_write = sonar.pending_balance.and(sonar.chat_mix_at);
    [oled.release_at, wheel.return_at, chat_mix_write]
        .into_iter()
        .flatten()
        .map(|at| at.saturating_duration_since(Instant::now()))
        .fold(if polling { POLL_PERIOD } else { IDLE_WAIT }, Duration::min)
}

fn show_on_oled(
    oled: &mut Oled,
    target: Control,
    mixer: Option<Mixer>,
    hooks: &Hooks,
) -> Result<(), String> {
    let lines = match target {
        Control::Output => (hooks.output)().map(|output| {
            (oled_title(&output.name), oled_level(output.volume, output.muted))
        }),
        Control::Channel(index) => mixer.and_then(|mixer| mixer.channels[index]).map(|level| {
            (target.label().to_owned(), oled_level(level.percent, level.muted))
        }),
        Control::ChatMix => mixer.and_then(|mixer| mixer.chat_mix).map(|mix| {
            (target.label().to_owned(), oled_balance(mix.balance))
        }),
    };
    lines.map_or(Ok(()), |(title, level)| oled.show(&title, &level))
}

fn sonar_in_use(features: Features, wheel: &Wheel, hooks: &Hooks) -> bool {
    features.follow_output
        || (features.mixer && (hooks.mixer_open)())
        || wheel.target != Control::Output
}

/// Runs until the program stops. Waits for events and only polls Sonar when an option needs it.
pub fn run(events: Receiver<Event>, hooks: Hooks) {
    let mut oled = Oled::default();
    let mut sonar = Sonar::default();
    let mut wheel = Wheel {
        target: Control::Output,
        return_at: None,
    };
    let mut oled_report = Reporter::default();
    let mut sonar_report = Reporter::default();
    loop {
        let polling = sonar_in_use((hooks.features)(), &wheel, &hooks);
        let mut requests = Requests::default();
        match events.recv_timeout(wait_time(polling, &oled, &wheel, &sonar)) {
            Ok(event) => requests.add(event),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        while let Ok(event) = events.try_recv() {
            requests.add(event);
        }

        let features = (hooks.features)();
        if !features.follow_output {
            (hooks.follow_output)(None);
        }
        if !features.wheel_select {
            requests.next_target = false;
            wheel.set(Control::Output, &hooks);
        }
        wheel.return_if_due(&hooks);
        sonar.apply_wheel(wheel.target, &mut requests, (hooks.step)());
        if requests.moves_wheel() {
            wheel.keep_target();
        }

        let wants_sonar = sonar_in_use(features, &wheel, &hooks) || requests.needs_sonar();
        let retry_allowed = sonar.retry_at.is_none_or(|at| Instant::now() >= at);
        if wants_sonar && retry_allowed {
            let result = sonar.tick(&requests, wheel.target, features, &hooks);
            sonar.retry_at = result.is_err().then(|| Instant::now() + RETRY_PERIOD);
            if result.is_err() {
                sonar.port = None;
                // Keys that Sonar can't act on go back to doing something.
                wheel.set(Control::Output, &hooks);
            } else if requests.next_target {
                let chat_mix = sonar.mixer.and_then(|mixer| mixer.chat_mix);
                wheel.advance(chat_mix.is_some_and(ChatMix::is_available), &hooks);
            }
            sonar_report.outcome("Sonar", result, &hooks);
        }

        let shown = requests.next_target
            || requests.moves_wheel()
            || (requests.output_changed && wheel.target == Control::Output);
        if features.oled && shown {
            let result = show_on_oled(&mut oled, wheel.target, sonar.mixer, &hooks);
            oled_report.outcome("OLED", result, &hooks);
        }
        oled.release_if_due();
    }
}
