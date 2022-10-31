use std::fmt::Display;

use chrono::{DateTime, FixedOffset, TimeZone};
use chrono_tz::Asia::Vladivostok;
use lazy_static::lazy_static;
use regex::Regex;
use select::{
    document::Document,
    node::Find,
    predicate::{Name, Predicate},
};
use serde::{Deserialize, Serialize};

lazy_static! {
    static ref WIND_DIRECTION: Regex = Regex::new("([0-9]{1,3})°").unwrap();
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Observation {
    pub time: DateTime<FixedOffset>,
    pub direction: u16,
    pub avg_speed: f32,
}

impl Display for Observation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {:2.1} m/s {:3}°",
            self.time.format("%H:%M"),
            self.avg_speed,
            self.direction
        )
    }
}

pub fn parse(input: &str) -> Vec<Observation> {
    let document = Document::from(input);
    let selector = Name("table").descendant(Name("tr"));
    let rows = document.find(selector);
    let mut result = vec![];

    for row in rows {
        if let Some(_) = row.find(Name("th")).next() {
            // skippping header
            continue;
        }
        let mut columns = row.find(Name("td"));
        let time = parse_column(&mut columns, vlat_time_parser);
        let direction = parse_column(&mut columns, direction_parser);
        let avg_speed = parse_column(&mut columns, wind_speed_parser);

        match (time, direction, avg_speed) {
            (Some(time), Some(direction), Some(avg_speed)) => result.push(Observation {
                time,
                direction,
                avg_speed,
            }),
            _ => {
                panic!("Unable to parse: {}", row.html())
            }
        }
    }
    result
}

fn parse_column<O, I: Predicate>(
    columns: &mut Find<I>,
    parser: fn(&str) -> Option<O>,
) -> Option<O> {
    columns.next().and_then(|n| parser(&n.text()))
}

// Parsing the string of format: `СЗЗ (301°)`
fn direction_parser(input: &str) -> Option<u16> {
    if let Some(caps) = WIND_DIRECTION.captures(input) {
        let direction = caps.get(1).unwrap();
        let direction = direction.as_str().parse::<u16>().unwrap();
        if direction <= 360 {
            return Some(direction % 360);
        }
    }
    None
}

fn wind_speed_parser(input: &str) -> Option<f32> {
    input.parse::<f32>().ok()
}

// Parse time in format: `29.10.2022 22:45` assuming it's in VLAT
fn vlat_time_parser(input: &str) -> Option<DateTime<FixedOffset>> {
    Vladivostok
        .datetime_from_str(input, "%d.%m.%Y %H:%M")
        .ok()
        .map(|t| t.with_timezone(&FixedOffset::east(10 * 3600)))
}

#[cfg(test)]
mod test {

    use super::*;
    use insta::assert_yaml_snapshot;

    #[test]
    fn foo() {
        let input = include_str!("../tests/example.html");
        assert_yaml_snapshot!(parse(input));
    }
}
