// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Glyphvane contributors

//! Dates and durations of the expression language.
//!
//! A [`Date`] is a local wall-clock time in milliseconds since
//! 1970-01-01 00:00 (proleptic Gregorian calendar, no time zone): the
//! caller converts file times with its UTC offset
//! ([`crate::EvalContext`]). A [`Duration`] has a calendar part (months)
//! and an exact part (milliseconds), so `"1M"` adds a calendar month.

use std::cmp::Ordering;
use std::fmt::Write as _;

const MS_PER_SECOND: i64 = 1_000;
const MS_PER_MINUTE: i64 = 60 * MS_PER_SECOND;
const MS_PER_HOUR: i64 = 60 * MS_PER_MINUTE;
const MS_PER_DAY: i64 = 24 * MS_PER_HOUR;

/// A local date, with or without a time of day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Date {
    /// Milliseconds since 1970-01-01 00:00 local time.
    pub ms: i64,
    /// Whether the value has a time of day (`now()`, file times) or is a
    /// whole day (`today()`, `date("2025-01-01")`). Only display uses it.
    pub has_time: bool,
}

/// Broken-down fields of a [`Date`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    /// Year.
    pub year: i64,
    /// 1–12.
    pub month: u32,
    /// 1–31.
    pub day: u32,
    /// 0–23.
    pub hour: u32,
    /// 0–59.
    pub minute: u32,
    /// 0–59.
    pub second: u32,
    /// 0–999.
    pub millisecond: u32,
    /// 0 = Sunday … 6 = Saturday.
    pub weekday: u32,
}

