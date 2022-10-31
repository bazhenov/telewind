use std::time::Duration;

use chrono::{DateTime, FixedOffset};
use clap::Parser;
use log::{trace, warn};
use parser::{parse, Observation};
use tokio::time::{sleep, sleep_until};

mod parser;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Opts {
    /// source url to download
    #[arg(short, long, default_value_t = String::from("http://3volna.ru/anemometer/getwind?id=1"))]
    url: String,

    #[arg(short, long, default_value_t = 5.0)]
    speed: f32,
}

#[tokio::main]
async fn main2() {
    env_logger::init();

    let mut luup = NotificationLoop;
    luup.run().await
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let opts = Opts::parse();
    let body = reqwest::get(opts.url).await.unwrap().text().await.unwrap();

    let mut fsm = TrackingFsm {
        state: State::Low,
        wind_sector: Sector::EAST_90,
        candidate_steps: 2,
        cooldown_steps: 2,
        avg_speed_threshold: opts.speed,
    };

    let mut observations = parse(&body);
    observations.reverse();
    for observation in observations {
        let from_state = fsm.state();
        let to_state = fsm.step(&observation);

        let event_fired = match (from_state, to_state) {
            (State::Low, State::High) => true,
            (State::Candidate(..), State::High) => true,
            _ => false,
        };
        println!(
            "{time}   {wind:2.1}m/s  {direction:3}° {event_fired:>6}    {to_state:?}",
            time = observation.time,
            wind = observation.avg_speed,
            direction = observation.direction
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Low,
    Candidate(u8),
    High,
    Cooldown(u8),
}

struct TrackingFsm {
    state: State,
    wind_sector: Sector,
    candidate_steps: u8,
    cooldown_steps: u8,
    avg_speed_threshold: f32,
}

impl TrackingFsm {
    fn step(&mut self, observation: &Observation) -> State {
        use State::*;

        let direction_match = self.wind_sector.test(observation.direction);
        let speed_match = observation.avg_speed >= self.avg_speed_threshold;
        self.state = if speed_match && direction_match {
            match self.state {
                High => High,
                Low if self.candidate_steps == 0 => High,
                Low => Candidate(1),
                Candidate(i) if i >= self.candidate_steps => High,
                Candidate(i) => Candidate(i + 1),
                Cooldown(..) => High,
            }
        } else {
            match self.state {
                Low => Low,
                Candidate(..) => Low,
                High if self.cooldown_steps == 0 => Low,
                High => Cooldown(1),
                Cooldown(i) if i >= self.cooldown_steps => Low,
                Cooldown(i) => Cooldown(i + 1),
            }
        };
        self.state
    }

    fn state(&self) -> State {
        self.state
    }
}

/// Circle sector
///
/// Can test if given angle (0-350 deg.) is in circle sector.
/// Sector is defined as two angles (from angle and to angle). Two angles
/// always given in clockwise order, so `Sector::new(270, 90)` is upper half circle and
/// `Sector::new(90, 270)` is lower.
struct Sector(u16, u16);

impl Sector {
    const NORTH_90: Sector = Sector(315, 45);
    const EAST_90: Sector = Sector(45, 135);
    const SOUTH_90: Sector = Sector(135, 225);
    const WEST_90: Sector = Sector(225, 315);

    fn new(angle_from: u16, angle_to: u16) -> Self {
        Self(angle_from, angle_to)
    }

    fn test(&self, angle: u16) -> bool {
        let angle = angle % 360;
        if self.0 <= self.1 {
            self.0 <= angle && angle <= self.1
        } else {
            self.0 <= angle || angle <= self.1
        }
    }
}

struct NotificationLoop;

impl NotificationLoop {
    async fn run(&mut self) {
        let mut fsm = TrackingFsm {
            state: State::Low,
            avg_speed_threshold: 1.0,
            candidate_steps: 2,
            cooldown_steps: 2,
            wind_sector: Sector::EAST_90,
        };

        let mut last_time: Option<DateTime<FixedOffset>> = None;
        let url = "http://3volna.ru/anemometer/getwind?id=1";
        loop {
            let body = reqwest::get(url).await.unwrap().text().await.unwrap();
            let mut observations = parse(&body);
            observations.sort_by_key(|o| o.time);

            let new_observations = match last_time {
                Some(last_time) => observations
                    .into_iter()
                    .filter(|o| o.time > last_time)
                    .collect(),
                // Take only last observation as input at the start of the system
                None => observations.split_off(observations.len() - 1),
            };

            if let Some(last_observation) = new_observations.last() {
                last_time = Some(last_observation.time)
            }

            let mut fired_observation = None;

            for obs in new_observations {
                trace!("Processing new observation: {}", obs);

                let event_fired = match (fsm.state(), fsm.step(&obs)) {
                    (State::Low, State::High) => true,
                    (State::Candidate(..), State::High) => true,
                    _ => false,
                };

                if event_fired {
                    fired_observation = fired_observation.or(Some(obs));
                }
            }

            if let Some(observation) = fired_observation {
                self.notify(&observation);
            }

            sleep(Duration::from_secs(10)).await;
        }
    }

    fn notify(&self, observation: &Observation) {
        warn!(
            "Wind is growing up: {speed:2.1} m/s, {direction:3}°",
            speed = observation.avg_speed,
            direction = observation.direction
        );
    }
}

#[cfg(test)]
mod test {

    use super::*;
    use crate::parser::Observation;
    use chrono::{DateTime, Duration, FixedOffset};

    struct ObservationSequence {
        time: DateTime<FixedOffset>,
    }

    impl ObservationSequence {
        fn next(&mut self, avg_speed: f32, direction: u16) -> Observation {
            self.time = self.time + Duration::minutes(1);
            Observation {
                time: self.time,
                avg_speed,
                direction,
            }
        }
    }

    fn new_seq_and_fsm(
        candidate_steps: u8,
        cooldown_steps: u8,
    ) -> (ObservationSequence, TrackingFsm) {
        let seq = ObservationSequence {
            time: DateTime::parse_from_rfc3339("2022-02-01T00:00:00+10:00").unwrap(),
        };

        let fsm = TrackingFsm {
            state: State::Low,
            wind_sector: Sector(135, 225), // SE-SW
            candidate_steps,
            cooldown_steps,
            avg_speed_threshold: 5.0,
        };
        (seq, fsm)
    }

    #[test]
    fn fsm_full_cycle() {
        let (mut seq, mut fsm) = new_seq_and_fsm(2, 2);

        assert_eq!(fsm.step(&seq.next(3.2, 180)), State::Low);
        assert_eq!(fsm.step(&seq.next(5.7, 180)), State::Candidate(1));
        assert_eq!(fsm.step(&seq.next(5.4, 180)), State::Candidate(2));
        assert_eq!(fsm.step(&seq.next(5.4, 180)), State::High);
        assert_eq!(fsm.step(&seq.next(3.5, 180)), State::Cooldown(1));
        assert_eq!(fsm.step(&seq.next(3.5, 180)), State::Cooldown(2));
        assert_eq!(fsm.step(&seq.next(4.1, 180)), State::Low);
    }

    #[test]
    fn fsm_directorion_mismatch() {
        let (mut seq, mut fsm) = new_seq_and_fsm(2, 2);

        assert_eq!(fsm.step(&seq.next(5.0, 180)), State::Candidate(1));
        assert_eq!(fsm.step(&seq.next(5.0, 0)), State::Low);
    }

    #[test]
    fn fsm_candidate_reset() {
        let (mut seq, mut fsm) = new_seq_and_fsm(2, 2);

        assert_eq!(fsm.step(&seq.next(5.7, 180)), State::Candidate(1));
        assert_eq!(fsm.step(&seq.next(3.4, 180)), State::Low);
    }

    #[test]
    fn fsm_cooldown_reset() {
        let (mut seq, mut fsm) = new_seq_and_fsm(0, 2);

        assert_eq!(fsm.step(&seq.next(5.7, 180)), State::High);
        assert_eq!(fsm.step(&seq.next(3.7, 180)), State::Cooldown(1));
        assert_eq!(fsm.step(&seq.next(5.4, 180)), State::High);
    }

    #[test]
    fn sector() {
        let sector = Sector::new(0, 45);

        assert_eq!(true, sector.test(0));
        assert_eq!(true, sector.test(30));
        assert_eq!(true, sector.test(45));

        assert_eq!(false, sector.test(46));
        assert_eq!(false, sector.test(359));

        let sector = Sector::new(280, 90);

        assert_eq!(true, sector.test(290));
        assert_eq!(true, sector.test(0));
        assert_eq!(true, sector.test(45));
        assert_eq!(true, sector.test(90));

        assert_eq!(false, sector.test(180));
        assert_eq!(false, sector.test(279));
    }
}
