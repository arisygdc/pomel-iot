use std::fmt::Display;

use anyhow::Error;
use esp_idf_svc::hal::{gpio::{Output, OutputPin, PinDriver}, peripheral::Peripheral};

use crate::util::{sys_now, Time};

#[derive(Clone)]
pub struct RunOrder {
    pub start_at: Time,
    pub end_at: Time
}

impl RunOrder {
    #[inline]
    /// panic when end <= start
    pub fn new(start_at: u64, end_at: u64) -> Self {
        assert!(start_at <= end_at);
        Self{ start_at: Time::new(start_at), end_at: Time::new(end_at) }
    }
}

struct Relay<'drv, R> 
where 
    R: OutputPin
{
    pin: PinDriver<'drv, R, Output>,
    name: &'static str,
    running: Option<RunOrder>,
}

#[derive(Clone)]
pub enum SetState {
    Run(RunOrder),
    Stop
}

pub struct Event {
    /// time to stop the device when its true
    pub run_deadline: bool,
    pub name: &'static str
}

impl<'drv, R> Relay<'drv, R> 
where 
    R: OutputPin
{
    #[inline]
    fn new(pin: PinDriver<'drv, R, Output>, name: &'static str) -> Self {
        Self { pin, name, running: None }
    }

    fn run(&mut self, ord: RunOrder) -> anyhow::Result<()> {
        match self.running {
            None => { self.running = Some(ord); }, 
            Some(_) => return Err(Error::msg(format!("relay {} at ON state, turn off first!", self.name)))
        }

        self.pin.set_high().map_err(Into::into)
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        self.pin.set_low().map_err(Into::into)
    }

    fn set(&mut self, state: SetState) -> anyhow::Result<()> {
        match state {
            SetState::Run(ord) => self.run(ord),
            SetState::Stop => self.stop()
        }
    }

    fn is_run_deadline(&self, now: u64) -> bool {
        if let Some(r) = &self.running {
            return r.end_at <= Time::new(now);
        }
        false
    }

    fn get_status(&self) -> RelayStatus {
        RelayStatus{
            name: self.name,
            run_info: self.running.as_ref()
        }
    }
}

pub struct DoubleRelay<'drv, R1, R2> 
where 
    R1: OutputPin,
    R2: OutputPin
{
    first_relay: Relay<'drv, R1>,
    second_relay: Relay<'drv, R2>,
}

#[derive(Clone, Copy)]
pub enum RelayAddr {
    First = 1,
    Second = 2,
    Both = 3
}

impl<'drv, R1, R2> DoubleRelay<'drv, R1, R2>
where 
    R1: OutputPin,
    R2: OutputPin
{
    #[inline]
    pub fn new(
        first_pin: impl Peripheral<P = R1> + 'drv, 
        second_pin: impl Peripheral<P = R2> + 'drv
    ) -> Self {
        Self {
            first_relay: Relay::new(PinDriver::output(first_pin).unwrap(), "pompa_air"), 
            second_relay: Relay::new(PinDriver::output(second_pin).unwrap(), "lain_lain"), 
        }
    }

    pub fn set(&mut self, target: RelayAddr, state: SetState) -> anyhow::Result<()> {
        let muxed = target as u8;
        if (muxed & 1) == 1 {
            self.first_relay.set(state.clone())?;
        }

        if (muxed >> 1) == 1 {
            self.second_relay.set(state)?;
        }

        Ok(())
    }

    pub fn resolve_addr(&self, name: &str) -> Option<RelayAddr> {
        if name.eq("both") {
            Some(RelayAddr::Both)
        } else if name.eq(self.first_relay.name) {
            Some(RelayAddr::First)
        } else if name.eq(self.second_relay.name) {
            Some(RelayAddr::Second)
        } else {
            None
        }
    }

    #[must_use]
    pub fn pool_event(&mut self) -> [Option<Event>; 2]
    {
        let t = sys_now();
        let e1 = self.first_relay.is_run_deadline(t);
        let e2 = self.second_relay.is_run_deadline(t);

        let mut events: [Option<Event>; 2] = [const { None }; 2];
        if e1 {
            events[0] = Some(Event{
                name: self.first_relay.name,
                run_deadline: e1
            })
        }

        if e2 {
            events[1] = Some(Event{
                name: self.second_relay.name,
                run_deadline: e2
            })
        }

        events
    }

    pub fn get_status(&self, target: RelayAddr) -> DoubleRelayStatus {
        let single = match target {
            RelayAddr::First => self.first_relay.get_status(),
            RelayAddr::Second => self.second_relay.get_status(),
            RelayAddr::Both => return DoubleRelayStatus::Both([
                self.first_relay.get_status(),
                self.second_relay.get_status()
            ])
        };

        DoubleRelayStatus::Single(single)
    }

    const NAME_NOTFOUND: &'static str = "cannot resolve name";
    const INV_INSTRUCTION: &'static str = "invalid instruction";
    pub fn interprete(&mut self, query: RelayQuery) -> anyhow::Result<DoubleRelayStatus> {
        let name = query.name.ok_or(Error::msg(Self::NAME_NOTFOUND))?;
        let r_addr = self.resolve_addr(name).ok_or(Error::msg(Self::NAME_NOTFOUND))?;

        let instruction = query.instruction.ok_or(Error::msg(Self::INV_INSTRUCTION))?;
        let instruction = match instruction {
            true => {
                let t = sys_now();
                let end = match query.duration {
                    None => t + 3600,
                    Some(dur) => t + dur as u64
                };
                SetState::Run(RunOrder::new(t, end))
            }, false => SetState::Stop,
        };
        
        self.set(r_addr, instruction)?;
        Ok(self.get_status(r_addr))
    }
}

pub enum DoubleRelayStatus<'r> {
    Single(RelayStatus<'r>),
    Both([RelayStatus<'r>; 2])
}

impl<'r> Display for DoubleRelayStatus<'r> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DoubleRelayStatus::Single(s) => write!(f, "{}", s),
            DoubleRelayStatus::Both(b) => write!(f, "{}\n\n{}", b[0], b[1])
        }
    }
}

pub struct RelayStatus<'r> {
    pub name: &'r str,
    pub run_info: Option<&'r RunOrder>
}

impl<'r> Display for RelayStatus<'r> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Relay {} status ", self.name)?;
        match self.run_info {
            None => write!(f, "off"),
            Some(ord) => write!(f, "on\nstart: {}\nfinish: {}", ord.start_at, ord.end_at),
        }
    }
}
#[derive(Default)]
pub struct RelayQuery<'a> {
    pub name: Option<&'a str>,
    /// set On when is true
    pub instruction: Option<bool>,
    /// time second
    pub duration: Option<u32>
}
