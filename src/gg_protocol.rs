//! Parsing, messages and decisions for SteelSeries GG, Sonar and GameSense, free of Windows calls
//! so they can be tested anywhere: `rustc --edition 2024 --test src/gg_protocol.rs`.
//!
//! The documents are small and known, and the program must stay free of extra crates, so these
//! helpers search for the few fields needed. Anything unexpected returns `None`.

const OLED_GAME: &str = "SONAR_ROLLERBAR";
const OLED_EVENT: &str = "VOLUME";
// Keeps line two within what the Apex Pro OLED shows: "[========] 100%".
const OLED_BAR_WIDTH: usize = 8;
const OLED_TITLE_WIDTH: usize = 16;
// Cells on each side of the centre of "G [--==|----] C".
const OLED_BALANCE_WIDTH: usize = 4;
const CHAT_MIX_PATH: &str = "/v1/chatMix";
const CHAT_MIX_STEP: i32 = 5;

#[derive(Debug, PartialEq)]
pub struct Channel {
    pub key: &'static str,
    pub label: &'static str,
    /// Streamer-mode action that makes the GG window redraw. Classic mode has none.
    pub ui_action: Option<&'static str>,
}

pub const CHANNELS: [Channel; 6] = [
    Channel {
        key: "master",
        label: "Master",
        ui_action: Some("MONITOR_MASTER_VOLUME"),
    },
    Channel {
        key: "game",
        label: "Game",
        ui_action: Some("MONITOR_GAME_VOLUME"),
    },
    Channel {
        key: "chatRender",
        label: "Chat",
        ui_action: Some("MONITOR_CHAT_VOLUME"),
    },
    Channel {
        key: "media",
        label: "Media",
        ui_action: Some("MONITOR_MEDIA_VOLUME"),
    },
    Channel {
        key: "aux",
        label: "Aux",
        ui_action: Some("MONITOR_AUX_VOLUME"),
    },
    Channel {
        key: "chatCapture",
        label: "Mic",
        ui_action: None,
    },
];
const GAME: usize = 1;
const AUX: usize = 4;

/// Something the mixer window can set and the wheel can be pointed at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Control {
    Output,
    Channel(usize),
    ChatMix,
}

impl Control {
    pub fn label(self) -> &'static str {
        match self {
            Self::Output => "Output",
            Self::Channel(index) => CHANNELS[index].label,
            Self::ChatMix => "ChatMix",
        }
    }

    /// The wheel's next target, with ChatMix one double click away. Master and the Mic aren't
    /// targets: the mixer window sets them.
    pub fn next(self, chat_mix_available: bool) -> Self {
        match self {
            Self::Output if chat_mix_available => Self::ChatMix,
            Self::Output | Self::ChatMix => Self::Channel(GAME),
            Self::Channel(index) if index < AUX => Self::Channel(index + 1),
            Self::Channel(_) => Self::Output,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Classic,
    Stream,
}

impl Mode {
    pub fn parse(body: &str) -> Option<Self> {
        match body.trim().trim_matches('"') {
            "classic" => Some(Self::Classic),
            "stream" => Some(Self::Stream),
            _ => None,
        }
    }

    pub fn volumes_path(self) -> &'static str {
        match self {
            Self::Classic => "/volumeSettings/classic",
            Self::Stream => "/volumeSettings/streamer",
        }
    }

    pub fn set_volume_path(self, channel: &Channel, percent: i32) -> String {
        let (key, value) = (channel.key, f64::from(percent) / 100.0);
        match self {
            Self::Classic => format!("/volumeSettings/classic/{key}/Volume/{value:.2}"),
            Self::Stream => {
                format!("/volumeSettings/streamer/monitoring/{key}/Volume/{value:.2}")
            }
        }
    }

    pub fn set_mute_path(self, channel: &Channel, muted: bool) -> String {
        let key = channel.key;
        match self {
            Self::Classic => format!("/volumeSettings/classic/{key}/Mute/{muted}"),
            Self::Stream => format!("/volumeSettings/streamer/monitoring/{key}/isMuted/{muted}"),
        }
    }

    pub fn routing_path(self) -> &'static str {
        match self {
            Self::Classic => "/classicRedirections",
            Self::Stream => "/streamRedirections",
        }
    }
}

fn after_key<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let (_, rest) = json.split_once(&format!("\"{key}\""))?;
    rest.trim_start().strip_prefix(':').map(str::trim_start)
}

pub fn json_string<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    after_key(json, key)?.strip_prefix('"')?.split('"').next()
}

