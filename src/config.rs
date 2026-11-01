//! Options that can change after installation, stored as a small INI file that the installer and
//! the tray menu both edit. Output choices stay on the command line.
//! Tests run anywhere with `rustc --edition 2024 --test src/config.rs`.

use std::fs;
use std::io;
use std::path::Path;

const STEP_RANGE: std::ops::RangeInclusive<i32> = 1..=100;

#[derive(Debug, PartialEq)]
pub struct Options {
    pub oled: bool,
    pub sonar_mixer: bool,
    pub follow_output: bool,
    pub wheel_select: bool,
    pub wheel_master: bool,
    /// Wheel step in percent, for each output that has its own.
    pub steps: Vec<(String, i32)>,
}

impl Options {
    pub const fn new() -> Self {
        Self {
            oled: false,
            sonar_mixer: false,
            follow_output: false,
            wheel_select: false,
            wheel_master: false,
            steps: Vec::new(),
        }
    }

    pub fn parse(text: &str) -> Self {
        let mut options = Self::new();
        let mut in_steps = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') && line.ends_with(']') {
                in_steps = line == "[steps]";
            } else if let Some((key, value)) = line.rsplit_once('=') {
                let (key, value) = (key.trim(), value.trim());
                if in_steps {
                    if let Some(step) = value.parse().ok().filter(|step| STEP_RANGE.contains(step)) {
                        options.set_step(key, step);
                    }
                } else {
                    options.set_flag(key, value == "1");
                }
            }
        }
        options
    }

    fn set_flag(&mut self, key: &str, enabled: bool) {
        match key {
            "oled" => self.oled = enabled,
            "sonar_mixer" => self.sonar_mixer = enabled,
            "follow_output" => self.follow_output = enabled,
            "wheel_select" => self.wheel_select = enabled,
            "wheel_master" => self.wheel_master = enabled,
            _ => {}
        }
    }

    pub fn render(&self) -> String {
        let flag = u8::from;
        let mut text = format!(
            "[options]\noled={}\nsonar_mixer={}\nfollow_output={}\nwheel_select={}\n\
             wheel_master={}\n\n[steps]\n",
            flag(self.oled),
            flag(self.sonar_mixer),
            flag(self.follow_output),
            flag(self.wheel_select),
            flag(self.wheel_master),
        );
        for (output, step) in &self.steps {
            text.push_str(&format!("{output}={step}\n"));
        }
        text
    }

    pub fn step_for(&self, output: &str) -> Option<i32> {
        self.steps
            .iter()
            .find(|(name, _)| name == output)
            .map(|(_, step)| *step)
    }

    pub fn set_step(&mut self, output: &str, step: i32) {
        match self.steps.iter_mut().find(|(name, _)| name == output) {
            Some((_, current)) => *current = step,
            None => self.steps.push((output.to_owned(), step)),
        }
    }

    /// A missing or unreadable file gives the defaults, with every option off.
    pub fn load(path: &Path) -> Self {
        fs::read(path)
            .map(|bytes| Self::parse(&String::from_utf8_lossy(&bytes)))
            .unwrap_or_else(|_| Self::new())
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(directory) = path.parent() {
            fs::create_dir_all(directory)?;
        }
        fs::write(path, self.render())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_option_is_off_by_default() {
        let options = Options::parse("");
        assert!(!options.oled && !options.sonar_mixer && !options.follow_output);
        assert!(!options.wheel_select && !options.wheel_master);
    }

    #[test]
    fn reads_the_installer_format() {
        let text = "[options]\r\noled=1\r\nsonar_mixer=0\r\nfollow_output=1\r\nwheel_select=1\r\n\
                    wheel_master=1\r\n";
        let options = Options::parse(text);
        assert!(options.oled && !options.sonar_mixer && options.follow_output);
        assert!(options.wheel_select && options.wheel_master);
    }

    #[test]
    fn keeps_a_step_for_each_output() {
        let mut options = Options::new();
        options.set_step("Speakers (Bar = 2)", 5);
        options.set_step("Headset", 10);
        options.set_step("Headset", 1);

        let reloaded = Options::parse(&options.render());
        assert_eq!(reloaded.step_for("Speakers (Bar = 2)"), Some(5));
        assert_eq!(reloaded.step_for("Headset"), Some(1));
        assert_eq!(reloaded.step_for("Other"), None);
    }

    #[test]
    fn ignores_invalid_values() {
        let options = Options::parse("[steps]\nA=0\nB=101\nC=x\nD=7\n[options]\nunknown=1\n");
        assert_eq!(options.steps, vec![("D".to_owned(), 7)]);
    }

    #[test]
    fn round_trips_through_text() {
        let mut options = Options::new();
        options.oled = true;
        options.wheel_master = true;
        options.set_step("Headset", 5);
        assert_eq!(Options::parse(&options.render()), options);
    }
}
