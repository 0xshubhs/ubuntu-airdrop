//! "Stay visible for": the receive window.
//!
//! Taken from how AirDrop works, and from omdrop, which ports it to Linux: a
//! device is not permanently open to the network, it becomes visible for a
//! while and then goes quiet on its own. Drop used to be the other way
//! around, reachable for as long as the daemon ran, so `Span::Always` keeps
//! that available and is still the default.
//!
//! The window governs arrivals only: signing in, offering, uploading, pasting.
//! A phone that is already past the PIN can still collect what this machine
//! put up for it, because that is this side's doing, not the network's.

use std::fmt;

/// What "on" means when a window is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    /// No window: receiving stays on while the daemon runs.
    Always,
    /// Open for this many seconds, then close.
    For(u64),
    /// Close as soon as one transfer finishes.
    Once,
}

/// Why receiving is off, in the words the tray and the panel use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shut {
    /// Nobody has turned it on yet, or someone turned it off.
    Off,
    /// The window ran out.
    Expired,
    /// A `Once` window closed behind the file it was opened for.
    OneFile,
}

impl Shut {
    pub fn say(&self) -> &'static str {
        match self {
            Shut::Off => "off",
            Shut::Expired => "the window ran out",
            Shut::OneFile => "one file arrived",
        }
    }
}

impl Span {
    /// Parse what the CLI, the panel and the config file all write.
    ///
    /// A bare number is minutes, so `drop on 15` reads the way it sounds.
    pub fn parse(spec: &str) -> Option<Span> {
        let s = spec.trim().to_ascii_lowercase();
        match s.as_str() {
            "always" | "on" | "forever" => return Some(Span::Always),
            "once" | "one" | "1 file" => return Some(Span::Once),
            "" => return None,
            _ => {}
        }
        let (digits, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
        let n: u64 = digits.parse().ok()?;
        let secs = match unit.trim() {
            "" | "m" | "min" | "mins" | "minute" | "minutes" => n * 60,
            "s" | "sec" | "secs" | "second" | "seconds" => n,
            "h" | "hr" | "hrs" | "hour" | "hours" => n * 3600,
            _ => return None,
        };
        (secs > 0).then_some(Span::For(secs))
    }

    /// The form `parse` reads back, which is what the config file stores.
    pub fn spec(&self) -> String {
        match self {
            Span::Always => "always".into(),
            Span::Once => "once".into(),
            Span::For(s) if s % 3600 == 0 => format!("{}h", s / 3600),
            Span::For(s) if s % 60 == 0 => format!("{}m", s / 60),
            Span::For(s) => format!("{s}s"),
        }
    }
}

impl fmt::Display for Span {
    /// How the choice is named in a menu: "15 minutes", "one file".
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Span::Always => write!(f, "until I turn it off"),
            Span::Once => write!(f, "one file"),
            Span::For(s) if *s % 3600 == 0 && *s > 3600 => write!(f, "{} hours", s / 3600),
            Span::For(s) if *s == 3600 => write!(f, "1 hour"),
            Span::For(s) if *s % 60 == 0 => write!(f, "{} minutes", s / 60),
            Span::For(s) => write!(f, "{s} seconds"),
        }
    }
}

/// The choices the tray and the panel offer, in the order they show them.
pub const CHOICES: [Span; 5] = [
    Span::For(5 * 60),
    Span::For(15 * 60),
    Span::For(60 * 60),
    Span::Once,
    Span::Always,
];

#[derive(Debug, Clone)]
pub struct Window {
    /// What opening a window means, and what the config file remembers.
    span: Span,
    open: bool,
    /// Epoch second the window closes at. `None` while open without a clock.
    until: Option<u64>,
    /// Close behind the next arrival.
    once: bool,
    shut: Shut,
}

impl Window {
    /// A window at rest. `Always` is already receiving: that preference means
    /// there is no window to open.
    pub fn new(span: Span) -> Window {
        let mut w = Window {
            span,
            open: false,
            until: None,
            once: false,
            shut: Shut::Off,
        };
        if span == Span::Always {
            w.open_for(Span::Always, 0);
        }
        w
    }

    pub fn span(&self) -> Span {
        self.span
    }

    /// Open a window, either as `span` says or as the stored preference does.
    pub fn open(&mut self, span: Option<Span>, now: u64) {
        self.open_for(span.unwrap_or(self.span), now);
    }

    fn open_for(&mut self, span: Span, now: u64) {
        self.open = true;
        self.shut = Shut::Off;
        self.once = span == Span::Once;
        self.until = match span {
            Span::For(secs) => Some(now + secs),
            Span::Always | Span::Once => None,
        };
    }

    pub fn close(&mut self, why: Shut) {
        self.open = false;
        self.once = false;
        self.until = None;
        self.shut = why;
    }

    /// Change what "on" means. A live window takes the new span at once, so
    /// picking "5 minutes" while visible shortens the window in front of you
    /// rather than waiting for the next one.
    pub fn set_span(&mut self, span: Span, now: u64) {
        self.span = span;
        // `Always` has no window to wait for, and a window already up takes
        // the new span straight away. A shut one stays shut: a preference is
        // not the same as turning it on.
        if span == Span::Always || self.open {
            self.open_for(span, now);
        }
    }

    /// Is this machine taking arrivals right now?
    ///
    /// Expiry is decided here as well as by the sweeper, so a window is never
    /// open just because nothing has looked at the clock lately.
    pub fn receiving(&self, now: u64) -> bool {
        self.open && !self.past(now)
    }