/// Days since 1970-01-01 of a civil date (H. Hinnant's algorithm).
#[must_use]
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The civil date of a day number.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Date {
    /// A date from civil fields (already validated).
    #[must_use]
    pub fn from_civil(
        year: i64,
        month: u32,
        day: u32,
        hour: u32,
        minute: u32,
        second: u32,
        ms: u32,
    ) -> Self {
        let days = days_from_civil(year, month, day);
        Self {
            ms: days * MS_PER_DAY
                + i64::from(hour) * MS_PER_HOUR
                + i64::from(minute) * MS_PER_MINUTE
                + i64::from(second) * MS_PER_SECOND
                + i64::from(ms),
            has_time: true,
        }
    }

    /// The fields.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn civil(self) -> Civil {
        let days = self.ms.div_euclid(MS_PER_DAY);
        let rest = self.ms.rem_euclid(MS_PER_DAY);
        let (year, month, day) = civil_from_days(days);
        Civil {
            year,
            month,
            day,
            hour: (rest / MS_PER_HOUR) as u32,
            minute: (rest % MS_PER_HOUR / MS_PER_MINUTE) as u32,
            second: (rest % MS_PER_MINUTE / MS_PER_SECOND) as u32,
            millisecond: (rest % MS_PER_SECOND) as u32,
            weekday: (days + 4).rem_euclid(7) as u32,
        }
    }

    /// Midnight of the same day, without a time of day.
    #[must_use]
    pub fn day_start(self) -> Self {
        Self {
            ms: self.ms - self.ms.rem_euclid(MS_PER_DAY),
            has_time: false,
        }
    }

    /// `self + d`: calendar months first (clamping the day to the month's
    /// length, as Moment.js does), then the exact part.
    #[must_use]
    pub fn shifted(self, d: Duration) -> Self {
        let mut ms = self.ms;
        if d.months != 0 {
            let c = self.civil();
            let total = c.year * 12 + i64::from(c.month - 1) + d.months;
            let year = total.div_euclid(12);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let month = total.rem_euclid(12) as u32 + 1;
            let day = c.day.min(days_in_month(year, month));
            ms = Self::from_civil(year, month, day, c.hour, c.minute, c.second, c.millisecond).ms;
        }
        Self {
            ms: ms.saturating_add(d.ms),
            has_time: self.has_time || d.ms % MS_PER_DAY != 0,
        }
    }

    /// Parses `YYYY-MM-DD`, optionally followed by `T` or a space and
    /// `HH:mm`, `HH:mm:ss` or `HH:mm:ss.SSS`, optionally followed by `Z`
    /// or `±HH:MM` (then converted to local time with
    /// `local_offset_minutes`).
    #[must_use]
    pub fn parse(text: &str, local_offset_minutes: i32) -> Option<Self> {
        let s = text.trim();
        let b = s.as_bytes();
        if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        let year: i64 = digits(&s[0..4])?.into();
        let month = digits(&s[5..7])?;
        let day = digits(&s[8..10])?;
        if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
            return None;
        }
        let rest = &s[10..];
        if rest.is_empty() {
            let mut d = Self::from_civil(year, month, day, 0, 0, 0, 0);
            d.has_time = false;
            return Some(d);
        }
        let rest = rest.strip_prefix(['T', ' '])?;
        let (time, zone) = split_zone(rest);
        let mut parts = time.split(':');
        let hour = digits(parts.next()?)?;
        let minute = digits(parts.next()?)?;
        let (second, milli) = match parts.next() {
            None => (0, 0),
            Some(sec) => match sec.split_once('.') {
                None => (digits(sec)?, 0),
                Some((whole, frac)) => {
                    if frac.is_empty() || !frac.bytes().all(|c| c.is_ascii_digit()) {
                        return None;
                    }
                    let padded: String = frac.chars().chain("000".chars()).take(3).collect();
                    (digits(whole)?, digits(&padded)?)
                }
            },
        };
        if parts.next().is_some() || hour > 23 || minute > 59 || second > 59 {
            return None;
        }
        let mut d = Self::from_civil(year, month, day, hour, minute, second, milli);
        if let Some(zone) = zone {
            let offset = parse_zone(zone)?;
            d.ms += i64::from(local_offset_minutes - offset) * MS_PER_MINUTE;
        }
        Some(d)
    }

    /// Formats with a Moment.js format string. Supported tokens: `YYYY`
    /// `YY` `Q` `MMMM` `MMM` `MM` `M` `Do` `DD` `D` `DDDD` `dddd` `ddd`
    /// `dd` `d` `HH` `H` `hh` `h` `mm` `m` `ss` `s` `SSS` `A` `a` `X` `x`
    /// `W` `WW`, and `[literal text]`. Other letters are copied.
    #[must_use]
    pub fn format(self, pattern: &str) -> String {
        const TOKENS: &[&str] = &[
            "YYYY", "MMMM", "DDDD", "dddd", "SSS", "MMM", "ddd", "YY", "MM", "Do", "DD", "dd",
            "HH", "hh", "mm", "ss", "WW", "M", "D", "d", "H", "h", "m", "s", "A", "a", "Q", "X",
            "x", "W",
        ];
        let c = self.civil();
        let mut out = String::new();
        let mut rest = pattern;
        'outer: while let Some(ch) = rest.chars().next() {
            if ch == '['
                && let Some(end) = rest.find(']')
            {
                out.push_str(&rest[1..end]);
                rest = &rest[end + 1..];
                continue;
            }
            for token in TOKENS {
                if let Some(after) = rest.strip_prefix(token) {
                    self.push_token(&mut out, token, &c);
                    rest = after;
                    continue 'outer;
                }
            }
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
        out
    }

    fn push_token(self, out: &mut String, token: &str, c: &Civil) {
        const MONTHS: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        const DAYS: [&str; 7] = [
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ];
        let month_name = MONTHS[(c.month - 1) as usize];
        let day_name = DAYS[c.weekday as usize];
        let h12 = match c.hour % 12 {
            0 => 12,
            h => h,
        };
        let _ = match token {
            "YYYY" => write!(out, "{:04}", c.year),
            "YY" => write!(out, "{:02}", c.year.rem_euclid(100)),
            "Q" => write!(out, "{}", (c.month - 1) / 3 + 1),
            "MMMM" => write!(out, "{month_name}"),
            "MMM" => write!(out, "{}", &month_name[..3]),
            "MM" => write!(out, "{:02}", c.month),
            "M" => write!(out, "{}", c.month),
            "Do" => write!(out, "{}{}", c.day, ordinal_suffix(c.day)),
            "DD" => write!(out, "{:02}", c.day),
            "D" => write!(out, "{}", c.day),
            "DDDD" => write!(out, "{:03}", self.day_of_year()),
            "dddd" => write!(out, "{day_name}"),
            "ddd" => write!(out, "{}", &day_name[..3]),
            "dd" => write!(out, "{}", &day_name[..2]),
            "d" => write!(out, "{}", c.weekday),
            "HH" => write!(out, "{:02}", c.hour),
            "H" => write!(out, "{}", c.hour),
            "hh" => write!(out, "{h12:02}"),
            "h" => write!(out, "{h12}"),
            "mm" => write!(out, "{:02}", c.minute),
            "m" => write!(out, "{}", c.minute),
            "ss" => write!(out, "{:02}", c.second),
            "s" => write!(out, "{}", c.second),
            "SSS" => write!(out, "{:03}", c.millisecond),
            "A" => write!(out, "{}", if c.hour < 12 { "AM" } else { "PM" }),
            "a" => write!(out, "{}", if c.hour < 12 { "am" } else { "pm" }),
            "X" => write!(out, "{}", self.ms.div_euclid(MS_PER_SECOND)),
            "x" => write!(out, "{}", self.ms),
            "W" => write!(out, "{}", self.iso_week()),
            "WW" => write!(out, "{:02}", self.iso_week()),
            _ => Ok(()),
        };
    }

    fn day_of_year(self) -> i64 {
        let c = self.civil();
        self.ms.div_euclid(MS_PER_DAY) - days_from_civil(c.year, 1, 1) + 1
    }

    fn iso_week(self) -> i64 {
        let days = self.ms.div_euclid(MS_PER_DAY);
        // Monday = 0.
        let weekday = (days + 3).rem_euclid(7);
        let thursday = days - weekday + 3;
        let (year, _, _) = civil_from_days(thursday);
        (thursday - days_from_civil(year, 1, 1)) / 7 + 1
    }

    /// The default display: `YYYY-MM-DD`, with ` HH:mm` when the value
    /// has a time of day.
    #[must_use]
    pub fn display(self) -> String {
        if self.has_time {
            self.format("YYYY-MM-DD HH:mm")
        } else {
            self.format("YYYY-MM-DD")
        }
    }

    /// The time of day, `HH:mm:ss`.
    #[must_use]
    pub fn time_text(self) -> String {
        self.format("HH:mm:ss")
    }

    /// "3 days ago", "in 2 hours", with Moment.js's thresholds.
    #[must_use]
    pub fn relative(self, now: Date) -> String {
        let diff = self.ms - now.ms;
        let future = diff > 0;
        let secs = (diff.abs() as f64) / 1000.0;
        let mins = secs / 60.0;
        let hours = mins / 60.0;
        let days = hours / 24.0;
        let phrase = if secs < 45.0 {
            "a few seconds".to_owned()
        } else if secs < 90.0 {
            "a minute".to_owned()
        } else if mins < 45.0 {
            format!("{} minutes", mins.round())
        } else if mins < 90.0 {
            "an hour".to_owned()
        } else if hours < 22.0 {
            format!("{} hours", hours.round())
        } else if hours < 36.0 {
            "a day".to_owned()
        } else if days < 26.0 {
            format!("{} days", days.round())
        } else if days < 45.0 {
            "a month".to_owned()
        } else if days < 320.0 {
            format!("{} months", (days / 30.4375).round())
        } else if days < 548.0 {
            "a year".to_owned()
        } else {
            format!("{} years", (days / 365.25).round())
        };
        if future {
            format!("in {phrase}")
        } else {
            format!("{phrase} ago")
        }
    }
}

