use core::str;
use std::time::Duration;
use anyhow::Error;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop, hal::{delay::FreeRtos, gpio::{OutputPin, PinDriver}, prelude::Peripherals}, http::client::{Configuration as HttpConfiguration, EspHttpConnection}, nvs::EspDefaultNvsPartition, sntp::{EspSntp, SyncStatus}, sys::EspError, wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi}
};
use log::{info, warn};
use queue::MsgFMQueue;
use relay::{DoubleRelay, DoubleRelayStatus, RelayQuery, SetState};
use serde::Deserialize;
use telegram::{SendMessage, TelePool};

mod relay;
mod telegram;
pub mod util;
pub mod queue;

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

const TELE_FETCH_LIMIT: usize = 1;

fn main() -> anyhow::Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    let peripherals = Peripherals::take()?;
    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    
    let mut internal_led = PinDriver::output(peripherals.pins.gpio2)?;
    internal_led.set_high()?;
    
    let mut wifi = BlockingWifi::wrap(
        EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs.clone()))?,
        sys_loop,
    )?;
    
    let cfg = load_config();
    info!("Connecting wifi ssid: {}", cfg.wifi.ssid);
    while connect_wifi(&mut wifi, &cfg.wifi).is_err() {
        info!("Reconnect Wifi");
        FreeRtos::delay_ms(1000)
    }

    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
    info!("Wifi DHCP info: {:?}", ip_info);

    sync_ntp()?;

    let mut tele_pool: TelePool<TELE_FETCH_LIMIT> = TelePool::new(&cfg.telegram);

    // INITIALIZE PIN
    let mut relay = DoubleRelay::new(peripherals.pins.gpio5, peripherals.pins.gpio6);

    let mut message_queue = MsgFMQueue::new(nvs)?;
    'm: loop {
        info!("--- main loop ---");
        FreeRtos::delay_ms(10_000);

        // TODO: send feedback for who sent the order
        let rsvc = relay_service(&mut relay);
        if let Err(err) = rsvc {
            warn!("{}", err);
        }

        let connect = ensure_wifi_connected(&mut wifi, &cfg.wifi);
        if let Err(err) = connect {
            warn!("err: {:?}", err);
            continue 'm;
        }
    
        let tele_notif = {
            let mut buffer = [0u8; 1024];
            get_tele_notif(&mut tele_pool, &mut buffer)
        };
        
        match tele_notif {
            Ok(notification) => notification
                .into_iter()
                .for_each(|each| {
                    let text = match run_query(&each, &mut relay) {
                        Ok(s) => s.to_string(),
                        Err(err) => err.to_string()
                    };

                    let msg = SendMessage { chat_id: each.chat_id, text };
                    message_queue.enqueue(msg);
                }),
            Err(err) => { warn!("failed to get updates: {}", err); }
        };


        const MAX_SEND_EFFORT: usize = 8;
        let send_result = send_message_queue(&mut tele_pool, &mut message_queue, MAX_SEND_EFFORT);
        if let Err(err) = send_result {
            warn!("send message from queue error: {}", err)
        }
    }
}

fn create_http_connection() -> anyhow::Result<EspHttpConnection> {
    let http_config = HttpConfiguration  {
        use_global_ca_store: true,
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        timeout: Some(Duration::from_secs(15)),
        ..Default::default()
    };
    EspHttpConnection::new(&http_config).map_err(Into::into)
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

fn send_message_queue(
    tele_pool: &mut TelePool<TELE_FETCH_LIMIT>,
    message_queue: &mut MsgFMQueue,
    max_try: usize
) -> anyhow::Result<()> {
    let http_connection = create_http_connection()?;
    tele_pool.set_connection(http_connection);

    let mut buffer = [0_u8; 256];
        
    for _ in 0..max_try {
        let msg = match message_queue.peek(&mut buffer) {
            None => break,
            Some(text) => text
        };

        info!("send chat: {}, text: {}", msg.chat_id, msg.text);
        let sent_result = tele_pool.send_message(msg);
        match sent_result {
            Ok(_) => { message_queue.remove_first(); }, 
            Err(err) => return Err(err)
        }
        FreeRtos::delay_ms(1000);
    }

    Ok(())
}

fn get_tele_notif(tele_pool: &mut TelePool<TELE_FETCH_LIMIT>, buffer: &mut [u8]) -> anyhow::Result<Vec<BotQuery>> {
    let http_connection = create_http_connection()?;
    tele_pool.set_connection(http_connection);

    let incoming_message = tele_pool.pool_fetch(buffer)?;
    let collect = incoming_message.result
        .into_iter()
        .filter(|updt| updt.message.text.starts_with('/'))
        .map(|v| BotQuery {
            chat_id: v.message.chat.id,
            q: v.message.text
        })
        .collect();
    
    tele_pool.reset_connection();
    Ok(collect)
}

#[derive(Default)]
pub struct BotQuery {
    pub chat_id: u32,
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

fn ensure_wifi_connected(wifi: &mut BlockingWifi<EspWifi<'static>>, config: &WifiConfig) -> Result<(), EspError> {
    if wifi.is_connected()? {
        return Ok(());
    }
    
    connect_wifi(wifi, config)?;
    let ip_info = wifi.wifi().sta_netif().get_ip_info();
    info!("Wifi DHCP info: {:?}", ip_info);
    Ok(())
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
    wifi.connect()?;
    wifi.wait_netif_up()?;
    info!("Wifi connected");
    info!("Wifi netif up");

    Ok(())
}