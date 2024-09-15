use core::str;
use std::fmt::Display;

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use log::{info, warn};

use crate::telegram::SendMessage;

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

pub struct MsgFMQueue {
    inner: FMemQueue
}

impl MsgFMQueue {
    pub fn new(partition: EspDefaultNvsPartition) -> anyhow::Result<Self> {
        Ok(Self{ inner: FMemQueue::new(partition)? })
    }
    
    pub fn enqueue(&mut self, msg: SendMessage) -> bool {
        let buf = msg.into_bytes();
        self.inner.enqueue(&buf)
    }

    pub fn peek(&mut self, buf: &mut [u8]) -> Option<SendMessage> {
        let peek = self.inner.peek(buf)?;
        Some(SendMessage::from_bytes(peek))
    }

    pub fn remove_first(&mut self) -> bool {
        self.inner.remove_first()
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

    pub fn enqueue(&mut self, value: &[u8]) -> bool {
        let is_full = self.is_full();
        if is_full {
            warn!("queue full: {}", is_full);
            return !is_full;
        }
        let tail = unsafe { str::from_utf8_unchecked(&self.addr[1..])};

        info!("set queue [{}]", tail);
        self.storage.set_blob(tail, value).unwrap();

        // increment tail
        self.increment_address(QTarget::Tail);
        false
    }

    pub fn dequeue<'a>(&mut self, buf: &'a mut [u8]) -> Option<&'a [u8]> {
        let peek = self.peek(buf)?;
        match self.remove_first() {
            true => panic!(),
            false => Some(peek)
        }
    }

    pub fn peek<'a>(&mut self, buf: &'a mut [u8]) -> Option<&'a [u8]> {
        if self.is_empty() {
            warn!("queue empty: {}", self.is_empty());
            return None;
        }

        let head = unsafe { str::from_utf8_unchecked(&self.addr[0..1])};
        info!("get queue [{}]", head);
        let get_val = self.storage.get_blob(head, buf).unwrap();

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