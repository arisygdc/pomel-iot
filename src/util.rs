use std::fmt::Display;
use std::time::{SystemTime, UNIX_EPOCH};

const WIB_OFFSET: u64 = 25200;

#[derive(Clone, PartialEq, PartialOrd)]
pub struct Time(u64);
impl Time {
    pub fn new(t: u64) -> Self {
        Self(t)
    }

    pub fn now() -> Self {
        let now = sys_now();
        Self(now)
    }

    fn is_leap_year(year: i64) -> bool {
        (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
    }
    
    fn day_of_year_to_date(year: i64, day_of_year: u32) -> (u32, u32) {
        let days_in_month = [31, 28 + Self::is_leap_year(year) as u32, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        
        let mut month = 0;
        let mut day = day_of_year;
    
        while day >= days_in_month[month] {
            day -= days_in_month[month];
            month += 1;
        }
    
        (month as u32 + 1, day + 1)
    }
    
    fn seconds_to_hms(seconds: u32) -> (u32, u32, u32) {
        let hours = seconds / 3600;
        let minutes = (seconds % 3600) / 60;
        let seconds = seconds % 60;
    
        (hours, minutes, seconds)
    }
}

impl Display for Time {
    /// Note: convert to WIB when convert to string
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let epoch = self.0 + WIB_OFFSET;
        let days_since_epoch = epoch / 86400; // 86400 one day
        let seconds_in_day = epoch % 86400;

        let mut year = 1970;
        let mut days_in_year = if Self::is_leap_year(year) { 366 } else { 365 };
        
        let mut days = days_since_epoch;
        while days >= days_in_year {
            days -= days_in_year;
            year += 1;
            days_in_year = if Self::is_leap_year(year) { 366 } else { 365 };
        }

        let (month, day) = Self::day_of_year_to_date(year, days as u32);
        let (hour, minute, second) = Self::seconds_to_hms(seconds_in_day as u32);

        write!(f, "{:04}-{:02}-{:02} {:02}:{:02}:{:02} WIB", year, month, day, hour, minute, second)
    }
}

#[inline]
pub fn sys_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}