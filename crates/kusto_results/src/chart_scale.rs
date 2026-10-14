//! Where the ticks of a chart axis go and what they are called: linear and logarithmic axes
//! of numbers, and axes of times and of durations.
//!
//! Times are counted in milliseconds since the Unix epoch and are always read as UTC, the zone
//! Kusto stores them in.

use chrono::{DateTime, Datelike as _, TimeZone as _, Timelike as _, Utc};

/// A mark on an axis and its label.
#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    pub value: f64,
    pub label: String,
}

/// The step between ticks of a linear axis: 1, 2 or 5 times a power of ten that gives at most
/// `target` ticks across `span`.
pub fn nice_step(span: f64, target: usize) -> f64 {
    if !span.is_finite() || span <= 0.0 {
        return 1.0;
    }
    let raw = span / target.max(1) as f64;
    let magnitude = 10f64.powf(raw.log10().floor());
    let fraction = raw / magnitude;
    let nice = if fraction <= 1.0 {
        1.0
    } else if fraction <= 2.0 {
        2.0
    } else if fraction <= 5.0 {
        5.0
    } else {
        10.0
    };
    nice * magnitude
}

/// `min` and `max` widened to multiples of the step that `nice_step` picks, and that step.
pub fn nice_range(min: f64, max: f64, target: usize) -> (f64, f64, f64) {
    if !(min.is_finite() && max.is_finite()) || min > max {
        return (0.0, 1.0, 0.2);
    }
    if min == max {
        let pad = if min == 0.0 { 1.0 } else { min.abs() * 0.1 };
        return nice_range(min - pad, max + pad, target);
    }
    let step = nice_step(max - min, target);
    (
        (min / step).floor() * step,
        (max / step).ceil() * step,
        step,
    )
}

/// The ticks of a linear axis between `min` and `max`, each a multiple of `step`.
pub fn linear_ticks(min: f64, max: f64, step: f64) -> Vec<Tick> {
    if !(min.is_finite() && max.is_finite() && step.is_finite()) || step <= 0.0 || min > max {
        return Vec::new();
    }
    let first = (min / step - 1e-9).ceil() as i64;
    let last = (max / step + 1e-9).floor() as i64;
    // A degenerate request must not make a huge list.
    if last - first > 1000 {
        return Vec::new();
    }
    let values: Vec<f64> = (first..=last).map(|index| index as f64 * step).collect();
    let labels = format_ticks(&values, step);
    values
        .into_iter()
        .zip(labels)
        .map(|(value, label)| Tick { value, label })
        .collect()
}

/// Labels for the ticks of a linear axis, all with the same number of decimals and a common
/// unit (`k`, `M`, `G`) once the numbers are large.
pub fn format_ticks(values: &[f64], step: f64) -> Vec<String> {
    let largest = values
        .iter()
        .fold(0f64, |largest, value| largest.max(value.abs()));
    let (divisor, suffix) = if step >= 1000.0 && largest >= 10_000.0 {
        si_unit(largest)
    } else {
        (1.0, "")
    };
    let decimals = decimals_for(step / divisor);
    values
        .iter()
        .map(|value| {
            let scaled = value / divisor;
            // Zero has no sign, and no unit.
            if scaled == 0.0 {
                "0".to_string()
            } else {
                format!("{scaled:.decimals$}{suffix}")
            }
        })
        .collect()
}

fn si_unit(largest: f64) -> (f64, &'static str) {
    if largest >= 1e12 {
        (1e12, "T")
    } else if largest >= 1e9 {
        (1e9, "G")
    } else if largest >= 1e6 {
        (1e6, "M")
    } else {
        (1e3, "k")
    }
}

/// How many decimals show every multiple of `step` exactly.
fn decimals_for(step: f64) -> usize {
    if !step.is_finite() || step <= 0.0 {
        return 0;
    }
    (0..=9usize)
        .find(|decimals| {
            let scaled = step * 10f64.powi(*decimals as i32);
            (scaled - scaled.round()).abs() < 1e-6 * scaled.abs().max(1.0)
        })
        .unwrap_or(9)
}

