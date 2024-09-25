use std::fmt::Display;
use std::time::{SystemTime, UNIX_EPOCH};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_svc::sntp::{EspSntp, SyncStatus};
use log::info;
use esp_idf_svc::sys::EspError;
use esp_idf_svc::wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi};

use crate::WifiConfig;

const WIB_OFFSET: u64 = 25200;

#[derive(Clone, Debug, PartialEq, PartialOrd)]
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

pub fn sync_ntp() -> anyhow::Result<()> {
    let sntp = EspSntp::new_default()?;
    println!("Synchronizing with NTP Server");
    while sntp.get_sync_status() != SyncStatus::Completed { FreeRtos::delay_ms(10) }
    println!("Time Sync Completed");
    Ok(())
}

pub fn ensure_wifi_connected(wifi: &mut BlockingWifi<EspWifi<'static>>, config: &WifiConfig) -> Result<(), EspError> {
    if wifi.is_connected()? {
        return Ok(());
    }
    
    connect_wifi(wifi, config)?;
    let ip_info = wifi.wifi().sta_netif().get_ip_info();
    info!("Wifi DHCP info: {:?}", ip_info);
    Ok(())
}

pub fn connect_wifi(wifi: &mut BlockingWifi<EspWifi<'static>>, config: &WifiConfig) -> Result<(), EspError> {
    let wifi_configuration: Configuration = Configuration::Client(ClientConfiguration {
        ssid: config.ssid.as_str().try_into().unwrap(),
        bssid: None,
        auth_method: AuthMethod::WPA2Personal,
        password: config.password.as_str().try_into().unwrap(),
        channel: None,
        ..Default::default()
    });

    wifi.set_configuration(&wifi_configuration)?;

    wifi.start()?;
    wifi.connect()?;
    wifi.wait_netif_up()?;
    info!("Wifi connected");
    info!("Wifi netif up");

    Ok(())
}