fn json_number(json: &str, key: &str) -> Option<f64> {
    let rest = after_key(json, key)?;
    let end = rest.find(|c: char| !matches!(c, '0'..='9' | '.' | '-' | '+' | 'e' | 'E'))?;
    rest[..end].parse().ok()
}

/// Accepts `127.0.0.1:6327` and `http://127.0.0.1:8150`. Any other host is refused.
pub fn loopback_port(address: &str) -> Option<u16> {
    let address = address.strip_prefix("http://").unwrap_or(address);
    address.strip_prefix("127.0.0.1:")?.trim_end_matches('/').parse().ok()
}

pub fn sonar_port(sub_apps: &str) -> Option<u16> {
    let (_, sonar) = sub_apps.split_once("\"sonar\"")?;
    loopback_port(json_string(sonar, "webServerAddress")?)
}

/// The part of Sonar's volume document that follows this channel's slider.
fn channel_node<'a>(volumes: &'a str, mode: Mode, channel: &Channel) -> Option<&'a str> {
    let (_, section) = if channel.key == "master" {
        volumes.split_once("\"masters\"")?
    } else {
        let (_, devices) = volumes.split_once("\"devices\"")?;
        devices.split_once(&format!("\"{}\"", channel.key))?
    };
    let slider = match mode {
        Mode::Classic => "\"classic\"",
        Mode::Stream => "\"monitoring\"",
    };
    let (_, node) = section.split_once(slider)?;
    Some(node)
}

/// Reads a channel's volume in whole percents from Sonar's volume document.
fn channel_percent(volumes: &str, mode: Mode, channel: &Channel) -> Option<i32> {
    let value = json_number(channel_node(volumes, mode, channel)?, "volume")?;
    (0.0..=1.0).contains(&value).then(|| (value * 100.0).round() as i32)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChannelLevel {
    pub percent: i32,
    pub muted: bool,
}

pub fn channel_level(volumes: &str, mode: Mode, channel: &Channel) -> Option<ChannelLevel> {
    let node = channel_node(volumes, mode, channel)?;
    Some(ChannelLevel {
        percent: channel_percent(volumes, mode, channel)?,
        muted: after_key(node, "muted")?.starts_with("true"),
    })
}

/// Sonar refuses to set ChatMix in some setups, as the GG window does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChatMixState {
    Enabled,
    HardwareDial,
    DifferentOutputs,
    Unavailable,
}

impl ChatMixState {
    pub fn note(self) -> &'static str {
        match self {
            Self::Enabled => "",
            Self::HardwareDial => "Use the ChatMix dial on your headset.",
            Self::DifferentOutputs => "Game and Chat must use the same output.",
            Self::Unavailable => "Sonar can't use ChatMix on this output.",
        }
    }
}

/// `balance` goes from -100, all Game, to 100, all Chat.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChatMix {
    pub balance: i32,
    pub state: ChatMixState,
}

impl ChatMix {
    pub const READ_PATH: &str = CHAT_MIX_PATH;

    pub fn parse(body: &str) -> Option<Self> {
        let balance = json_number(body, "balance")?;
        let state = match json_string(body, "state")? {
            "enabled" => ChatMixState::Enabled,
            "finiteWheel" => ChatMixState::HardwareDial,
            "differentDeviceSelected" => ChatMixState::DifferentOutputs,
            _ => ChatMixState::Unavailable,
        };
        (-1.0..=1.0).contains(&balance).then(|| Self {
            balance: (balance * 100.0).round() as i32,
            state,
        })
    }

    pub fn is_available(self) -> bool {
        self.state == ChatMixState::Enabled
    }

    pub fn write_path(balance: i32) -> String {
        format!("{CHAT_MIX_PATH}?balance={:.2}", f64::from(balance) / 100.0)
    }

    /// Wheel notches lean towards Chat when they are positive.
    pub fn turned(balance: i32, notches: i32) -> i32 {
        (balance + notches * CHAT_MIX_STEP).clamp(-100, 100)
    }
}

/// The Windows ID of the real output Sonar plays to: the Game channel's in Classic mode and the
/// monitoring mix's in Streamer mode, where it is empty until an output is set.
pub fn routed_output(redirections: &str, mode: Mode) -> Option<&str> {
    let marker = match mode {
        Mode::Classic => "\"game\"",
        Mode::Stream => "\"monitoring\"",
    };
    let (_, route) = redirections.split_once(marker)?;
    json_string(route, "deviceId").filter(|id| !id.is_empty())
}

fn json_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            control if u32::from(control) < 0x20 => {}
            other => escaped.push(other),
        }
    }
    escaped
}

