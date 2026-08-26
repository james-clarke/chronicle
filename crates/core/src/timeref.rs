//! Deterministic time references in chat questions → local-time ms ranges.
//! No NLP: a fixed phrase set covers the questions people actually ask.

use jiff::civil::{Date, Weekday};
use jiff::tz::TimeZone;
use jiff::{ToSpan, Zoned};

/// Parse a time reference out of `text`. Returns a half-open UTC ms range,
/// or None when the question carries no recognizable time reference.
pub fn parse(text: &str, now: &Zoned) -> Option<(i64, i64)> {
    let words = normalize(text);
    let tz = now.time_zone();
    let today = now.date();
    let now_ms = now.timestamp().as_millisecond();

    // Explicit ISO date wins over everything.
    if let Some(date) = words.iter().find_map(|w| w.parse::<Date>().ok()) {
        return day_range(date, 0, 24, tz);
    }

    if let Some(range) = parse_last_n(&words, now_ms) {
        return Some(range);
    }

    if has_phrase(&words, &["this", "morning"]) {
        return day_range(today, 0, 12, tz);
    }
    if has_phrase(&words, &["this", "afternoon"]) {
        return day_range(today, 12, 18, tz);
    }
    if has_phrase(&words, &["this", "evening"]) || has_word(&words, "tonight") {
        return day_range(today, 17, 24, tz);
    }
    if has_phrase(&words, &["last", "night"]) {
        let yesterday = today.checked_sub(1.day()).ok()?;
        return Some((at_ms(yesterday, 18, tz)?, at_ms(today, 6, tz)?));
    }
    if has_word(&words, "yesterday") {
        let yesterday = today.checked_sub(1.day()).ok()?;
        if has_word(&words, "morning") {
            return day_range(yesterday, 0, 12, tz);
        }
        if has_word(&words, "afternoon") {
            return day_range(yesterday, 12, 18, tz);
        }
        if has_word(&words, "evening") {
            return day_range(yesterday, 17, 24, tz);
        }
        return day_range(yesterday, 0, 24, tz);
    }
    if has_word(&words, "today") {
        return day_range(today, 0, 24, tz);
    }
    if has_phrase(&words, &["this", "week"]) {
        let monday = week_start(today)?;
        return day_range_between(monday, today.checked_add(1.day()).ok()?, tz);
    }
    if has_phrase(&words, &["last", "week"]) {
        let monday = week_start(today)?;
        let prev = monday.checked_sub(7.days()).ok()?;
        return day_range_between(prev, monday, tz);
    }
    // A bare weekday name means its most recent occurrence (today included).
    for (name, weekday) in WEEKDAYS {
        if has_word(&words, name) {
            let back = i64::from(
                (today.weekday().to_monday_zero_offset() + 7 - weekday.to_monday_zero_offset()) % 7,
            );
            let date = today.checked_sub(back.days()).ok()?;
            return day_range(date, 0, 24, tz);
        }
    }
    None
}

const WEEKDAYS: [(&str, Weekday); 7] = [
    ("monday", Weekday::Monday),
    ("tuesday", Weekday::Tuesday),
    ("wednesday", Weekday::Wednesday),
    ("thursday", Weekday::Thursday),
    ("friday", Weekday::Friday),
    ("saturday", Weekday::Saturday),
    ("sunday", Weekday::Sunday),
];

/// "last/past hour", "last/past N hours/minutes", digit or small number word.
fn parse_last_n(words: &[String], now_ms: i64) -> Option<(i64, i64)> {
    let anchor = words.iter().position(|w| w == "last" || w == "past")?;
    let rest = &words[anchor + 1..];
    let (n, unit) = match rest {
        [unit, ..] if is_hour(unit) => (1, 60),
        [n, unit, ..] if is_hour(unit) => (parse_count(n)?, 60),
        [n, unit, ..] if is_minute(unit) => (parse_count(n)?, 1),
        _ => return None,
    };
    if n == 0 || n > 24 * 60 {
        return None;
    }
    Some((now_ms - n * unit * 60_000, now_ms))
}

fn is_hour(w: &str) -> bool {
    matches!(w, "hour" | "hours" | "hr" | "hrs")
}

fn is_minute(w: &str) -> bool {
    matches!(w, "minute" | "minutes" | "min" | "mins")
}

