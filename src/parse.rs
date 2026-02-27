use bytes::Bytes;
use serde::Deserialize;
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug)]
pub enum Problem {
    UnrecognisedTopic,
    BadUtf8,
    BadJson,
    Stale,
}

#[derive(Clone, Debug)]
pub enum Message {
    Empty,
    #[allow(dead_code)]
    Err(Problem, Bytes),
    Boiler(crate::pb::Boiler),
    Climate(crate::pb::Climate),
    SimpleSwitch(crate::pb::SimpleSwitch),
}

fn check_last_seen(ls_epoch: u64, max_staleness: Duration) -> Result<(), ()> {
    match std::time::UNIX_EPOCH.checked_add(Duration::from_millis(ls_epoch)) {
        Some(last_seen) => match SystemTime::now().duration_since(last_seen) {
            Ok(staleness) if staleness <= max_staleness => Ok(()),
            _ => Err(()),
        },
        None => Err(()),
    }
}

fn on_off(v: &str) -> Result<bool, ()> {
    match v {
        "ON" => Ok(true),
        "OFF" => Ok(false),
        _ => Err(()),
    }
}

#[derive(Deserialize, Debug)]
struct BoilerJson {
    state_l1: String,
    state_l2: String,
    last_seen: u64,
}

fn parse_boiler(p: &Bytes) -> Result<Message, Problem> {
    let json = std::str::from_utf8(p).map_err(|_| Problem::BadUtf8)?;
    let m = serde_json::from_str::<BoilerJson>(json).map_err(|_| Problem::BadJson)?;
    check_last_seen(m.last_seen, Duration::from_secs(300)).map_err(|_| Problem::Stale)?;
    Ok(Message::Boiler(crate::pb::Boiler {
        heating: Some(on_off(&m.state_l2).map_err(|_| Problem::BadJson)?),
        hot_water: Some(on_off(&m.state_l1).map_err(|_| Problem::BadJson)?),
    }))
}

#[derive(Deserialize, Debug)]
struct ClimateJson {
    battery: f64,
    humidity: f64,
    temperature: f64,
    last_seen: u64,
}

fn parse_climate(p: &Bytes, unit: &str) -> Result<Message, Problem> {
    let json = std::str::from_utf8(p).map_err(|_| Problem::BadUtf8)?;
    let m = serde_json::from_str::<ClimateJson>(json).map_err(|_| Problem::BadJson)?;
    check_last_seen(m.last_seen, Duration::from_secs(4500)).map_err(|_| Problem::Stale)?;
    Ok(Message::Climate(crate::pb::Climate {
        unit: Some(unit.to_string()),
        battery: Some(m.battery),
        temperature: Some(m.temperature),
        humidity: Some(m.humidity),
    }))
}

#[derive(Deserialize, Debug)]
struct SimpleSwitchJson {
    state: String,
    last_seen: u64,
}

fn parse_simple_switch(p: &Bytes) -> Result<Message, Problem> {
    let json = std::str::from_utf8(p).map_err(|_| Problem::BadUtf8)?;
    let m = serde_json::from_str::<SimpleSwitchJson>(json).map_err(|_| Problem::BadJson)?;
    check_last_seen(m.last_seen, Duration::from_secs(66000)).map_err(|_| Problem::Stale)?;
    Ok(Message::SimpleSwitch(crate::pb::SimpleSwitch {
        state: Some(on_off(&m.state).map_err(|_| Problem::BadJson)?),
    }))
}

impl From<rumqttc::Publish> for Message {
    fn from(p: rumqttc::Publish) -> Message {
        let m = match p.topic.as_str() {
            "zigbee/boiler" => parse_boiler(&p.payload),
            "zigbee/garden" => parse_simple_switch(&p.payload),
            "zigbee/kitchen_ceiling" => parse_simple_switch(&p.payload),
            "zigbee/kitchen_under_stairs" => parse_simple_switch(&p.payload),
            "zigbee/kitchen_under_cupboards" => parse_simple_switch(&p.payload),
            t => match t.strip_prefix("zigbee/climate/") {
                Some(unit) => parse_climate(&p.payload, unit),
                None => Err(Problem::UnrecognisedTopic),
            },
        };
        match m {
            Ok(m) => m,
            Err(problem) => Message::Err(problem, p.payload),
        }
    }
}
