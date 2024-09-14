use std::fmt::Display;
use std::time::{SystemTime, UNIX_EPOCH};
use std::ptr;

const WIB_OFFSET: u64 = 25200;

pub struct Queue<T> {
    head: *mut Node<T>
}

impl<T> Queue<T> {
    pub fn new(val: T) -> Self {
        let node = Node::new(val);
        let p = allocate_pbox(node);
        Self { head: p }
    }

    pub fn enqueue(&mut self, val: T) { 
        let node = Node::new(val);
        let p = allocate_pbox(node);

        let head = self.head;
        if head.is_null() {
            self.head = p;
            return;
        }

        unsafe { 
            let gap_node = Self::traverse(head);
            (*gap_node).next = p;
        };
    }

    pub fn insert_head(&mut self, val: T) {
        let mut node = Node::new(val);
        
        if !self.head.is_null() {
            node.next = self.head;
        }
        
        let p = allocate_pbox(node);
        self.head = p;
    }

    #[inline]
    unsafe fn traverse(mut node: *mut Node<T>) -> *mut Node<T> {
        while !(*node).next.is_null() {
            node = (*node).next;
        }

        node
    }

    pub fn dequeue(&mut self) -> Option<T> {
        if self.head.is_null() {
            return None;
        }

        let n_head = unsafe { Box::from_raw(self.head) };
        self.head = n_head.next;

        Some(n_head.value)
    }

}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self { head: ptr::null_mut() }
    }
}

impl<T> Drop for Queue<T> {
    fn drop(&mut self) {
        while self.dequeue().is_some() {}
    }
}

struct Node<T> {
    value: T,
    next: *mut Node<T>
}

impl<T> Node<T> {
    #[inline]
    fn new(val: T) -> Self {
        Self { value: val, next: ptr::null_mut() }
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

#[inline]
fn allocate_pbox<T>(val: T) -> *mut T {
    Box::into_raw(Box::new(val))
}