fn parse_count(w: &str) -> Option<i64> {
    if let Ok(n) = w.parse() {
        return Some(n);
    }
    let words = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven",
        "twelve",
    ];
    words.iter().position(|&s| s == w).map(|i| i as i64 + 1)
}

/// Lowercased words; only alphanumerics and `-` survive (keeps ISO dates).
fn normalize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn has_word(words: &[String], word: &str) -> bool {
    words.iter().any(|w| w == word)
}

fn has_phrase(words: &[String], phrase: &[&str]) -> bool {
    words.windows(phrase.len()).any(|w| w == phrase)
}

fn at_ms(date: Date, hour: i8, tz: &TimeZone) -> Option<i64> {
    let (date, hour) = if hour == 24 {
        (date.checked_add(1.day()).ok()?, 0)
    } else {
        (date, hour)
    };
    Some(
        date.at(hour, 0, 0, 0)
            .to_zoned(tz.clone())
            .ok()?
            .timestamp()
            .as_millisecond(),
    )
}

fn day_range(date: Date, h0: i8, h1: i8, tz: &TimeZone) -> Option<(i64, i64)> {
    Some((at_ms(date, h0, tz)?, at_ms(date, h1, tz)?))
}

fn day_range_between(start: Date, end: Date, tz: &TimeZone) -> Option<(i64, i64)> {
    Some((at_ms(start, 0, tz)?, at_ms(end, 0, tz)?))
}

fn week_start(today: Date) -> Option<Date> {
    today
        .checked_sub(i64::from(today.weekday().to_monday_zero_offset()).days())
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil;

    fn now() -> Zoned {
        // Wed 2026-08-26 14:30 UTC.
        civil::date(2026, 8, 26)
            .at(14, 30, 0, 0)
            .to_zoned(TimeZone::UTC)
            .unwrap()
    }

    fn ms(date: civil::Date, hour: i8) -> i64 {
        at_ms(date, hour, &TimeZone::UTC).unwrap()
    }

    #[test]
    fn this_morning() {
        let d = civil::date(2026, 8, 26);
        assert_eq!(
            parse("what did I do this morning?", &now()),
            Some((ms(d, 0), ms(d, 12)))
        );
    }

    #[test]
    fn yesterday_qualified_and_whole() {
        let y = civil::date(2026, 8, 25);
        assert_eq!(
            parse("yesterday afternoon", &now()),
            Some((ms(y, 12), ms(y, 18)))
        );
        assert_eq!(
            parse("what happened yesterday", &now()),
            Some((ms(y, 0), ms(y, 24)))
        );
    }

    #[test]
    fn last_n_units() {
        let n = now().timestamp().as_millisecond();
        assert_eq!(parse("the last hour", &now()), Some((n - 3_600_000, n)));
        assert_eq!(parse("past 2 hours", &now()), Some((n - 2 * 3_600_000, n)));
        assert_eq!(
            parse("last thirty minutes", &now()),
            None // number word not in the small map
        );
        assert_eq!(parse("last 30 minutes", &now()), Some((n - 30 * 60_000, n)));
        assert_eq!(
            parse("last two hours", &now()),
            Some((n - 2 * 3_600_000, n))
        );
    }

    #[test]
    fn weeks_and_weekdays() {
        let mon = civil::date(2026, 8, 24);
        assert_eq!(
            parse("this week", &now()),
            Some((ms(mon, 0), ms(civil::date(2026, 8, 27), 0)))
        );
        assert_eq!(
            parse("last week", &now()),
            Some((ms(civil::date(2026, 8, 17), 0), ms(mon, 0)))
        );
        // Wednesday = today; Thursday = last week's.
        assert_eq!(
            parse("on wednesday", &now()),
            Some((
                ms(civil::date(2026, 8, 26), 0),
                ms(civil::date(2026, 8, 27), 0)
            ))
        );
        assert_eq!(
            parse("on thursday", &now()),
            Some((
                ms(civil::date(2026, 8, 20), 0),
                ms(civil::date(2026, 8, 21), 0)
            ))
        );
    }

    #[test]
    fn iso_date_and_none() {
        assert_eq!(
            parse("on 2026-08-20 what happened", &now()),
            Some((
                ms(civil::date(2026, 8, 20), 0),
                ms(civil::date(2026, 8, 21), 0)
            ))
        );
        assert_eq!(parse("what is the chronicle project", &now()), None);
    }
}
