//! Calendar arithmetic for the date picker (proleptic Gregorian, no time zones): ISO parsing and
//! formatting, weekdays, month grids and day/month stepping. Pure, so the picker's behaviour is
//! testable without a window.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime {
    pub date: Date,
    pub hour: u32,
    pub minute: u32,
}

pub fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        2 if is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> Date {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    Date { year: (if m <= 2 { y + 1 } else { y }) as i32, month: m, day: d }
}

impl Date {
    pub fn new(year: i32, month: u32, day: u32) -> Option<Date> {
        ((1..=12).contains(&month) && (1..=9999).contains(&year) && day >= 1 && day <= days_in_month(year, month)).then_some(Date { year, month, day })
    }

    pub fn parse(s: &str) -> Option<Date> {
        let mut it = s.split('-');
        let (y, m, d) = (it.next()?, it.next()?, it.next()?);
        if it.next().is_some() || y.len() != 4 || m.len() != 2 || d.len() != 2 {
            return None;
        }
        Date::new(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?)
    }

    pub fn iso(&self) -> String {
        format!("{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }

    fn ordinal(&self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    /// 0 = Monday … 6 = Sunday.
    pub fn weekday(&self) -> u32 {
        (self.ordinal() + 3).rem_euclid(7) as u32 // 1970-01-01 was a Thursday
    }

    pub fn add_days(&self, n: i64) -> Date {
        civil_from_days(self.ordinal() + n)
    }

    /// Same day-of-month in another month, clamped (Jan 31 + 1 month = Feb 28/29).
    pub fn add_months(&self, n: i32) -> Date {
        let total = self.year * 12 + (self.month as i32 - 1) + n;
        let (year, month) = (total.div_euclid(12), (total.rem_euclid(12) + 1) as u32);
        Date { year, month, day: self.day.min(days_in_month(year, month)) }
    }

    pub fn first_of_month(&self) -> Date {
        Date { day: 1, ..*self }
    }

    /// Six weeks of dates (Monday first) covering the month: the calendar grid.
    pub fn month_grid(&self) -> Vec<Vec<Date>> {
        let first = self.first_of_month();
        let start = first.add_days(-(first.weekday() as i64));
        (0..6).map(|w| (0..7).map(|d| start.add_days(w * 7 + d)).collect()).collect()
    }

    pub fn month_name(&self) -> &'static str {
        ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"][self.month as usize - 1]
    }
}

impl DateTime {
    /// `YYYY-MM-DDTHH:MM`
    pub fn parse(s: &str) -> Option<DateTime> {
        let (d, t) = s.split_once('T')?;
        let (h, m) = t.split_once(':')?;
        if h.len() != 2 || m.len() != 2 {
            return None;
        }
        let (hour, minute) = (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?);
        (hour < 24 && minute < 60).then_some(DateTime { date: Date::parse(d)?, hour, minute })
    }

    pub fn iso(&self) -> String {
        format!("{}T{:02}:{:02}", self.date.iso(), self.hour, self.minute)
    }
}

/// Today's date in UTC (the picker is only a convenience; no time zone database is shipped).
pub fn today_utc() -> Date {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    civil_from_days(secs.div_euclid(86_400))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> Date {
        Date::new(y, m, day).unwrap()
    }

    #[test]
    fn parsing_is_strict() {
        assert_eq!(Date::parse("2026-11-08"), Some(d(2026, 11, 8)));
        for bad in ["2026-2-30", "2026-02-30", "2026-13-01", "26-01-01", "2026/01/01", "2026-01", "2026-01-01-01", "", "abcd-ef-gh"] {
            assert_eq!(Date::parse(bad), None, "{bad}");
        }
        assert_eq!(Date::parse("2024-02-29"), Some(d(2024, 2, 29)));
        assert_eq!(Date::parse("2023-02-29"), None);
        assert_eq!(DateTime::parse("2026-01-02T03:04").unwrap().iso(), "2026-01-02T03:04");
        assert_eq!(DateTime::parse("2026-01-02T24:00"), None);
        assert_eq!(DateTime::parse("2026-01-02"), None);
    }

    #[test]
    fn weekdays_match_known_dates() {
        assert_eq!(d(1970, 1, 1).weekday(), 3); // Thursday
        assert_eq!(d(2000, 1, 1).weekday(), 5); // Saturday
        assert_eq!(d(2024, 2, 29).weekday(), 3); // Thursday
        assert_eq!(d(2026, 11, 8).weekday(), 6); // Sunday
    }

    #[test]
    fn day_and_month_steps_cross_boundaries() {
        assert_eq!(d(2026, 12, 31).add_days(1), d(2027, 1, 1));
        assert_eq!(d(2024, 3, 1).add_days(-1), d(2024, 2, 29));
        assert_eq!(d(2026, 1, 1).add_days(-1), d(2025, 12, 31));
        assert_eq!(d(2026, 1, 31).add_months(1), d(2026, 2, 28), "clamped");
        assert_eq!(d(2024, 1, 31).add_months(1), d(2024, 2, 29));
        assert_eq!(d(2026, 1, 15).add_months(-1), d(2025, 12, 15));
        assert_eq!(d(2026, 11, 8).add_months(14), d(2028, 1, 8));
        assert_eq!(d(2026, 3, 31).add_days(366).iso(), "2027-04-01");
    }

    #[test]
    fn month_grid_starts_on_monday_and_covers_the_month() {
        let g = d(2026, 11, 8).month_grid();
        assert_eq!(g.len(), 6);
        assert!(g.iter().all(|w| w.len() == 7 && w[0].weekday() == 0));
        assert_eq!(g[0][0], d(2026, 10, 26)); // Nov 1st 2026 is a Sunday
        assert!(g.iter().flatten().any(|x| *x == d(2026, 11, 30)));
        assert_eq!(d(2026, 11, 8).month_name(), "November");
    }

    #[test]
    fn today_is_a_plausible_date() {
        assert!(today_utc().year >= 2024);
    }
}