/// The ticks of a logarithmic axis: powers of ten, with 2 and 5 between them when the axis
/// spans fewer than three decades. `min` must be above zero.
pub fn log_ticks(min: f64, max: f64) -> Vec<Tick> {
    if !(min.is_finite() && max.is_finite()) || min <= 0.0 || min > max {
        return Vec::new();
    }
    let first_decade = min.log10().floor() as i32;
    let last_decade = max.log10().ceil() as i32;
    if last_decade - first_decade > 300 {
        return Vec::new();
    }
    let multipliers: &[f64] = if last_decade - first_decade < 3 {
        &[1.0, 2.0, 5.0]
    } else {
        &[1.0]
    };
    let mut ticks = Vec::new();
    for decade in first_decade..=last_decade {
        for multiplier in multipliers {
            let value = multiplier * 10f64.powi(decade);
            if value >= min * (1.0 - 1e-9) && value <= max * (1.0 + 1e-9) {
                ticks.push(Tick {
                    value,
                    label: format_plain(value),
                });
            }
        }
    }
    ticks
}

/// A number the way a tooltip or a log tick shows it: grouped by thousands, with the digits
/// that matter and no trailing zeros.
pub fn format_plain(value: f64) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let magnitude = value.abs();
    if magnitude != 0.0 && !(1e-4..1e15).contains(&magnitude) {
        return format!("{value:e}");
    }
    let text = if magnitude >= 1000.0 {
        format!("{value:.2}")
    } else if magnitude >= 1.0 {
        format!("{value:.4}")
    } else {
        format!("{value:.6}")
    };
    let text = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    };
    group_thousands(&text)
}

fn group_thousands(text: &str) -> String {
    let (sign, rest) = match text.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", text),
    };
    let (whole, fraction) = match rest.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (rest, None),
    };
    let mut grouped = String::new();
    for (position, digit) in whole.chars().enumerate() {
        if position > 0 && (whole.len() - position) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    match fraction {
        Some(fraction) => format!("{sign}{grouped}.{fraction}"),
        None => format!("{sign}{grouped}"),
    }
}

const SECOND: i64 = 1000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;
/// 1970-01-01 was a Thursday; weeks start on Monday, four days later.
const FIRST_MONDAY: i64 = 4 * DAY;

#[derive(Clone, Copy)]
enum Step {
    Fixed(i64),
    Months(i64),
}

impl Step {
    fn length(self) -> f64 {
        match self {
            Step::Fixed(milliseconds) => milliseconds as f64,
            Step::Months(months) => months as f64 * 30.4375 * DAY as f64,
        }
    }
}

const TIME_STEPS: &[Step] = &[
    Step::Fixed(1),
    Step::Fixed(2),
    Step::Fixed(5),
    Step::Fixed(10),
    Step::Fixed(20),
    Step::Fixed(50),
    Step::Fixed(100),
    Step::Fixed(200),
    Step::Fixed(500),
    Step::Fixed(SECOND),
    Step::Fixed(2 * SECOND),
    Step::Fixed(5 * SECOND),
    Step::Fixed(10 * SECOND),
    Step::Fixed(15 * SECOND),
    Step::Fixed(30 * SECOND),
    Step::Fixed(MINUTE),
    Step::Fixed(2 * MINUTE),
    Step::Fixed(5 * MINUTE),
    Step::Fixed(10 * MINUTE),
    Step::Fixed(15 * MINUTE),
    Step::Fixed(30 * MINUTE),
    Step::Fixed(HOUR),
    Step::Fixed(3 * HOUR),
    Step::Fixed(6 * HOUR),
    Step::Fixed(12 * HOUR),
    Step::Fixed(DAY),
    Step::Fixed(2 * DAY),
    Step::Fixed(WEEK),
    Step::Fixed(2 * WEEK),
    Step::Months(1),
    Step::Months(3),
    Step::Months(6),
    Step::Months(12),
    Step::Months(24),
    Step::Months(60),
    Step::Months(120),
    Step::Months(240),
    Step::Months(600),
    Step::Months(1200),
];

