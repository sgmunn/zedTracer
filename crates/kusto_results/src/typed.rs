//! Parsing and comparison of Kusto-typed values.
//!
//! Sorting and filtering compare values by type, never by their formatted text.
//! This module holds the parsers and comparators both of them share.

use std::cmp::Ordering;

use crate::result::{Cell, ColumnKind};

pub const TICKS_PER_SECOND: i64 = 10_000_000;
const TICKS_PER_DAY: i64 = 86_400 * TICKS_PER_SECOND;
const DAYS_FROM_YEAR_ONE_TO_UNIX_EPOCH: i64 = 719_162;

/// Parses an ISO 8601 date or date-time into 100 ns ticks since 0001-01-01T00:00:00Z.
///
/// Text without a zone is read as UTC, because Kusto datetimes are UTC. The fractional
/// part keeps up to seven digits, the resolution the server sends.
pub fn parse_datetime_ticks(text: &str) -> Option<i64> {
    let text = text.trim();
    let bytes = text.as_bytes();
    if bytes.len() < 10 || bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    let year: i64 = text.get(0..4)?.parse().ok()?;
    let month: i64 = text.get(5..7)?.parse().ok()?;
    let day: i64 = text.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    let mut ticks = days_from_civil(year, month, day)
        .checked_add(DAYS_FROM_YEAR_ONE_TO_UNIX_EPOCH)?
        .checked_mul(TICKS_PER_DAY)?;

    let mut rest = text.get(10..)?;
    if rest.is_empty() {
        return Some(ticks);
    }
    rest = rest.strip_prefix(['T', ' '])?;

    let (clock, zone) = split_zone(rest);
    let (whole, fraction) = match clock.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (clock, ""),
    };
    let mut parts = whole.split(':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = match parts.next() {
        Some(second) => second.parse().ok()?,
        None => 0,
    };
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    ticks = ticks.checked_add(((hour * 60 + minute) * 60 + second) * TICKS_PER_SECOND)?;
    ticks = ticks.checked_add(parse_fraction_ticks(fraction)?)?;

    let offset_seconds = parse_zone_offset_seconds(zone)?;
    ticks.checked_sub(offset_seconds.checked_mul(TICKS_PER_SECOND)?)
}

fn split_zone(text: &str) -> (&str, &str) {
    if let Some(clock) = text.strip_suffix('Z') {
        return (clock, "Z");
    }
    match text.rfind(['+', '-']) {
        Some(index) if index > 0 => (&text[..index], &text[index..]),
        _ => (text, ""),
    }
}

fn parse_zone_offset_seconds(zone: &str) -> Option<i64> {
    if zone.is_empty() || zone == "Z" {
        return Some(0);
    }
    let sign = if zone.starts_with('-') { -1 } else { 1 };
    let digits = zone.get(1..)?;
    let (hours, minutes) = match digits.split_once(':') {
        Some((hours, minutes)) => (hours, minutes),
        None if digits.len() == 4 => digits.split_at(2),
        None => (digits, "0"),
    };
    let hours: i64 = hours.parse().ok()?;
    let minutes: i64 = minutes.parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

fn parse_fraction_ticks(fraction: &str) -> Option<i64> {
    if fraction.is_empty() {
        return Some(0);
    }
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut digits = String::from(&fraction[..fraction.len().min(7)]);
    while digits.len() < 7 {
        digits.push('0');
    }
    digits.parse().ok()
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        _ => 28,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Parses a .NET constant-format timespan, `[-][d.]hh:mm:ss[.fffffff]`, into ticks.
pub fn parse_timespan_ticks(text: &str) -> Option<i64> {
    let text = text.trim();
    let (negative, body) = match text.strip_prefix('-') {
        Some(body) => (true, body),
        None => (false, text),
    };
    let (days, clock) = match body.split_once('.') {
        Some((days, rest)) if !days.contains(':') => (days.parse::<i64>().ok()?, rest),
        _ => (0, body),
    };
    let (whole, fraction) = match clock.split_once('.') {
        Some((whole, fraction)) => (whole, fraction),
        None => (clock, ""),
    };
    let mut parts = whole.split(':');
    let hours: i64 = parts.next()?.parse().ok()?;
    let minutes: i64 = parts.next()?.parse().ok()?;
    let seconds: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || minutes > 59 || seconds > 59 {
        return None;
    }
    let seconds_total = ((days.checked_mul(24)?.checked_add(hours)?) * 60 + minutes) * 60 + seconds;
    let ticks = seconds_total
        .checked_mul(TICKS_PER_SECOND)?
        .checked_add(parse_fraction_ticks(fraction)?)?;
    Some(if negative { -ticks } else { ticks })
}

/// An exact decimal number, kept as text so values beyond 64 bits survive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decimal {
    negative: bool,
    integer: String,
    fraction: String,
}

impl Decimal {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (negative, digits) = match text.strip_prefix('-') {
            Some(digits) => (true, digits),
            None => (false, text.strip_prefix('+').unwrap_or(text)),
        };
        let (integer, fraction) = digits.split_once('.').unwrap_or((digits, ""));
        let all_digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
        if integer.is_empty() && fraction.is_empty()
            || !all_digits(integer)
            || !all_digits(fraction)
        {
            return None;
        }
        let integer = integer.trim_start_matches('0');
        let integer = if integer.is_empty() { "0" } else { integer };
        let fraction = fraction.trim_end_matches('0');
        let is_zero = integer == "0" && fraction.is_empty();
        Some(Self {
            negative: negative && !is_zero,
            integer: integer.to_string(),
            fraction: fraction.to_string(),
        })
    }

    pub fn to_f64(&self) -> f64 {
        let text = format!(
            "{}{}.{}",
            if self.negative { "-" } else { "" },
            self.integer,
            if self.fraction.is_empty() {
                "0"
            } else {
                &self.fraction
            }
        );
        text.parse().unwrap_or(f64::NAN)
    }
}

