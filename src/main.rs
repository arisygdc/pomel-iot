use core::str;
use anyhow::Error;
use embedded_svc::http::client::Client;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop, hal::{delay::FreeRtos, gpio::{Gpio5, Gpio6, OutputPin}, prelude::Peripherals}, http::client::{Configuration as HttpConfiguration, EspHttpConnection}, ipv4::IpInfo, nvs::EspDefaultNvsPartition, sntp::{EspSntp, SyncStatus}, sys::EspError, wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi}
};
use log::{info, warn};
use relay::{DoubleRelay, DoubleRelayStatus, RelayQuery, SetState};
use serde::Deserialize;
use telegram::TelePool;
use util::Queue;

mod relay;
mod telegram;
pub mod util;

#[derive(Deserialize, Debug)]
struct AppConfig {
    wifi: WifiConfig,
    telegram: TelegramConfig,
}

#[derive(Deserialize, Debug)]
pub struct WifiConfig {
    ssid: String,
    password: String
}

#[derive(Deserialize, Debug)]
pub struct TelegramConfig {
    api_base: String,
    bot_token: String
}

fn load_config() -> AppConfig {
    toml::from_str(include_str!("../cfg.toml")).expect("Failed to parse config")
}

const TELE_FETCH_LIMIT: usize = 3;

fn main() -> anyhow::Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?,
        sys_loop,
    )?;

    let cfg = load_config();
    connect_wifi(&mut wifi, &cfg.wifi)?;
    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
    info!("Wifi DHCP info: {:?}", ip_info);

    sync_ntp()?;


    let http_connection = EspHttpConnection::new(&HttpConfiguration {
        use_global_ca_store: true,
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        ..Default::default()
    })?;

    let client = Client::wrap(http_connection);
    let mut tele_pool: TelePool<TELE_FETCH_LIMIT> = TelePool::new(client, &cfg.telegram);

    // INITIALIZE PIN
    let (first_pin, second_pin) = unsafe {
        (Gpio5::new(), Gpio6::new())
    };
    let mut relay = DoubleRelay::new(first_pin, second_pin);

    let mut buffer = [0u8; 1024];

    let mut message_queue = Queue::default();
    loop {
        FreeRtos::delay_ms(5000);
        
        let connect = ensure_wifi_connected(&mut wifi, &cfg.wifi);
        match connect {
            Ok(ip_info) => {
                info!("Wifi DHCP info: {:?}", ip_info);
            },
            Err(err) => {
                warn!("err: {:?}", err);
                continue;
            }
        };

        let rsvc = relay_service(&mut relay);
        if let Err(err) = rsvc {
            let send_result = tele_pool.send_message(&err.to_string());
            if let Err(err) = send_result {
                warn!("{}", err);
                message_queue.enqueue(err.to_string());
            }
        }
    
        let tele_notif = get_tele_notif(&mut tele_pool, &mut buffer);
        match tele_notif {
            Ok(notification) => notification
                .into_iter()
                .for_each(|each| {
                    match run_query(&each, &mut relay) {
                        Ok(s) => { message_queue.enqueue(s.to_string()); },
                        Err(err) => { message_queue.enqueue(err.to_string()); }
                    }
                }),
            Err(err) => { warn!("failed to get updates: {}", err); }
        };


        const MAX_ITER: usize = 8;
        let mut iter_cnt = 1;
        while let Some(msg) = message_queue.dequeue() {
            let sent_result = tele_pool.send_message(&msg);
            if sent_result.is_err() {
                message_queue.insert_head(msg);
            }
            
            if iter_cnt == MAX_ITER {
                break;
            }

            iter_cnt += 1;
        }
    }
}