/// The event GG core relays to Sonar so that its window redraws, in Streamer mode.
pub fn ui_action_message(action: &str, percent: i32) -> String {
    format!(
        "{{\"event\":\"EVENT_SUB_APP_ACTIONS_DATA_CHANGED\",\"data\":{{\"SubAppName\":\"sonar\",\
         \"Id\":\"00000000-0000-0000-0000-000000000000\",\"ActionName\":\"{action}\",\
         \"Value\":{percent}.0}}}}"
    )
}

pub fn oled_metadata() -> String {
    format!(
        "{{\"game\":\"{OLED_GAME}\",\"game_display_name\":\"sonar-rollerbar\",\
         \"developer\":\"sonar-rollerbar\"}}"
    )
}

/// Icon 16 is GameSense's built-in volume icon.
pub fn oled_binding() -> String {
    format!(
        "{{\"game\":\"{OLED_GAME}\",\"event\":\"{OLED_EVENT}\",\"value_optional\":true,\
         \"icon_id\":16,\"handlers\":[{{\"device-type\":\"screened\",\"mode\":\"screen\",\
         \"zone\":\"one\",\"datas\":[{{\"lines\":[\
         {{\"has-text\":true,\"context-frame-key\":\"line1\"}},\
         {{\"has-text\":true,\"context-frame-key\":\"line2\"}}]}}]}}]}}"
    )
}

pub fn oled_frame(line1: &str, line2: &str) -> String {
    format!(
        "{{\"game\":\"{OLED_GAME}\",\"event\":\"{OLED_EVENT}\",\"data\":{{\"frame\":\
         {{\"line1\":\"{}\",\"line2\":\"{}\"}}}}}}",
        json_escape(line1),
        json_escape(line2),
    )
}

pub fn oled_stop() -> String {
    format!("{{\"game\":\"{OLED_GAME}\"}}")
}

/// `[=====---]  67%`, or `[ MUTED  ]  67%`, which has the same width.
pub fn oled_level(percent: i32, muted: bool) -> String {
    let inner = if muted {
        format!("{:^OLED_BAR_WIDTH$}", "MUTED")
    } else {
        let width = OLED_BAR_WIDTH as i32;
        let filled = ((percent * width + 50) / 100).clamp(0, width) as usize;
        format!("{}{}", "=".repeat(filled), "-".repeat(OLED_BAR_WIDTH - filled))
    };
    format!("[{inner}] {percent:>3}%")
}

/// `G [--==|----] C` leans towards Game and `G [----|==--] C` towards Chat.
pub fn oled_balance(balance: i32) -> String {
    let width = OLED_BALANCE_WIDTH as i32;
    let filled = ((balance.abs() * width + 50) / 100).min(width) as usize;
    let (game, chat) = if balance < 0 { (filled, 0) } else { (0, filled) };
    format!(
        "G [{}{}|{}{}] C",
        "-".repeat(OLED_BALANCE_WIDTH - game),
        "=".repeat(game),
        "=".repeat(chat),
        "-".repeat(OLED_BALANCE_WIDTH - chat),
    )
}

/// `Speakers (Creative Stage Pro)` becomes `Creative Stage Pro`: the brackets name the device.
pub fn device_name(output: &str) -> &str {
    output
        .split_once('(')
        .and_then(|(_, rest)| rest.rsplit_once(')'))
        .map_or(output, |(inner, _)| inner)
        .trim()
}

