use core::str;
use anyhow::Error;
use embedded_svc::http::client::Client;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop, hal::{delay::FreeRtos, gpio::{Gpio5, Gpio6, OutputPin}, prelude::Peripherals}, http::client::{Configuration as HttpConfiguration, EspHttpConnection}, ipv4::IpInfo, nvs::EspDefaultNvsPartition, sntp::{EspSntp, SyncStatus}, sys::EspError, wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi}
};
use log::{info, warn};
use relay::{DoubleRelay, RelayQuery, SetState};
use telegram::TelePool;

mod relay;
mod telegram;
pub mod helper;

// FIXME: Not working
#[toml_cfg::toml_config]
pub struct AppConfig {
    #[default("")]
    wifi_ssid: &'static str,
    #[default("")]
    wifi_password: &'static str,
    #[default("https://api.telegram.org")]
    telegram_api_base: &'static str,
    #[default("")]
    telegram_bot_token: &'static str,
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

    connect_wifi(&mut wifi)?;
    let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
    info!("Wifi DHCP info: {:?}", ip_info);

    sync_ntp()?;

    let http_connection = EspHttpConnection::new(&HttpConfiguration {
        use_global_ca_store: true,
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        ..Default::default()
    })?;

    let client = Client::wrap(http_connection);
    let mut tele_pool: TelePool<TELE_FETCH_LIMIT> = TelePool::new(client);

    // INITIALIZE PIN
    let (first_pin, second_pin) = unsafe {
        (Gpio5::new(), Gpio6::new())
    };
    let mut relay = DoubleRelay::new(first_pin, second_pin);

    let mut buffer = [0u8; 1024];

    loop {
        FreeRtos::delay_ms(5000);
        
        let connect = ensure_wifi_connected(&mut wifi);
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
            let send_result = tele_pool.send_message(err.to_string());
            if let Err(err) = send_result {
                warn!("{}", err);
                FreeRtos::delay_ms(5000);
                continue;
            }
        }
    
        let tele_notif = get_tele_notif(&mut tele_pool, &mut buffer);
        let query_list = match tele_notif {
            Ok(notification) => notification,
            Err(err) => {
                warn!("failed to get updates: {}", err);
                continue;
            }
        };

        for query in query_list {
            // TODO: callback
            run_query(&query, &mut relay).unwrap();
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
const INVALID_UNIT: &str = "unknown unit, example: 1h (one hours)";

fn run_query<R1, R2> (
    q: &BotQuery, 
    relay: &mut DoubleRelay<'_, R1, R2>
) -> anyhow::Result<()> 
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

fn ensure_wifi_connected(wifi: &mut BlockingWifi<EspWifi<'static>>) -> Result<IpInfo, EspError> {
    if !wifi.is_connected()? {
        connect_wifi(wifi)?;
    }

    wifi.wifi().sta_netif().get_ip_info()
}

fn connect_wifi(wifi: &mut BlockingWifi<EspWifi<'static>>) -> Result<(), EspError> {
    let wifi_configuration: Configuration = Configuration::Client(ClientConfiguration {
        ssid: APP_CONFIG.wifi_ssid.try_into().unwrap(),
        bssid: None,
        auth_method: AuthMethod::WPA2Personal,
        password: APP_CONFIG.wifi_password.try_into().unwrap(),
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