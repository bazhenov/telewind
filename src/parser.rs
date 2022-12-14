use crate::prelude::*;
use anyhow::bail;
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
use std::fmt::Display;

lazy_static! {
    static ref WIND_DIRECTION: Regex = Regex::new("([0-9]{1,3})°").unwrap();
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Observation {
    pub time: DateTime<FixedOffset>,
    pub direction: u16,
    pub avg_speed: f32,
}

const DIRECTIONS: [(u16, &str, &str); 9] = [
    (0, "N", "↓"),
    (360, "N", "↓"),
    (45, "NE", "↙"),
    (90, "E", "←"),
    (135, "SE", "↖"),
    (180, "S", "↑"),
    (225, "SW", "↗"),
    (270, "W", "→"),
    (315, "NW", "↘"),
];

impl Display for Observation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (_, direction_str, direction_marker) = DIRECTIONS
            .iter()
            .min_by_key(|d| self.direction.abs_diff(d.0))
            .unwrap();
        write!(
            f,
            "{} {:2.1} m/s {:2} {} ({:3}°)",
            self.time.format("%H:%M"),
            self.avg_speed,
            direction_str,
            direction_marker,
            self.direction
        )
    }
}

pub fn parse(input: &str) -> Result<Vec<Observation>> {
    let document = Document::from(input);
    let selector = Name("table").descendant(Name("tr"));
    let rows = document.find(selector);
    let mut result = vec![];

    for row in rows {
        if row.find(Name("th")).next().is_some() {
            // skippping header
            continue;
        }
        let mut columns = row.find(Name("td"));
        let time = parse_column(&mut columns, vlat_time_parser)?;
        let direction = parse_column(&mut columns, direction_parser)?;
        let avg_speed = parse_column(&mut columns, wind_speed_parser)?;

        match (time, direction, avg_speed) {
            (Some(time), Some(direction), Some(avg_speed)) => result.push(Observation {
                time,
                direction,
                avg_speed,
            }),
            _ => bail!("Unable to parse HTML"),
        }
    }
    Ok(result)
}

fn parse_column<O, I: Predicate>(
    columns: &mut Find<I>,
    parser: fn(&str) -> Result<Option<O>>,
) -> Result<Option<O>> {
    if let Some(column) = columns.next() {
        Ok(parser(&column.text())?)
    } else {
        bail!("No column left")
    }
}

// Parsing the string of format: `СЗЗ (301°)`
fn direction_parser(input: &str) -> Result<Option<u16>> {
    if let Some(caps) = WIND_DIRECTION.captures(input) {
        let direction = caps.get(1).unwrap();
        let direction = direction.as_str().parse::<u16>()?;
        if direction <= 360 {
            return Ok(Some(direction % 360));
        }
    }
    Ok(None)
}

fn wind_speed_parser(input: &str) -> Result<Option<f32>> {
    Ok(Some(input.parse::<f32>()?))
}

// Parse time in format: `29.10.2022 22:45` assuming it's in VLAT
fn vlat_time_parser(input: &str) -> Result<Option<DateTime<FixedOffset>>> {
    let time = Vladivostok.datetime_from_str(input, "%d.%m.%Y %H:%M")?;
    Ok(Some(time.with_timezone(&FixedOffset::east(10 * 3600))))
}

#[cfg(test)]
mod test {

    use super::*;
    use insta::assert_yaml_snapshot;

    #[test]
    fn foo() -> Result<()> {
        let input = include_str!("../tests/example.html");
        assert_yaml_snapshot!(parse(input)?);

        Ok(())
    }
}
