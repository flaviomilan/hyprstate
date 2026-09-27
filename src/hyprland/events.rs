//! Event socket (`.socket2.sock`): newline-delimited `EVENT>>DATA`.

use std::io::{BufRead, BufReader, ErrorKind};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use super::ipc::instance_dir;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `openwindow>>ADDRESS,WORKSPACENAME,CLASS,TITLE` (address without 0x)
    OpenWindow { address: String, workspace: String, class: String, title: String },
    CloseWindow { address: String },
    Other { name: String, data: String },
}

pub fn parse_event(line: &str) -> Option<Event> {
    let (name, data) = line.split_once(">>")?;
    Some(match name {
        "openwindow" => {
            let mut parts = data.splitn(4, ',');
            Event::OpenWindow {
                address: normalize_address(parts.next()?),
                workspace: parts.next()?.to_string(),
                class: parts.next()?.to_string(),
                title: parts.next().unwrap_or("").to_string(),
            }
        }
        "closewindow" => Event::CloseWindow { address: normalize_address(data) },
        _ => Event::Other { name: name.to_string(), data: data.to_string() },
    })
}

/// Hyprland events omit the `0x` that `j/clients` includes.
pub fn normalize_address(a: &str) -> String {
    let a = a.trim();
    if a.starts_with("0x") { a.to_string() } else { format!("0x{a}") }
}

pub trait EventSource {
    /// Next event, or `None` once `deadline` has passed.
    fn next_before(&mut self, deadline: Instant) -> Result<Option<Event>>;
}

pub struct HyprEvents {
    reader: BufReader<UnixStream>,
    line: String,
}

impl HyprEvents {
    pub fn connect() -> Result<Self> {
        let path = instance_dir()?.join(".socket2.sock");
        let stream = UnixStream::connect(&path)
            .with_context(|| format!("connecting to {}", path.display()))?;
        Ok(Self { reader: BufReader::new(stream), line: String::new() })
    }
}

impl EventSource for HyprEvents {
    fn next_before(&mut self, deadline: Instant) -> Result<Option<Event>> {
        loop {
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let wait = (deadline - now).max(Duration::from_millis(1));
            self.reader.get_ref().set_read_timeout(Some(wait))?;
            // A timed-out read may leave a partial line in `self.line`; we keep
            // it and let the next read_line append the rest.
            match self.reader.read_line(&mut self.line) {
                Ok(0) => anyhow::bail!("Hyprland event socket closed"),
                Ok(_) if self.line.ends_with('\n') => {
                    let line = std::mem::take(&mut self.line);
                    if let Some(ev) = parse_event(line.trim_end()) {
                        return Ok(Some(ev));
                    }
                }
                Ok(_) => {}
                Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openwindow_with_commas_in_title() {
        let ev = parse_event("openwindow>>58e4d0140850,2,foot,a, b, c").unwrap();
        assert_eq!(
            ev,
            Event::OpenWindow {
                address: "0x58e4d0140850".into(),
                workspace: "2".into(),
                class: "foot".into(),
                title: "a, b, c".into(),
            }
        );
        assert_eq!(
            parse_event("closewindow>>abc").unwrap(),
            Event::CloseWindow { address: "0xabc".into() }
        );
        assert!(parse_event("garbage").is_none());
    }
}
