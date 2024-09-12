use anyhow::Error;
use esp_idf_svc::hal::{gpio::{Output, OutputPin, PinDriver}, peripheral::Peripheral};

use crate::helper::sys_now;

#[derive(Clone)]
pub struct RunOrder {
    #[allow(dead_code)]
    start_at: u64,
    end_at: u64
}

impl RunOrder {
    #[inline]
    /// panic when end <= start
    pub fn new(start_at: u64, end_at: u64) -> Self {
        assert!(start_at <= end_at);
        Self{ start_at, end_at }
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
            return r.end_at <= now;
        }
        false
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
        if muxed == 1 {
            self.first_relay.set(state.clone())?;
        }

        if (muxed << 1) == 1 {
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

    const NAME_NOTFOUND: &'static str = "cannot resolve name";
    const INV_INSTRUCTION: &'static str = "invalid instruction";
    pub fn interprete(&mut self, query: RelayQuery) -> anyhow::Result<()> {
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
        Ok(())
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
