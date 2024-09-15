use core::str;
use std::fmt::Display;
use std::time::{SystemTime, UNIX_EPOCH};

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use log::{info, warn};

const WIB_OFFSET: u64 = 25200;

// pub enum QueueError {
//     InsertAtFull,
//     GetFromEmpty,
//     EspError(EspError)
// }

enum QTarget {
    Head = 0, 
    Tail = 1
}

impl Display for QTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            QTarget::Head => "head",
            QTarget::Tail => "tail"
        };
        write!(f, "{}", s)
    }
}

// Ring buffer
pub struct FMemQueue {
    storage: EspNvs<NvsDefault>,
    /// head = addr[0]
    /// tail = addr[1]
    addr: [u8; 2],
}

impl FMemQueue {
    const QUEUE_LIMIT: u8 = 20;
    const START_INDEX: u8 = 0x41;

    pub fn new(partition: EspDefaultNvsPartition) -> anyhow::Result<Self> {
        let storage = EspNvs::new(partition, "queue", true)?;
        let head = storage.get_u8(&QTarget::Head.to_string())?.unwrap_or(Self::START_INDEX);
        let tail = storage.get_u8(&QTarget::Tail.to_string())?.unwrap_or(Self::START_INDEX);

        Ok(Self { 
            storage,
            addr: [head, tail],
        })
    }

    fn increment_address(&mut self, target: QTarget) {
        let key = target.to_string();
        let idx = target as usize;

        self.addr[idx] = self.increment(self.addr[idx]);
        self.storage.set_u8(&key, self.addr[idx]).unwrap();
    }

    pub fn enqueue(&mut self, value: &str) -> bool {
        let is_full = self.is_full();
        if is_full {
            warn!("queue full: {}", is_full);
            return !is_full;
        }
        let tail = unsafe { str::from_utf8_unchecked(&self.addr[1..])};
        info!("set queue [{}] {}", tail, value);
        self.storage.set_str(tail, value).unwrap();

        // increment tail
        self.increment_address(QTarget::Tail);
        false
    }

    pub fn dequeue<'a>(&mut self, buf: &'a mut [u8]) -> Option<&'a str> {
        let peek = self.peek(buf)?;
        match self.remove_first() {
            true => panic!(),
            false => Some(peek)
        }
    }

    pub fn peek<'a>(&mut self, buf: &'a mut [u8]) -> Option<&'a str> {
        if self.is_empty() {
            warn!("queue empty: {}", self.is_empty());
            return None;
        }

        let head = unsafe { str::from_utf8_unchecked(&self.addr[0..1])};
        info!("get queue [{}]", head);
        let get_val = self.storage.get_str(head, buf).unwrap();

        match get_val {
            None => panic!(),
            Some(rslt) => Some(rslt)
        }
    }

    pub fn remove_first(&mut self) -> bool {
        if self.is_empty() {
            return false;
        }

        let head = unsafe { str::from_utf8_unchecked(&self.addr[0..1])};

        // increment head
        self.storage.remove(head).unwrap();
        self.increment_address(QTarget::Head);
        true
    }

    pub fn is_empty(&self) -> bool {
        self.addr[0] == self.addr[1]
    }

    pub fn is_full(&self) -> bool {
        let inc_tail = self.increment(self.addr[1]);
        let head = self.addr[0];
        inc_tail == head
    }
    
    fn increment(&self, index: u8) -> u8 {
        if index == Self::START_INDEX + Self::QUEUE_LIMIT - 1 {
            Self::START_INDEX
        } else {
            index + 1
        }
    }
}


#[derive(Clone, PartialEq, PartialOrd)]
pub struct Time(u64);
impl Time {
    pub fn new(t: u64) -> Self {
        Self(t)
    }

    pub fn now() -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
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