/// Milliseconds since the epoch as a date, or `None` when it is out of range.
fn to_date(milliseconds: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(milliseconds).single()
}

/// The ticks of a time axis between `min` and `max` (milliseconds since the epoch), on the
/// boundary of a second, minute, hour, day, week, month or year, with about `target` of them.
pub fn time_ticks(min: f64, max: f64, target: usize) -> Vec<Tick> {
    if !(min.is_finite() && max.is_finite()) || min > max {
        return Vec::new();
    }
    let span = (max - min).max(1.0);
    let step = TIME_STEPS
        .iter()
        .copied()
        .find(|step| span / step.length() <= target.max(1) as f64)
        .unwrap_or(Step::Months(1200));
    let mut instants: Vec<i64> = Vec::new();
    match step {
        Step::Fixed(length) => {
            let offset = if length % WEEK == 0 { FIRST_MONDAY } else { 0 };
            let mut tick =
                ((min as i64 - offset) as f64 / length as f64).ceil() as i64 * length + offset;
            while (tick as f64) <= max && instants.len() <= 1000 {
                instants.push(tick);
                tick += length;
            }
        }
        Step::Months(months) => {
            let Some(start) = to_date(min as i64) else {
                return Vec::new();
            };
            let start_index = i64::from(start.year()) * 12 + i64::from(start.month0());
            let mut index = (start_index as f64 / months as f64).ceil() as i64 * months;
            while instants.len() <= 1000 {
                let (year, month) = (index.div_euclid(12), index.rem_euclid(12) + 1);
                let Some(date) = i32::try_from(year).ok().and_then(|year| {
                    Utc.with_ymd_and_hms(year, month as u32, 1, 0, 0, 0)
                        .single()
                }) else {
                    break;
                };
                let tick = date.timestamp_millis();
                if (tick as f64) > max {
                    break;
                }
                if (tick as f64) >= min {
                    instants.push(tick);
                }
                index += months;
            }
        }
    }
    let spans_years = match (to_date(min as i64), to_date(max as i64)) {
        (Some(first), Some(last)) => first.year() != last.year(),
        _ => false,
    };
    instants
        .into_iter()
        .map(|instant| Tick {
            value: instant as f64,
            label: time_label(instant, step, spans_years),
        })
        .collect()
}

fn time_label(instant: i64, step: Step, spans_years: bool) -> String {
    let Some(date) = to_date(instant) else {
        return String::new();
    };
    let at_midnight = date.hour() == 0 && date.minute() == 0 && date.second() == 0;
    let format = match step {
        Step::Fixed(length) if length < SECOND => "%H:%M:%S%.3f",
        Step::Fixed(length) if length < MINUTE => "%H:%M:%S",
        Step::Fixed(length) if length < DAY => {
            // The day a run of times belongs to is named where the day changes.
            if at_midnight {
                if spans_years { "%b %-d, %Y" } else { "%b %-d" }
            } else {
                "%H:%M"
            }
        }
        Step::Fixed(_) => {
            if spans_years {
                "%b %-d, %Y"
            } else {
                "%b %-d"
            }
        }
        Step::Months(months) if months < 12 => "%b %Y",
        Step::Months(_) => "%Y",
    };
    date.format(format).to_string()
}

/// A time as a tooltip shows it: ISO 8601, with fractions of a second only when there are some.
pub fn format_time(milliseconds: f64) -> String {
    let Some(date) = to_date(milliseconds.round() as i64) else {
        return String::new();
    };
    if date.timestamp_subsec_millis() == 0 {
        date.format("%Y-%m-%dT%H:%M:%SZ").to_string()
    } else {
        date.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
    }
}

