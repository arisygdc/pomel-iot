use core::str;

use anyhow::Error;
use embedded_svc::http::client::Client;
use esp_idf_svc::http::client::{self, EspHttpConnection};
use log::info;
use serde::{Deserialize, Serialize};

use crate::TelegramConfig;

pub struct TelePool<'cfg, const FETCH_LIMIT: usize> {
    client: Option<Client<EspHttpConnection>>,
    last_updtid: u32,
    config: &'cfg TelegramConfig
}

impl<'cfg, const FETCH_LIMIT: usize> TelePool<'cfg, FETCH_LIMIT> {
    /// this is empty client connection
    /// call [`TelePool::set_connection`]
    /// 
    /// recomend to [`TelePool::reset_connection`] after use
    #[inline]
    pub fn new(config: &'cfg TelegramConfig) -> Self {
        Self {
            client: None,
            last_updtid: 0,
            config
        }
    }

    pub  fn reset_connection(&mut self) {
        self.client = None;
    }

    pub fn set_connection(&mut self, conn: EspHttpConnection) {
        self.client = Some(Client::wrap(conn));
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
        
        let client = self.client.as_mut().unwrap();
        let request = client.get(&url)?;
        let response = request.submit()?;
        let status = response.status();

        info!("Response code: {}\n", status);

        let updates: Updates = try_read(buf, response)?;
        if let Some(update) = updates.result.last() {
            self.last_updtid = update.update_id;
        }
        
        Ok(updates)
    }

    pub fn send_message(&mut self, msg: SendMessage) -> anyhow::Result<()> {
        let headers = [("Content-Type", "application/json")];
        let url = format!("{}/bot{}/sendMessage", self.config.api_base, self.config.bot_token);

        let client = self.client.as_mut().unwrap();
        let mut request = client.post(url.as_ref(), &headers)?;

        let buf = serde_json::to_vec(&msg)?;
        request.write(&buf)?;

        let response = request.submit()?;
        let status = response.status();

        if !matches!(status, 200..299) {
            return Err(Error::msg(
                format!("code {}: {:?}", 
                response.status(), 
                response.status_message()
            )));
        }
        
        Ok(())
    }
}

#[derive(Serialize)]
pub struct SendMessage {
    pub chat_id: u32,
    pub text: String
}

impl SendMessage {
    pub fn into_bytes(self) -> Vec<u8> {
        let mut bytes = Vec::new();
        
        let chat_id = self.chat_id.to_be_bytes();
        bytes.extend_from_slice(&chat_id);
        bytes.extend_from_slice(self.text.as_bytes());
        bytes
    }

    pub fn from_bytes(buf: &[u8]) -> Self {
        assert!(buf.len() > 5);
        let s = unsafe { str::from_utf8_unchecked(&buf[4..]) };
        Self { 
            chat_id: u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]), 
            text: s.to_owned()
        }
    }
}

fn try_read<'de, T: Deserialize<'de>>(buf: &'de mut [u8], response: client::Response<&mut EspHttpConnection>) -> anyhow::Result<T> {
    let bytes_read = embedded_svc::utils::io::try_read_full(
        response,
        buf,
    )
    .map_err(|e| e.0)?;

    let res_body = std::str::from_utf8(&buf[..bytes_read])?;
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
    pub chat: Chat,
    pub text: String,
}

#[derive(Deserialize, Debug)]
pub struct Chat {
    pub id: u32
}