    fn past(&self, now: u64) -> bool {
        self.until.is_some_and(|until| now >= until)
    }

    /// Seconds left, when the window is on a clock.
    pub fn remaining(&self, now: u64) -> Option<u64> {
        let until = self.until?;
        self.open.then(|| until.saturating_sub(now))
    }

    pub fn shut(&self) -> Shut {
        self.shut
    }

    /// Will this window close behind the next arrival? Asked by the tray and
    /// the panel, which cannot tell from the stored preference: `drop on
    /// once` opens a one-file window without changing it.
    pub fn once(&self) -> bool {
        self.open && self.once
    }

    /// Close an expired window. True the once, so the caller can say so.
    pub fn sweep(&mut self, now: u64) -> bool {
        if self.open && self.past(now) {
            self.close(Shut::Expired);
            return true;
        }
        false
    }

    /// Called after something finishes arriving. True if that was the file a
    /// `Once` window was held open for.
    pub fn arrived(&mut self) -> bool {
        if self.open && self.once {
            self.close(Shut::OneFile);
            return true;
        }
        false
    }
}

/// A countdown for a menu label: `4:09`, or `1:02:40` past the hour.
pub fn countdown(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_specs_people_type() {
        assert_eq!(Span::parse("always"), Some(Span::Always));
        assert_eq!(Span::parse("once"), Some(Span::Once));
        assert_eq!(Span::parse("15"), Some(Span::For(900)));
        assert_eq!(Span::parse("15m"), Some(Span::For(900)));
        assert_eq!(Span::parse(" 90s "), Some(Span::For(90)));
        assert_eq!(Span::parse("2h"), Some(Span::For(7200)));
        assert_eq!(Span::parse("ALWAYS"), Some(Span::Always));
        assert_eq!(Span::parse("0m"), None);
        assert_eq!(Span::parse("soon"), None);
        assert_eq!(Span::parse(""), None);
    }

    #[test]
    fn specs_round_trip() {
        for span in CHOICES {
            assert_eq!(Span::parse(&span.spec()), Some(span), "{}", span.spec());
        }
    }

    #[test]
    fn always_is_open_from_the_start() {
        let w = Window::new(Span::Always);
        assert!(w.receiving(0));
        assert_eq!(w.remaining(0), None);
    }

    #[test]
    fn a_timed_window_is_shut_until_opened() {
        let mut w = Window::new(Span::For(60));
        assert!(!w.receiving(100));
        assert_eq!(w.shut(), Shut::Off);

        w.open(None, 100);
        assert!(w.receiving(100));
        assert_eq!(w.remaining(130), Some(30));
        assert!(w.receiving(159));
        assert!(!w.receiving(160), "closed on the second it runs out");
    }

    #[test]
    fn sweeping_an_expired_window_reports_it_once() {
        let mut w = Window::new(Span::For(60));
        w.open(None, 0);
        assert!(!w.sweep(59));
        assert!(w.sweep(60));
        assert_eq!(w.shut(), Shut::Expired);
        assert!(!w.sweep(61), "already closed");
        assert_eq!(w.remaining(61), None);
    }

    #[test]
    fn once_closes_behind_the_first_arrival() {
        let mut w = Window::new(Span::Once);
        w.open(None, 0);
        assert!(w.receiving(0));
        assert!(w.arrived());
        assert!(!w.receiving(0));
        assert_eq!(w.shut(), Shut::OneFile);
        assert!(!w.arrived(), "nothing left to close");
    }

    #[test]
    fn a_one_file_window_says_so_while_it_is_up() {
        let mut w = Window::new(Span::For(60));
        w.open(Some(Span::Once), 0);
        assert!(w.once(), "opened as one-file, whatever the preference says");
        assert_eq!(w.span(), Span::For(60), "the preference is untouched");
        w.arrived();
        assert!(!w.once());
    }

    #[test]
    fn a_timed_window_survives_an_arrival() {
        let mut w = Window::new(Span::For(60));
        w.open(None, 0);
        assert!(!w.arrived());
        assert!(w.receiving(1));
    }

    #[test]
    fn always_keeps_receiving_through_an_arrival() {
        let mut w = Window::new(Span::Always);
        assert!(!w.arrived());
        assert!(w.receiving(10_000));
    }

    #[test]
    fn changing_the_span_moves_the_live_window() {
        let mut w = Window::new(Span::For(3600));
        w.open(None, 0);
        w.set_span(Span::For(60), 10);
        assert_eq!(w.span(), Span::For(60));
        assert_eq!(w.remaining(10), Some(60));
        assert!(!w.receiving(70));
    }

    #[test]
    fn switching_to_always_opens_a_shut_window() {
        let mut w = Window::new(Span::For(60));
        assert!(!w.receiving(0));
        w.set_span(Span::Always, 0);
        assert!(w.receiving(10_000));
    }

    #[test]
    fn switching_away_from_always_leaves_a_shut_window_shut() {
        let mut w = Window::new(Span::For(60));
        w.close(Shut::Off);
        w.set_span(Span::Once, 5);
        assert!(!w.receiving(5), "a preference is not a window");
    }

    #[test]
    fn counts_down_in_minutes_then_hours() {
        assert_eq!(countdown(0), "0:00");
        assert_eq!(countdown(9), "0:09");
        assert_eq!(countdown(249), "4:09");
        assert_eq!(countdown(3760), "1:02:40");
    }
}