impl Ord for Decimal {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self.negative, other.negative) {
            (false, true) => return Ordering::Greater,
            (true, false) => return Ordering::Less,
            _ => {}
        }
        let magnitude = self
            .integer
            .len()
            .cmp(&other.integer.len())
            .then_with(|| self.integer.cmp(&other.integer))
            .then_with(|| self.fraction.cmp(&other.fraction));
        if self.negative {
            magnitude.reverse()
        } else {
            magnitude
        }
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A number in whichever exact form its column supplies.
#[derive(Debug, Clone, PartialEq)]
pub enum NumberKey {
    Int(i64),
    Real(f64),
    Decimal(Decimal),
}

impl NumberKey {
    fn to_f64(&self) -> f64 {
        match self {
            NumberKey::Int(value) => *value as f64,
            NumberKey::Real(value) => *value,
            NumberKey::Decimal(value) => value.to_f64(),
        }
    }

    /// Integers and decimals compare exactly. Only a comparison that involves a real goes
    /// through floating point, where exactness beyond 2^53 is not meaningful anyway.
    pub fn compare(&self, other: &Self) -> Ordering {
        match (self, other) {
            (NumberKey::Int(left), NumberKey::Int(right)) => left.cmp(right),
            (NumberKey::Decimal(left), NumberKey::Decimal(right)) => left.cmp(right),
            _ => self.to_f64().total_cmp(&other.to_f64()),
        }
    }
}

/// Parses number text according to its column: decimals stay exact, everything else is an
/// integer when it can be and a real otherwise.
pub fn parse_number(text: &str, kind: ColumnKind) -> Option<NumberKey> {
    let text = text.trim();
    if kind == ColumnKind::Decimal {
        if let Some(decimal) = Decimal::parse(text) {
            return Some(NumberKey::Decimal(decimal));
        }
    }
    if let Ok(value) = text.parse::<i64>() {
        return Some(NumberKey::Int(value));
    }
    text.parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
        .map(NumberKey::Real)
}

/// The comparable number in a cell of a numeric column, or `None` for null and for values
/// that are not numbers.
pub fn cell_number(cell: &Cell, kind: ColumnKind) -> Option<NumberKey> {
    match cell {
        Cell::Int(value) => Some(NumberKey::Int(*value)),
        Cell::Real(value) => Some(NumberKey::Real(*value)),
        Cell::Decimal(text) | Cell::Text(text) => parse_number(text, kind),
        Cell::Null | Cell::Bool(_) | Cell::Dynamic(_) => None,
    }
}

/// The comparable tick count in a cell of a datetime or timespan column.
pub fn cell_ticks(cell: &Cell, kind: ColumnKind) -> Option<i64> {
    match (cell, kind) {
        (Cell::Text(text), ColumnKind::DateTime) => parse_datetime_ticks(text),
        (Cell::Text(text), ColumnKind::TimeSpan) => parse_timespan_ticks(text),
        _ => None,
    }
}