fn relay_service<R1, R2>(relay: &mut DoubleRelay<'_, R1, R2>) -> anyhow::Result<()>
    where 
        R1: OutputPin,
        R2: OutputPin
{
    let events = relay.pool_event();
    for event in events.into_iter().flatten() {
        let addr = relay.resolve_addr(event.name).unwrap();
        if !event.run_deadline {
            continue;
        }

        let set_result = relay.set(addr, SetState::Stop);
        if let Err(err) = set_result {
            return Err(Error::msg(format!("cannot stop {} when deadline exceed, reason: {}", event.name, err)));
        }
    }
    Ok(())
}

fn get_tele_notif(tele_pool: &mut TelePool<TELE_FETCH_LIMIT>, buffer: &mut [u8]) -> anyhow::Result<Vec<BotQuery>> {
    let incoming_message = tele_pool.pool_fetch(buffer)?;
    let collect = incoming_message.result
        .into_iter()
        .filter(|updt| updt.message.text.starts_with('/'))
        .map(|v| BotQuery {q: v.message.text}).collect();

    Ok(collect)
}

#[derive(Default)]
pub struct BotQuery {
    pub q: String
}


const INVALID_CMD: &str = "Invalid Command";
const INVALID_UNIT: &str = "Invalid unit, example: 1h (one hours)";

fn run_query<'a, R1, R2> (
    q: &BotQuery, 
    relay: &'a mut DoubleRelay<'_, R1, R2>
) -> anyhow::Result<DoubleRelayStatus<'a>> 
    where 
        R1: OutputPin,
        R2: OutputPin
{
    let mut split = q.q.split(' ');
    let top_cmd = split.next().ok_or(Error::msg(INVALID_CMD))?;
    match top_cmd {
        "relay" => {
            let mut rlq = RelayQuery::default();
            let r_name = split
                .next()
                .ok_or(Error::msg(INVALID_CMD))?;
            rlq.name = Some(r_name);
            
            let r_instruction = split
                .next()
                .ok_or(Error::msg(INVALID_CMD))?;

            let r_instruction = match r_instruction {
                "on" => true,
                "off" => false,
                _ => return Err(Error::msg(INVALID_CMD)),
            };
            
            rlq.instruction = Some(r_instruction);

            if let Some(r_pred) = split.next() {
                rlq.duration = match r_pred.eq("for") {
                    true => {
                        let dur_str = split.next().ok_or(Error::msg("expected \"... for [duration]\""))?;
                        if dur_str.len() < 2 {
                            return Err(Error::msg(INVALID_UNIT));
                        }

                        let (dur, unit) = dur_str
                            .split_at(dur_str.len() - 1);
                        let unit = unit.as_bytes()[0];

                        let mul = match unit {
                            b'm' => 60,
                            b'h' => 3600,
                            _ => return Err(Error::msg(INVALID_UNIT))
                        };

                        let duration = dur.parse::<u32>()
                            .map_err(|_| Error::msg(INVALID_UNIT))?;

                        Some(duration * mul)
                        
                    }, false => return Err(Error::msg("no matching pattern"))
                };
            }

            relay.interprete(rlq)
        }
        _ => Err(Error::msg("unregister command")),
        
    }
}

fn sync_ntp() -> anyhow::Result<()> {
    let sntp = EspSntp::new_default()?;
    println!("Synchronizing with NTP Server");
    while sntp.get_sync_status() != SyncStatus::Completed { FreeRtos::delay_ms(10) }
    println!("Time Sync Completed");
    Ok(())
}

fn ensure_wifi_connected(wifi: &mut BlockingWifi<EspWifi<'static>>, config: &WifiConfig) -> Result<IpInfo, EspError> {
    if !wifi.is_connected()? {
        connect_wifi(wifi, config)?;
    }

    wifi.wifi().sta_netif().get_ip_info()
}

fn connect_wifi(wifi: &mut BlockingWifi<EspWifi<'static>>, config: &WifiConfig) -> Result<(), EspError> {
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
    info!("Wifi started");

    wifi.connect()?;
    info!("Wifi connected");

    wifi.wait_netif_up()?;
    info!("Wifi netif up");

    Ok(())
}