pub fn oled_title(output: &str) -> String {
    device_name(output).chars().take(OLED_TITLE_WIDTH).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASSIC: &str = r#"{"masters":{"stream":{},"classic":{"volume":0.48,"muted":false}},
        "devices":{"game":{"stream":{},"classic":{"volume":1.0,"muted":false}},
        "chatRender":{"stream":{},"classic":{"volume":0.25,"muted":false}}}}"#;
    const STREAM: &str = r#"{"masters":{"stream":{"streaming":{"volume":0.9,"muted":false},
        "monitoring":{"volume":0.7,"muted":false}},"classic":{"volume":0.0,"muted":false}},
        "devices":{"game":{"stream":{"streaming":{"volume":0.1,"muted":false},
        "monitoring":{"volume":0.2,"muted":false}},"classic":{"volume":0.0,"muted":false}}}}"#;

    #[test]
    fn reads_each_classic_channel() {
        assert_eq!(channel_percent(CLASSIC, Mode::Classic, &CHANNELS[0]), Some(48));
        assert_eq!(channel_percent(CLASSIC, Mode::Classic, &CHANNELS[1]), Some(100));
        assert_eq!(channel_percent(CLASSIC, Mode::Classic, &CHANNELS[2]), Some(25));
    }

    #[test]
    fn reads_the_monitoring_slider_in_streamer_mode() {
        assert_eq!(channel_percent(STREAM, Mode::Stream, &CHANNELS[0]), Some(70));
        assert_eq!(channel_percent(STREAM, Mode::Stream, &CHANNELS[1]), Some(20));
    }

    #[test]
    fn a_missing_channel_gives_nothing() {
        assert_eq!(channel_percent(CLASSIC, Mode::Classic, &CHANNELS[3]), None);
    }

    #[test]
    fn rejects_out_of_range_volumes() {
        let volumes = r#"{"masters":{"classic":{"volume":7.0,"x":1}}}"#;
        assert_eq!(channel_percent(volumes, Mode::Classic, &CHANNELS[0]), None);
    }

    #[test]
    fn reads_small_exponent_values() {
        let volumes = r#"{"masters":{"classic":{"volume":1E-05,"x":1}}}"#;
        assert_eq!(channel_percent(volumes, Mode::Classic, &CHANNELS[0]), Some(0));
    }

    #[test]
    fn builds_volume_paths() {
        assert_eq!(
            Mode::Classic.set_volume_path(&CHANNELS[0], 48),
            "/volumeSettings/classic/master/Volume/0.48"
        );
        assert_eq!(
            Mode::Stream.set_volume_path(&CHANNELS[1], 100),
            "/volumeSettings/streamer/monitoring/game/Volume/1.00"
        );
    }

    #[test]
    fn finds_the_sonar_port_among_other_apps() {
        let apps = r#"{"subApps":{"engine":{"metadata":{"webServerAddress":"http://127.0.0.1:1"}},
            "sonar":{"isReady":true,"metadata":{"webServerAddress":"http://127.0.0.1:8150"}}}}"#;
        assert_eq!(sonar_port(apps), Some(8150));
    }

    #[test]
    fn only_accepts_loopback_addresses() {
        assert_eq!(loopback_port("127.0.0.1:6327"), Some(6327));
        assert_eq!(loopback_port("http://127.0.0.1:8150/"), Some(8150));
        assert_eq!(loopback_port("http://192.168.1.2:8150"), None);
        assert_eq!(loopback_port("http://example.com:8150"), None);
        assert_eq!(loopback_port("127.0.0.1.example.com:80"), None);
    }

    #[test]
    fn reads_the_mode() {
        assert_eq!(Mode::parse("\"classic\"\n"), Some(Mode::Classic));
        assert_eq!(Mode::parse("\"stream\""), Some(Mode::Stream));
        assert_eq!(Mode::parse("\"other\""), None);
    }

    #[test]
    fn reads_a_channel_level_with_its_mute() {
        let volumes = r#"{"masters":{"stream":{},"classic":{"volume":0.16,"muted":false}},
            "devices":{"game":{"stream":{},"classic":{"volume":0.25,"muted":true}}}}"#;
        let level = |index: usize| channel_level(volumes, Mode::Classic, &CHANNELS[index]);
        assert_eq!(level(0), Some(ChannelLevel { percent: 16, muted: false }));
        assert_eq!(level(1), Some(ChannelLevel { percent: 25, muted: true }));
        assert_eq!(level(2), None);
    }

    #[test]
    fn builds_mute_paths() {
        assert_eq!(
            Mode::Classic.set_mute_path(&CHANNELS[1], true),
            "/volumeSettings/classic/game/Mute/true"
        );
        assert_eq!(
            Mode::Stream.set_mute_path(&CHANNELS[1], false),
            "/volumeSettings/streamer/monitoring/game/isMuted/false"
        );
    }

    #[test]
    fn reads_chat_mix() {
        let body = r#"{"balance":-0.25,"state":"enabled","id":"00000000-0000"}"#;
        let chat_mix = ChatMix::parse(body).unwrap();
        assert_eq!(chat_mix.balance, -25);
        assert!(chat_mix.is_available());
    }

    #[test]
    fn chat_mix_can_be_refused() {
        let state = |state: &str| {
            let body = format!(r#"{{"balance":0.0,"state":"{state}"}}"#);
            ChatMix::parse(&body).unwrap().state
        };
        assert_eq!(state("finiteWheel"), ChatMixState::HardwareDial);
        assert_eq!(state("differentDeviceSelected"), ChatMixState::DifferentOutputs);
        assert_eq!(state("noDeviceSelected"), ChatMixState::Unavailable);
        assert!(ChatMix::parse(r#"{"balance":3.0,"state":"enabled"}"#).is_none());
    }

    #[test]
    fn builds_the_chat_mix_path() {
        assert_eq!(ChatMix::write_path(-25), "/v1/chatMix?balance=-0.25");
        assert_eq!(ChatMix::write_path(100), "/v1/chatMix?balance=1.00");
    }

    #[test]
    fn turning_stops_at_each_end_of_chat_mix() {
        assert_eq!(ChatMix::turned(0, 2), 10);
        assert_eq!(ChatMix::turned(0, -2), -10);
        assert_eq!(ChatMix::turned(95, 3), 100);
        assert_eq!(ChatMix::turned(-95, -3), -100);
    }

    fn wheel_cycle(chat_mix_available: bool) -> Vec<Control> {
        let mut seen = vec![Control::Output.next(chat_mix_available)];
        while seen[seen.len() - 1] != Control::Output {
            seen.push(seen[seen.len() - 1].next(chat_mix_available));
        }
        seen
    }

    #[test]
    fn the_wheel_cycles_through_its_targets() {
        assert_eq!(
            wheel_cycle(true),
            [
                Control::ChatMix,
                Control::Channel(1),
                Control::Channel(2),
                Control::Channel(3),
                Control::Channel(4),
                Control::Output,
            ]
        );
    }

    #[test]
    fn the_wheel_skips_an_unavailable_chat_mix() {
        assert_eq!(Control::Output.next(false), Control::Channel(1));
        assert_eq!(Control::ChatMix.next(false), Control::Channel(1));
        assert_eq!(Control::Channel(4).next(false), Control::Output);
    }

    #[test]
    fn formats_the_oled_balance() {
        assert_eq!(oled_balance(0), "G [----|----] C");
        assert_eq!(oled_balance(-50), "G [--==|----] C");
        assert_eq!(oled_balance(100), "G [----|====] C");
    }

    #[test]
    fn finds_the_output_of_the_game_channel() {
        let routes = r#"[{"id":"aux","deviceId":"{0.0.0.00000000}.{aux}","isRunning":true},
            {"id":"game","deviceId":"{0.0.0.00000000}.{game}","isRunning":true},
            {"id":"mic","deviceId":"{0.0.1.00000000}.{mic}","isRunning":true}]"#;
        assert_eq!(routed_output(routes, Mode::Classic), Some("{0.0.0.00000000}.{game}"));
    }

    #[test]
    fn finds_the_output_of_the_monitoring_mix() {
        let routes = r#"[{"streamRedirectionId":"streaming","deviceId":"{streaming}","status":[]},
            {"streamRedirectionId":"monitoring","deviceId":"{monitoring}","status":
            [{"role":"game","isEnabled":true}],"isRunning":true}]"#;
        assert_eq!(routed_output(routes, Mode::Stream), Some("{monitoring}"));
    }

    #[test]
    fn an_empty_route_gives_nothing() {
        let routes = r#"[{"streamRedirectionId":"monitoring","deviceId":"","status":[]}]"#;
        assert_eq!(routed_output(routes, Mode::Stream), None);
        assert_eq!(routed_output("[]", Mode::Classic), None);
    }

    #[test]
    fn formats_the_oled_level() {
        assert_eq!(oled_level(100, false), "[========] 100%");
        assert_eq!(oled_level(0, false), "[--------]   0%");
        assert_eq!(oled_level(50, false), "[====----]  50%");
        assert_eq!(oled_level(67, true), "[ MUTED  ]  67%");
    }

    #[test]
    fn shortens_the_oled_title() {
        assert_eq!(oled_title("Speakers (Creative Stage Pro)"), "Creative Stage P");
        assert_eq!(oled_title("Headset (Arctis (USB))"), "Arctis (USB)");
        assert_eq!(oled_title("Plain name"), "Plain name");
    }

    #[test]
    fn escapes_oled_text() {
        assert_eq!(
            oled_frame("a\"b\\c\n", "x"),
            r#"{"game":"SONAR_ROLLERBAR","event":"VOLUME","data":{"frame":{"line1":"a\"b\\c","line2":"x"}}}"#
        );
    }

    #[test]
    fn builds_the_ui_action_message() {
        let message = ui_action_message("MONITOR_GAME_VOLUME", 48);
        assert!(message.contains("\"ActionName\":\"MONITOR_GAME_VOLUME\""));
        assert!(message.ends_with("\"Value\":48.0}}"));
    }
}
