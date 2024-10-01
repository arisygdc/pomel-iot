use anyhow::Error;
use core::str;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::{
        delay::FreeRtos,
        gpio::{OutputPin, PinDriver},
        prelude::Peripherals,
    },
    http::client::{Configuration as HttpConfiguration, EspHttpConnection},
    nvs::EspDefaultNvsPartition,
    wifi::{BlockingWifi, EspWifi},
};
use log::{info, warn};
use queue::MsgFMQueue;
use relay::{DoubleRelay, DoubleRelayStatus, RelayQuery, SetState};
use serde::Deserialize;
use std::time::Duration;
use telegram::{SendMessage, TeleAPI};
use util::{connect_wifi, ensure_wifi_connected, sync_ntp};

pub mod queue;
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
    password: String,
}

#[derive(Deserialize, Debug)]
pub struct TelegramConfig {
    api_base: String,
    bot_token: String,
}

fn load_config() -> AppConfig {
    toml::from_str(include_str!("../cfg.toml")).expect("Failed to parse config")
}

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

    const TELE_FETCH_LIMIT: usize = 1;
    let mut tele_api = TeleAPI::new(&cfg.telegram, TELE_FETCH_LIMIT);

    // INITIALIZE PIN
    let mut relay = DoubleRelay::new(peripherals.pins.gpio5, peripherals.pins.gpio6);

    let mut message_queue = MsgFMQueue::new(nvs)?;
    'm: loop {
        info!("--- main loop ---");
        for _ in 0..5 {
            FreeRtos::delay_ms(10_000);

            let rsvc = relay_service(&mut relay, &mut message_queue);
            if let Err(err) = rsvc {
                warn!("{:?}", err);
                let http_connection = create_http_connection()?;
                let mut tele_pool = tele_api.create_client(http_connection);
                let msg = SendMessage {
                    chat_id: err.order_by,
                    text: err.message,
                };
                tele_pool.send_message(msg).unwrap();
                critical_section(&mut relay, &mut message_queue);
            }

            const MAX_SEND_EFFORT: usize = 8;
            let send_result =
                send_message_queue(&mut tele_api, &mut message_queue, MAX_SEND_EFFORT);
            if let Err(err) = send_result {
                warn!("send message from queue error: {}", err)
            }
        }

        let connect = ensure_wifi_connected(&mut wifi, &cfg.wifi);
        if let Err(err) = connect {
            warn!("err: {:?}", err);
            continue 'm;
        }

        let tele_notif = {
            let mut buffer = [0u8; 1024];
            get_tele_notif(&mut tele_api, &mut buffer)
        };

        match tele_notif {
            Ok(notification) => notification.into_iter().for_each(|each| {
                let text = if each.is_command {
                    match run_command(&each, &mut relay) {
                        Ok(s) => s.to_string(),
                        Err(err) => err.to_string(),
                    }
                } else {
                    String::from("command starts with '/'")
                };

                let msg = SendMessage {
                    chat_id: each.chat_id,
                    text,
                };
                message_queue.enqueue(msg);
            }),
            Err(err) => {
                warn!("failed to get updates: {}", err);
            }
        };
    }
}

fn create_http_connection() -> anyhow::Result<EspHttpConnection> {
    let http_config = HttpConfiguration {
        use_global_ca_store: true,
        crt_bundle_attach: Some(esp_idf_svc::sys::esp_crt_bundle_attach),
        timeout: Some(Duration::from_secs(15)),
        ..Default::default()
    };
    EspHttpConnection::new(&http_config).map_err(Into::into)
}

fn relay_service<R1, R2>(
    relay: &mut DoubleRelay<'_, R1, R2>,
    message_queue: &mut MsgFMQueue,
) -> Result<(), RelayServiError>
where
    R1: OutputPin,
    R2: OutputPin,
{
    let events = relay.pool_event();
    info!("events: {:?}", events);
    for event in events.into_iter().flatten() {
        let addr = relay.resolve_addr(event.name).unwrap();
        if !event.run_deadline {
            continue;
        }

        let msg = {
            let status = relay.get_status(addr);
            let r_status = match status {
                DoubleRelayStatus::Single(ref s) => s,
                DoubleRelayStatus::Both(_) => panic!(),
            };

            let inf = r_status.run_info.unwrap();
            (
                inf.order_by,
                SendMessage {
                    chat_id: inf.order_by,
                    text: format!(
                        "Deadline... Turned off {}\nStart: {}\nFinish: {}",
                        r_status.name, inf.start_at, inf.end_at
                    ),
                },
            )
        };

        let set_result = relay.set(addr, SetState::Stop);

        if let Err(err) = set_result {
            let err = RelayServiError {
                message: format!(
                    "cannot stop {} when deadline exceed, reason: {}",
                    event.name, err
                ),
                order_by: msg.0,
            };
            return Err(err);
        }

        message_queue.enqueue(msg.1);
    }
    Ok(())
}