/// Orders strings the way people read them: case-insensitive, with digit runs compared by
/// value, so `Step2` sorts before `Step10`. The caller breaks remaining ties.
pub fn natural_cmp(left: &str, right: &str) -> Ordering {
    let mut left_chars = left.chars().peekable();
    let mut right_chars = right.chars().peekable();
    loop {
        match (left_chars.peek().copied(), right_chars.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left_char), Some(right_char)) => {
                if left_char.is_ascii_digit() && right_char.is_ascii_digit() {
                    let left_run = take_digits(&mut left_chars);
                    let right_run = take_digits(&mut right_chars);
                    let left_value = left_run.trim_start_matches('0');
                    let right_value = right_run.trim_start_matches('0');
                    let order = left_value
                        .len()
                        .cmp(&right_value.len())
                        .then_with(|| left_value.cmp(right_value));
                    if order != Ordering::Equal {
                        return order;
                    }
                } else {
                    let order = left_char.cmp(&right_char);
                    if order != Ordering::Equal {
                        return order;
                    }
                    left_chars.next();
                    right_chars.next();
                }
            }
        }
    }
}

fn take_digits(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    let mut digits = String::new();
    while let Some(digit) = chars.next_if(|candidate| candidate.is_ascii_digit()) {
        digits.push(digit);
    }
    digits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_keeps_seven_digit_precision() {
        let midnight = parse_datetime_ticks("2025-01-01T00:00:00.0000000Z");
        let seven_ticks_later = parse_datetime_ticks("2025-01-01T00:00:00.0000007Z");
        assert_eq!(seven_ticks_later, midnight.map(|ticks| ticks + 7));
    }

    #[test]
    fn datetime_without_zone_is_utc() {
        assert_eq!(
            parse_datetime_ticks("2025-01-01"),
            parse_datetime_ticks("2025-01-01T00:00:00Z")
        );
        assert_eq!(
            parse_datetime_ticks("2025-01-01T01:00:00+01:00"),
            parse_datetime_ticks("2025-01-01T00:00:00Z")
        );
    }

    #[test]
    fn datetime_rejects_invalid_dates() {
        assert_eq!(parse_datetime_ticks("2025-02-30"), None);
        assert_eq!(parse_datetime_ticks("not a date"), None);
        assert_eq!(parse_datetime_ticks("2025-01-01T25:00:00Z"), None);
        assert!(parse_datetime_ticks("2024-02-29T12:00:00.0000000Z").is_some());
    }

    #[test]
    fn datetime_known_epoch_value() {
        assert_eq!(
            parse_datetime_ticks("1970-01-01T00:00:00Z"),
            Some(719_162 * TICKS_PER_DAY)
        );
    }

    #[test]
    fn timespan_orders_by_duration_not_text() {
        let ten_days = parse_timespan_ticks("10.00:00:00");
        let one_day = parse_timespan_ticks("1.00:00:00");
        assert!(ten_days > one_day);
        assert!(parse_timespan_ticks("-00:00:05") < parse_timespan_ticks("00:00:00.0000001"));
        assert_eq!(
            parse_timespan_ticks("00:01:00"),
            parse_timespan_ticks("0:01:00")
        );
        assert_eq!(parse_timespan_ticks("1:2"), None);
    }

    #[test]
    fn decimal_beyond_sixty_four_bits_compares_exactly() {
        let huge = Decimal::parse("79228162514264337593543950335");
        let almost = Decimal::parse("79228162514264337593543950334");
        assert!(huge > almost);
        assert!(Decimal::parse("-0.01") < Decimal::parse("0"));
        assert_eq!(Decimal::parse("12.50"), Decimal::parse("12.5"));
        assert_eq!(Decimal::parse("-0.0"), Decimal::parse("0"));
        assert_eq!(Decimal::parse("1e5"), None);
    }

    #[test]
    fn integers_compare_exactly_beyond_two_to_the_fifty_third() {
        let below = NumberKey::Int(9_007_199_254_740_992);
        let above = NumberKey::Int(9_007_199_254_740_993);
        assert_eq!(below.compare(&above), Ordering::Less);
    }

    #[test]
    fn natural_order_reads_digit_runs_as_numbers() {
        assert_eq!(natural_cmp("step2", "step10"), Ordering::Less);
        assert_eq!(natural_cmp("step02", "step2"), Ordering::Equal);
        assert_eq!(natural_cmp("abc", "abd"), Ordering::Less);
        assert_eq!(natural_cmp("a", "a1"), Ordering::Less);
    }
}
