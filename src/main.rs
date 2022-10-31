use std::time::Duration;

use chrono::{DateTime, FixedOffset};
use clap::{Parser, Subcommand};
use log::{trace, warn};
use parser::{parse, Observation};
use tokio::time::sleep;

mod parser;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    action: Action,
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Opts {
    /// source url to download
    #[arg(short, long, default_value_t = String::from("http://3volna.ru/anemometer/getwind?id=1"))]
    url: String,

    #[arg(short, long, default_value_t = 5.0)]
    speed: f32,
}

#[derive(Debug, Subcommand)]
#[clap(author, version, about, long_about = None)]
enum Action {
    /// parse remote url
    Parse(Opts),
    /// running notification loop
    Run(Opts),
}

#[tokio::main]
async fn main() {
    env_logger::init();

    let args = Args::parse();

    match args.action {
        Action::Parse(opts) => run_parse(&opts).await,
        Action::Run(opts) => run_notification_loop(&opts).await,
    }
}

async fn run_parse(opts: &Opts) {
    let body = reqwest::get(&opts.url).await.unwrap().text().await.unwrap();

    let mut fsm = WindTracker {
        state: WindState::Low,
        wind_sector: Sector::EAST_90,
        candidate_steps: 2,
        cooldown_steps: 2,
        avg_speed_threshold: opts.speed,
    };

    let mut observations = parse(&body);
    observations.reverse();
    for observation in observations {
        let (from_state, to_state) = (fsm.state(), fsm.step(&observation));

        let event_fired = match (from_state, to_state) {
            (WindState::Low, WindState::High) => true,
            (WindState::Candidate(..), WindState::High) => true,
            _ => false,
        };
        println!("{observation} {event_fired:>6}    {to_state:?}")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum WindState {
    Low,
    Candidate(u8),
    High,
    Cooldown(u8),
}

/// Wind state tracking FSM
///
/// Implements hysterizis. Given number of observations (steps) are required for FSM to reach [`WindState::High`] state
/// (and reset to [`WindState::Low`] state). [`WindState::Candidate`] and [`WindState::Cooldown`] are transient states
/// created just for that reason.
struct WindTracker {
    state: WindState,
    wind_sector: Sector,
    /// Number of steps require for FSM to reach [`WindState::High`] from [`WindState::Low`]
    candidate_steps: u8,
    /// Number of steps require for FSM to reset from [`WindState::High`] to [`WindState::Low`]
    cooldown_steps: u8,
    /// Target threshold for wind speed
    avg_speed_threshold: f32,
}

impl WindTracker {
    fn step(&mut self, observation: &Observation) -> WindState {
        use WindState::*;

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

    fn state(&self) -> WindState {
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

async fn run_notification_loop(opts: &Opts) {
    let mut fsm = WindTracker {
        state: WindState::Low,
        avg_speed_threshold: opts.speed,
        candidate_steps: 2,
        cooldown_steps: 2,
        wind_sector: Sector::EAST_90,
    };

    let mut last_time: Option<DateTime<FixedOffset>> = None;
    loop {
        let body = reqwest::get(&opts.url).await.unwrap().text().await.unwrap();
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
                (WindState::Low, WindState::High) => true,
                (WindState::Candidate(..), WindState::High) => true,
                _ => false,
            };

            if event_fired {
                fired_observation = fired_observation.or(Some(obs));
            }
        }

        if let Some(observation) = fired_observation {
            notify(&observation);
        }

        sleep(Duration::from_secs(10)).await;
    }
}

fn notify(observation: &Observation) {
    warn!(
        "Wind is growing up: {speed:2.1} m/s, {direction:3}°",
        speed = observation.avg_speed,
        direction = observation.direction
    );
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
    ) -> (ObservationSequence, WindTracker) {
        let seq = ObservationSequence {
            time: DateTime::parse_from_rfc3339("2022-02-01T00:00:00+10:00").unwrap(),
        };

        let fsm = WindTracker {
            state: WindState::Low,
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

        assert_eq!(fsm.step(&seq.next(3.2, 180)), WindState::Low);
        assert_eq!(fsm.step(&seq.next(5.7, 180)), WindState::Candidate(1));
        assert_eq!(fsm.step(&seq.next(5.4, 180)), WindState::Candidate(2));
        assert_eq!(fsm.step(&seq.next(5.4, 180)), WindState::High);
        assert_eq!(fsm.step(&seq.next(3.5, 180)), WindState::Cooldown(1));
        assert_eq!(fsm.step(&seq.next(3.5, 180)), WindState::Cooldown(2));
        assert_eq!(fsm.step(&seq.next(4.1, 180)), WindState::Low);
    }

    #[test]
    fn fsm_directorion_mismatch() {
        let (mut seq, mut fsm) = new_seq_and_fsm(2, 2);

        assert_eq!(fsm.step(&seq.next(5.0, 180)), WindState::Candidate(1));
        assert_eq!(fsm.step(&seq.next(5.0, 0)), WindState::Low);
    }

    #[test]
    fn fsm_candidate_reset() {
        let (mut seq, mut fsm) = new_seq_and_fsm(2, 2);

        assert_eq!(fsm.step(&seq.next(5.7, 180)), WindState::Candidate(1));
        assert_eq!(fsm.step(&seq.next(3.4, 180)), WindState::Low);
    }

    #[test]
    fn fsm_cooldown_reset() {
        let (mut seq, mut fsm) = new_seq_and_fsm(0, 2);

        assert_eq!(fsm.step(&seq.next(5.7, 180)), WindState::High);
        assert_eq!(fsm.step(&seq.next(3.7, 180)), WindState::Cooldown(1));
        assert_eq!(fsm.step(&seq.next(5.4, 180)), WindState::High);
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