fn critical_section<R1, R2>(relay: &mut DoubleRelay<'_, R1, R2>, message_queue: &mut MsgFMQueue)
where
    R1: OutputPin,
    R2: OutputPin,
{
    let critical_retry = 12;
    for _ in 0..critical_retry {
        let retry = relay_service(relay, message_queue);
        if retry.is_ok() {
            return;
        }
        // delay 5 minutes
        FreeRtos::delay_ms(300_000);
    }

    panic!()
}

#[derive(Debug)]
struct RelayServiError {
    order_by: u32,
    message: String,
}

fn send_message_queue(
    tele_api: &mut TeleAPI,
    message_queue: &mut MsgFMQueue,
    max_try: usize,
) -> anyhow::Result<()> {
    if message_queue.is_empty() {
        return Ok(());
    }

    let http_connection = create_http_connection()?;
    let mut tele_pool = tele_api.create_client(http_connection);

    let mut buffer = [0_u8; 512];

    for _ in 0..max_try {
        let msg = match message_queue.peek(&mut buffer) {
            None => break,
            Some(text) => text,
        };

        info!("send chat: {}, text: {}", msg.chat_id, msg.text);
        let sent_result = tele_pool.send_message(msg);
        match sent_result {
            Ok(_) => {
                message_queue.remove_first();
            }
            Err(err) => return Err(err),
        }
        FreeRtos::delay_ms(1000);
    }

    Ok(())
}

fn get_tele_notif(tele_api: &mut TeleAPI, buffer: &mut [u8]) -> anyhow::Result<Vec<BotQuery>> {
    let http_connection = create_http_connection()?;
    let mut tele_client = tele_api.create_client(http_connection);

    let incoming_message = tele_client.pool_fetch(buffer)?;
    let collect = incoming_message
        .result
        .into_iter()
        .map(|mut v| BotQuery {
            chat_id: v.message.chat.id,
            is_command: v.message.text.starts_with('/'),
            q: v.message.text.split_off(1),
        })
        .collect();

    info!("collect: {:?}", collect);
    Ok(collect)
}

#[derive(Default, Debug)]
pub struct BotQuery {
    pub chat_id: u32,
    pub q: String,
    pub is_command: bool,
}

const INVALID_CMD: &str = "Invalid Command";
const INVALID_UNIT: &str = "Invalid unit, example: 1h (one hours)";

fn run_command<'a, R1, R2>(
    q: &BotQuery,
    relay: &'a mut DoubleRelay<'_, R1, R2>,
) -> anyhow::Result<DoubleRelayStatus<'a>>
where
    R1: OutputPin,
    R2: OutputPin,
{
    let mut split = q.q.split(' ');
    let top_cmd = split.next().ok_or(Error::msg(INVALID_CMD))?;

    match top_cmd {
        "relay" => {
            let mut rlq = RelayQuery::new(q.chat_id);
            let r_name = split.next().ok_or(Error::msg(INVALID_CMD))?;
            rlq.name = Some(r_name);

            let r_instruction = split.next().ok_or(Error::msg(INVALID_CMD))?;

            let r_instruction = match r_instruction {
                "on" => true,
                "off" => false,
                _ => return Err(Error::msg(INVALID_CMD)),
            };

            rlq.instruction = Some(r_instruction);

            if let Some(r_pred) = split.next() {
                rlq.duration = match r_pred.eq("for") {
                    true => {
                        let dur_str = split
                            .next()
                            .ok_or(Error::msg("expected \"... for [duration]\""))?;
                        if dur_str.len() < 2 {
                            return Err(Error::msg(INVALID_UNIT));
                        }

                        let (dur, unit) = dur_str.split_at(dur_str.len() - 1);
                        let unit = unit.as_bytes()[0];

                        let mul = match unit {
                            b'm' => 60,
                            b'h' => 3600,
                            _ => return Err(Error::msg(INVALID_UNIT)),
                        };

                        let duration = dur.parse::<u32>().map_err(|_| Error::msg(INVALID_UNIT))?;

                        Some(duration * mul)
                    }
                    false => return Err(Error::msg("no matching pattern")),
                };
            }

            relay.interprete(rlq)
        }
        _ => Err(Error::msg("unregister command")),
    }
}