/// A time as the label of a bar: the date, and the time only when some label has one.
pub fn format_time_label(milliseconds: f64, with_time: bool) -> String {
    let Some(date) = to_date(milliseconds.round() as i64) else {
        return String::new();
    };
    if with_time {
        date.format("%Y-%m-%d %H:%M").to_string()
    } else {
        date.format("%Y-%m-%d").to_string()
    }
}

const DURATION_STEPS: &[i64] = &[
    1,
    2,
    5,
    10,
    20,
    50,
    100,
    200,
    500,
    SECOND,
    2 * SECOND,
    5 * SECOND,
    10 * SECOND,
    15 * SECOND,
    30 * SECOND,
    MINUTE,
    2 * MINUTE,
    5 * MINUTE,
    10 * MINUTE,
    15 * MINUTE,
    30 * MINUTE,
    HOUR,
    3 * HOUR,
    6 * HOUR,
    12 * HOUR,
    DAY,
    2 * DAY,
    7 * DAY,
    14 * DAY,
    30 * DAY,
    90 * DAY,
    365 * DAY,
];

/// The ticks of an axis of durations (milliseconds): whole seconds, minutes, hours and days
/// rather than multiples of ten.
pub fn duration_ticks(min: f64, max: f64, target: usize) -> Vec<Tick> {
    if !(min.is_finite() && max.is_finite()) || min > max {
        return Vec::new();
    }
    let span = (max - min).max(1.0);
    let length = DURATION_STEPS
        .iter()
        .copied()
        .find(|length| span / *length as f64 <= target.max(1) as f64)
        .unwrap_or(365 * DAY);
    let first = (min / length as f64 - 1e-9).ceil() as i64;
    let last = (max / length as f64 + 1e-9).floor() as i64;
    if last - first > 1000 {
        return Vec::new();
    }
    (first..=last)
        .map(|index| {
            let value = (index * length) as f64;
            Tick {
                value,
                label: format_duration(value),
            }
        })
        .collect()
}

/// A duration given in milliseconds: `250ms`, `1.5s`, `2m 30s`, `1h 5m`, `3d 4h`.
pub fn format_duration(milliseconds: f64) -> String {
    if !milliseconds.is_finite() {
        return milliseconds.to_string();
    }
    let sign = if milliseconds < 0.0 { "-" } else { "" };
    let magnitude = milliseconds.abs();
    let text = if magnitude == 0.0 {
        "0".to_string()
    } else if magnitude < SECOND as f64 {
        format!("{}ms", trim_fraction(magnitude, 3))
    } else if magnitude < MINUTE as f64 {
        format!("{}s", trim_fraction(magnitude / SECOND as f64, 3))
    } else {
        let whole = magnitude.round() as i64;
        let (days, hours, minutes, seconds) = (
            whole / DAY,
            whole % DAY / HOUR,
            whole % HOUR / MINUTE,
            whole % MINUTE / SECOND,
        );
        if days > 0 {
            join_units(&[(days, "d"), (hours, "h")])
        } else if hours > 0 {
            join_units(&[(hours, "h"), (minutes, "m")])
        } else {
            join_units(&[(minutes, "m"), (seconds, "s")])
        }
    };
    format!("{sign}{text}")
}

