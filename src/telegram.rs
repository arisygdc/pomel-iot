use embedded_svc::http::client::Client;
use esp_idf_svc::http::client::{self, EspHttpConnection};
use log::info;
use serde::{Deserialize, Serialize};

use crate::TelegramConfig;

pub struct TelePool<'cfg, const FETCH_LIMIT: usize> {
    client: Client<EspHttpConnection>,
    last_updtid: u32,
    send_cnt: u32,
    config: &'cfg TelegramConfig
}

impl<'cfg, const FETCH_LIMIT: usize> TelePool<'cfg, FETCH_LIMIT> {
    #[inline]
    pub fn new(client: Client<EspHttpConnection>, config: &'cfg TelegramConfig) -> Self {
        Self {
            client,
            last_updtid: 0,
            send_cnt: 0,
            config
        }
    }

    pub fn pool_fetch(&mut self, buf: &mut [u8]) -> anyhow::Result<Updates> {
        let url = {
            let offset = match self.last_updtid == 0 {
                true => String::new(),
                false => format!("&offset={}", self.last_updtid+1)
            };

            format!(
                "{}/bot{}/getUpdates?limit={}{}", 
                self.config.api_base, 
                self.config.bot_token,
                FETCH_LIMIT,
                offset
            )
        };
        
        let request = self.client.get(&url)?;

        let response = request.submit()?;
        let status = response.status();

        info!("Response code: {}\n", status);

        let updates: Updates = try_read(buf, response)?;
        if let Some(update) = updates.result.last() {
            self.last_updtid = update.update_id;
        }
        
        Ok(updates)
    }

    pub fn send_message(&mut self, text: &str) -> anyhow::Result<()> {
        let headers = [("Content-Type", "application/json")];
        let url = format!("{}/bot{}/sendMessage", self.config.api_base, self.config.bot_token);
        let request = {
            let mut request = self.client.post(url.as_ref(), &headers)?;

            let message = SendMessage {
                chat_id: self.send_cnt,
                text
            };

            let buf = serde_json::to_vec(&message)?;
            request.write(&buf)?;
            request.flush()?;

            request
        };

        let response = request.submit()?;
        let status = response.status();

        println!("Response code: {}\n", status);
        
        Ok(())
    }
}

#[derive(Serialize)]
struct SendMessage<'a> {
    chat_id: u32,
    text: &'a str
}

fn try_read<'de, T: Deserialize<'de>>(buf: &'de mut [u8], response: client::Response<&mut EspHttpConnection>) -> anyhow::Result<T> {
    let bytes_read = embedded_svc::utils::io::try_read_full(
        response,
        buf,
    )
    .map_err(|e| e.0)?;

    let res_body = std::str::from_utf8(&buf[..bytes_read])?;
    info!("res body: {}", res_body);
    let body: T = serde_json::from_str(res_body)?;
    Ok(body)
}

#[derive(Deserialize, Debug)]
pub struct Updates {
    // pub ok: bool,
    pub result: Vec<Update>,
}

#[derive(Deserialize, Debug)]
pub struct Update {
    pub update_id: u32,
    pub message: Message,
}

#[derive(Deserialize, Debug)]
pub struct Message {
    // pub chat: Chat,
    pub text: String,
}