impl PartialOrd for Date {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.ms.cmp(&other.ms))
    }
}

fn ordinal_suffix(n: u32) -> &'static str {
    match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    }
}

fn digits(s: &str) -> Option<u32> {
    if s.is_empty() || s.len() > 9 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// Splits a trailing `Z` or `±HH:MM` / `±HHMM` from a time.
fn split_zone(time: &str) -> (&str, Option<&str>) {
    if let Some(t) = time.strip_suffix(['Z', 'z']) {
        return (t, Some("Z"));
    }
    match time.rfind(['+', '-']) {
        Some(i) => (&time[..i], Some(&time[i..])),
        None => (time, None),
    }
}

/// Minutes east of UTC.
fn parse_zone(zone: &str) -> Option<i32> {
    if zone == "Z" {
        return Some(0);
    }
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let body = zone[1..].replace(':', "");
    if body.len() != 4 {
        return None;
    }
    let h = i32::try_from(digits(&body[..2])?).ok()?;
    let m = i32::try_from(digits(&body[2..])?).ok()?;
    Some(sign * (h * 60 + m))
}

/// A length of time: calendar months plus exact milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Duration {
    /// Calendar months (years count as 12).
    pub months: i64,
    /// Exact milliseconds (weeks, days, hours, minutes, seconds).
    pub ms: i64,
}

impl Duration {
    /// Exactly `ms` milliseconds.
    #[must_use]
    pub fn from_ms(ms: i64) -> Self {
        Self { months: 0, ms }
    }