/// The units that are not zero, except that the first is always kept.
fn join_units(units: &[(i64, &str)]) -> String {
    units
        .iter()
        .enumerate()
        .filter(|(position, (amount, _))| *position == 0 || *amount != 0)
        .map(|(_, (amount, unit))| format!("{amount}{unit}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn trim_fraction(value: f64, decimals: usize) -> String {
    let text = format!("{value:.decimals$}");
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn labels(ticks: &[Tick]) -> Vec<&str> {
        ticks.iter().map(|tick| tick.label.as_str()).collect()
    }

    fn at(text: &str) -> f64 {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .timestamp_millis() as f64
    }

    #[test]
    fn linear_steps_are_one_two_or_five_times_a_power_of_ten() {
        assert_eq!(nice_step(100.0, 5), 20.0);
        assert_eq!(nice_step(100.0, 10), 10.0);
        assert_eq!(nice_step(7.0, 5), 2.0);
        assert_eq!(nice_step(0.9, 5), 0.2);
        assert_eq!(nice_step(0.0, 5), 1.0);
        assert_eq!(nice_step(f64::NAN, 5), 1.0);
    }

    #[test]
    fn a_range_is_widened_to_whole_steps() {
        assert_eq!(nice_range(3.0, 97.0, 5), (0.0, 100.0, 20.0));
        assert_eq!(nice_range(0.0, 0.0, 5), (-1.0, 1.0, 0.5));
        let (low, high, _) = nice_range(5.0, 5.0, 5);
        assert!(low < 5.0 && high > 5.0);
    }

    #[test]
    fn linear_ticks_are_multiples_of_the_step_inside_the_range() {
        let ticks = linear_ticks(0.0, 100.0, 25.0);
        assert_eq!(labels(&ticks), ["0", "25", "50", "75", "100"]);
        let ticks = linear_ticks(0.0, 1.0, 0.25);
        assert_eq!(labels(&ticks), ["0", "0.25", "0.50", "0.75", "1.00"]);
        let ticks = linear_ticks(-0.3, 0.3, 0.1);
        assert_eq!(labels(&ticks).len(), 7);
        assert_eq!(ticks[3].label, "0");
        assert_eq!(linear_ticks(1.0, 0.0, 1.0), Vec::new());
        assert_eq!(linear_ticks(0.0, 1e12, 1.0), Vec::new(), "too many to draw");
    }

    #[test]
    fn large_numbers_share_a_unit() {
        let ticks = linear_ticks(0.0, 100_000.0, 25_000.0);
        assert_eq!(labels(&ticks), ["0", "25k", "50k", "75k", "100k"]);
        let ticks = linear_ticks(0.0, 3_000_000.0, 1_000_000.0);
        assert_eq!(labels(&ticks), ["0", "1M", "2M", "3M"]);
        let ticks = linear_ticks(0.0, 15_000.0, 5_000.0);
        assert_eq!(labels(&ticks), ["0", "5k", "10k", "15k"]);
        let ticks = linear_ticks(0.0, 500.0, 100.0);
        assert_eq!(labels(&ticks), ["0", "100", "200", "300", "400", "500"]);
    }

    #[test]
    fn log_ticks_are_powers_of_ten() {
        let ticks = log_ticks(1.0, 100_000.0);
        assert_eq!(
            labels(&ticks),
            ["1", "10", "100", "1,000", "10,000", "100,000"]
        );
        let ticks = log_ticks(1.0, 100.0);
        assert_eq!(labels(&ticks), ["1", "2", "5", "10", "20", "50", "100"]);
        assert_eq!(log_ticks(0.0, 10.0), Vec::new());
        assert_eq!(
            log_ticks(0.5, 3.0).first().map(|tick| tick.value),
            Some(0.5)
        );
    }

    #[test]
    fn plain_numbers_keep_the_digits_that_matter() {
        assert_eq!(format_plain(1234567.0), "1,234,567");
        assert_eq!(format_plain(-1234.5), "-1,234.5");
        assert_eq!(format_plain(0.5), "0.5");
        assert_eq!(format_plain(12.3456789), "12.3457");
        assert_eq!(format_plain(0.0), "0");
        assert_eq!(format_plain(1e20), "1e20");
        assert_eq!(format_plain(f64::INFINITY), "inf");
    }

    #[test]
    fn day_ticks_fall_on_midnight_utc() {
        let ticks = time_ticks(at("2007-01-01T00:00:00Z"), at("2007-01-08T00:00:00Z"), 8);
        assert_eq!(
            labels(&ticks),
            [
                "Jan 1", "Jan 2", "Jan 3", "Jan 4", "Jan 5", "Jan 6", "Jan 7", "Jan 8"
            ]
        );
        assert_eq!(ticks[0].value, at("2007-01-01T00:00:00Z"));
    }

    #[test]
    fn a_day_of_hours_names_the_day_where_it_changes() {
        let ticks = time_ticks(at("2007-01-01T18:00:00Z"), at("2007-01-02T06:00:00Z"), 6);
        assert_eq!(
            labels(&ticks),
            ["18:00", "21:00", "Jan 2", "03:00", "06:00"]
        );
    }

    #[test]
    fn minutes_and_seconds_have_clock_labels() {
        let ticks = time_ticks(at("2026-10-14T10:00:07Z"), at("2026-10-14T10:00:37Z"), 6);
        assert_eq!(
            labels(&ticks),
            [
                "10:00:10", "10:00:15", "10:00:20", "10:00:25", "10:00:30", "10:00:35"
            ]
        );
    }

    #[test]
    fn weeks_start_on_monday() {
        let ticks = time_ticks(at("2026-10-01T00:00:00Z"), at("2026-12-01T00:00:00Z"), 9);
        let first = to_date(ticks[0].value as i64).unwrap();
        assert_eq!(first.format("%A").to_string(), "Monday");
    }

    #[test]
    fn months_and_years_land_on_the_first() {
        let ticks = time_ticks(at("2026-01-15T00:00:00Z"), at("2026-12-20T00:00:00Z"), 6);
        assert_eq!(labels(&ticks), ["Apr 2026", "Jul 2026", "Oct 2026"]);
        let ticks = time_ticks(at("2000-06-01T00:00:00Z"), at("2026-06-01T00:00:00Z"), 6);
        assert_eq!(labels(&ticks), ["2005", "2010", "2015", "2020", "2025"]);
        assert_eq!(ticks[0].value, at("2005-01-01T00:00:00Z"));
    }

    #[test]
    fn a_span_over_a_new_year_names_the_year_on_day_ticks() {
        let ticks = time_ticks(at("2026-12-29T00:00:00Z"), at("2027-01-03T00:00:00Z"), 8);
        assert_eq!(ticks[0].label, "Dec 29, 2026");
    }

    #[test]
    fn times_outside_the_calendar_give_no_ticks() {
        assert_eq!(time_ticks(f64::NAN, 1.0, 5), Vec::new());
        assert_eq!(time_ticks(5.0, 1.0, 5), Vec::new());
        assert_eq!(time_ticks(1e18, 1.1e18, 5), Vec::new());
    }

    #[test]
    fn times_are_shown_as_iso_text() {
        assert_eq!(
            format_time(at("2007-01-01T12:30:00Z")),
            "2007-01-01T12:30:00Z"
        );
        assert_eq!(
            format_time(at("2007-01-01T12:30:00.250Z")),
            "2007-01-01T12:30:00.250Z"
        );
        assert_eq!(
            format_time_label(at("2007-01-01T00:00:00Z"), false),
            "2007-01-01"
        );
        assert_eq!(
            format_time_label(at("2007-01-01T06:30:00Z"), true),
            "2007-01-01 06:30"
        );
    }

    #[test]
    fn durations_read_in_the_largest_units() {
        assert_eq!(format_duration(250.0), "250ms");
        assert_eq!(format_duration(1500.0), "1.5s");
        assert_eq!(format_duration(150_000.0), "2m 30s");
        assert_eq!(format_duration(120_000.0), "2m");
        assert_eq!(format_duration(3_900_000.0), "1h 5m");
        assert_eq!(format_duration(100_800_000.0), "1d 4h");
        assert_eq!(format_duration(-1500.0), "-1.5s");
    }

    #[test]
    fn duration_ticks_use_whole_units() {
        let ticks = duration_ticks(0.0, 120_000.0, 6);
        assert_eq!(labels(&ticks), ["0", "30s", "1m", "1m 30s", "2m"]);
    }
}