    /// An approximate length in milliseconds (a month counts as 30.4375
    /// days), for comparisons and division.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn approx_ms(self) -> f64 {
        self.months as f64 * 30.4375 * MS_PER_DAY as f64 + self.ms as f64
    }

    /// The negated duration.
    #[must_use]
    pub fn negated(self) -> Self {
        Self {
            months: -self.months,
            ms: -self.ms,
        }
    }

    /// The sum.
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        Self {
            months: self.months.saturating_add(other.months),
            ms: self.ms.saturating_add(other.ms),
        }
    }

    /// Scaled by `factor` (months are rounded).
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    pub fn times(self, factor: f64) -> Self {
        Self {
            months: (self.months as f64 * factor).round() as i64,
            ms: (self.ms as f64 * factor).round() as i64,
        }
    }

    /// Parses `"1d"`, `"2 hours"`, `"1M"`, `"1 year 2 months"`, `"-3w"`.
    /// Units: `y`/`year`/`years`, `M`/`month`/`months`, `w`/`week`/`weeks`,
    /// `d`/`day`/`days`, `h`/`hour`/`hours`, `m`/`minute`/`minutes`,
    /// `s`/`second`/`seconds` (one-letter units are case-sensitive: `M`
    /// is a month, `m` a minute; words ignore case). A number without a
    /// unit is milliseconds.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let mut s = text.trim();
        if s.is_empty() {
            return None;
        }
        let mut sign = 1.0;
        if let Some(rest) = s.strip_prefix('-') {
            sign = -1.0;
            s = rest.trim_start();
        } else if let Some(rest) = s.strip_prefix('+') {
            s = rest.trim_start();
        }
        let mut total = Self::default();
        let mut any = false;
        while !s.is_empty() {
            let num_end = s
                .find(|c: char| !(c.is_ascii_digit() || c == '.'))
                .unwrap_or(s.len());
            if num_end == 0 {
                return None;
            }
            let n: f64 = s[..num_end].parse().ok()?;
            s = s[num_end..].trim_start();
            let unit_end = s
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(s.len());
            let unit = &s[..unit_end];
            s = s[unit_end..].trim_start_matches([' ', ',']);
            let part = unit_duration(unit, n * sign)?;
            total = total.plus(part);
            any = true;
        }
        any.then_some(total)
    }

    /// "2 days 3 hours", largest units first; "0 seconds" when empty.
    #[must_use]
    pub fn display(self) -> String {
        let mut parts = Vec::new();
        let neg = self.months < 0 || (self.months == 0 && self.ms < 0);
        let months = self.months.abs();
        let (years, months) = (months / 12, months % 12);
        let mut ms = self.ms.abs();
        let units = [
            ("year", years),
            ("month", months),
            ("day", {
                let d = ms / MS_PER_DAY;
                ms %= MS_PER_DAY;
                d
            }),
            ("hour", {
                let h = ms / MS_PER_HOUR;
                ms %= MS_PER_HOUR;
                h
            }),
            ("minute", {
                let m = ms / MS_PER_MINUTE;
                ms %= MS_PER_MINUTE;
                m
            }),
            ("second", ms / MS_PER_SECOND),
        ];
        for (name, n) in units {
            if n != 0 {
                parts.push(format!("{n} {name}{}", if n == 1 { "" } else { "s" }));
            }
        }
        if parts.is_empty() {
            return "0 seconds".into();
        }
        let text = parts.join(" ");
        if neg { format!("-{text}") } else { text }
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn unit_duration(unit: &str, n: f64) -> Option<Duration> {
    let exact = |per: i64| Duration::from_ms((n * per as f64).round() as i64);
    let lower = unit.to_ascii_lowercase();
    Some(match unit {
        "M" => Duration {
            months: n.round() as i64,
            ms: 0,
        },
        "m" => exact(MS_PER_MINUTE),
        "" => exact(1),
        _ => match lower.as_str() {
            "y" | "year" | "years" | "yr" | "yrs" => Duration {
                months: (n * 12.0).round() as i64,
                ms: 0,
            },
            "month" | "months" => Duration {
                months: n.round() as i64,
                ms: 0,
            },
            "w" | "week" | "weeks" => exact(7 * MS_PER_DAY),
            "d" | "day" | "days" => exact(MS_PER_DAY),
            "h" | "hour" | "hours" | "hr" | "hrs" => exact(MS_PER_HOUR),
            "minute" | "minutes" | "min" | "mins" => exact(MS_PER_MINUTE),
            "s" | "second" | "seconds" | "sec" | "secs" => exact(MS_PER_SECOND),
            "ms" | "millisecond" | "milliseconds" => exact(1),
            _ => return None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Date {
        Date::parse(s, 0).expect("date")
    }

    #[test]
    fn civil_round_trip() {
        for days in [-1_000_000, -1, 0, 1, 10_957, 20_000, 2_932_896] {
            let (y, m, dd) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, dd), days);
        }
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }

    #[test]
    fn parsing() {
        let a = d("2025-05-27");
        assert!(!a.has_time);
        assert_eq!(a.format("YYYY-MM-DD"), "2025-05-27");
        let b = d("2025-05-27 13:04:05");
        assert_eq!(b.civil().second, 5);
        assert_eq!(d("2025-05-27T13:04").format("HH:mm:ss"), "13:04:00");
        assert_eq!(d("2025-05-27T13:04:05.5").civil().millisecond, 500);
        assert!(Date::parse("2025-02-30", 0).is_none());
        assert!(Date::parse("2025-13-01", 0).is_none());
        assert!(Date::parse("nope", 0).is_none());
        assert!(Date::parse("2025-01-01x", 0).is_none());
        // UTC to UTC+2.
        let z = Date::parse("2025-01-01T10:00:00Z", 120).expect("zoned");
        assert_eq!(z.format("HH:mm"), "12:00");
        let o = Date::parse("2025-01-01T10:00:00+01:00", 0).expect("offset");
        assert_eq!(o.format("HH:mm"), "09:00");
    }

    #[test]
    fn formatting() {
        let t = d("2024-03-05 14:07:09");
        assert_eq!(
            t.format("dddd, MMMM Do YYYY, h:mm:ss a"),
            "Tuesday, March 5th 2024, 2:07:09 pm"
        );
        assert_eq!(t.format("ddd MMM D YY [at] HH[h]"), "Tue Mar 5 24 at 14h");
        assert_eq!(t.format("Q DDDD"), "1 065");
        assert_eq!(d("2021-01-03").format("W"), "53");
        assert_eq!(d("2021-01-04").format("WW"), "01");
        assert_eq!(d("2024-01-01").display(), "2024-01-01");
        assert_eq!(t.display(), "2024-03-05 14:07");
    }

    #[test]
    fn durations() {
        assert_eq!(Duration::parse("1d"), Some(Duration::from_ms(MS_PER_DAY)));
        assert_eq!(
            Duration::parse("2 hours"),
            Some(Duration::from_ms(2 * MS_PER_HOUR))
        );
        assert_eq!(Duration::parse("1M").map(|x| x.months), Some(1));
        assert_eq!(
            Duration::parse("1m"),
            Some(Duration::from_ms(MS_PER_MINUTE))
        );
        assert_eq!(
            Duration::parse("1 Week"),
            Some(Duration::from_ms(7 * MS_PER_DAY))
        );
        assert_eq!(
            Duration::parse("1 year 2 months"),
            Some(Duration { months: 14, ms: 0 })
        );
        assert_eq!(
            Duration::parse("-3d"),
            Some(Duration::from_ms(-3 * MS_PER_DAY))
        );
        assert_eq!(Duration::parse("abc"), None);
        assert_eq!(Duration::parse("3 fortnights"), None);
        assert_eq!(Duration::parse(""), None);
        assert_eq!(
            Duration::parse("90m").map(Duration::display).as_deref(),
            Some("1 hour 30 minutes")
        );
        assert_eq!(Duration::default().display(), "0 seconds");
    }

    #[test]
    fn arithmetic() {
        // The help page's example: 2024-12-01 + 1M + 4h + 3m.
        let r = d("2024-12-01")
            .shifted(Duration::parse("1M").expect("dur"))
            .shifted(Duration::parse("4h").expect("dur"))
            .shifted(Duration::parse("3m").expect("dur"));
        assert_eq!(r.format("YYYY-MM-DD HH:mm:ss"), "2025-01-01 04:03:00");
        // Month ends clamp.
        let r = d("2024-01-31").shifted(Duration::parse("1M").expect("dur"));
        assert_eq!(r.format("YYYY-MM-DD"), "2024-02-29");
        let r = d("2024-03-31").shifted(Duration::parse("-1M").expect("dur"));
        assert_eq!(r.format("YYYY-MM-DD"), "2024-02-29");
        assert_eq!(d("1969-12-31 23:00").civil().hour, 23);
    }

    #[test]
    fn relative_times() {
        let now = d("2025-01-10 12:00");
        assert_eq!(d("2025-01-07 12:00").relative(now), "3 days ago");
        assert_eq!(d("2025-01-10 11:59:50").relative(now), "a few seconds ago");
        assert_eq!(d("2025-01-10 14:00").relative(now), "in 2 hours");
        assert_eq!(d("2024-01-01").relative(now), "a year ago");
        assert_eq!(d("2020-01-01").relative(now), "5 years ago");
    }
}
