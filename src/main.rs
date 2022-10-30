use clap::Parser;
use parser::{parse, Observation};

mod parser;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Opts {
    /// source url to download
    #[arg(short, long, default_value_t = String::from("http://3volna.ru/anemometer/getwind?id=1"))]
    url: String,
}

#[tokio::main]
async fn main() {
    let opts = Opts::parse();
    let body = reqwest::get(opts.url).await.unwrap().text().await.unwrap();

    for observation in parse(&body) {
        println!("{observation:?}")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Low,
    Candidate(u8),
    High,
    Cooldown(u8),
}

struct EventTrackingFsm {
    state: State,
    candidate_steps: u8,
    cooldown_steps: u8,
    avg_speed_threshold: f32,
}

impl EventTrackingFsm {
    fn step(&mut self, observation: &Observation) -> State {
        use State::*;

        self.state = if observation.avg_speed >= self.avg_speed_threshold {
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
    ) -> (ObservationSequence, EventTrackingFsm) {
        let seq = ObservationSequence {
            time: DateTime::parse_from_rfc3339("2022-02-01T00:00:00+10:00").unwrap(),
        };

        let fsm = EventTrackingFsm {
            state: State::Low,
            candidate_steps,
            cooldown_steps,
            avg_speed_threshold: 5.0,
        };
        (seq, fsm)
    }

    #[test]
    fn fsm_full_cycle() {
        let (mut seq, mut fsm) = new_seq_and_fsm(2, 2);

        assert_eq!(fsm.step(&seq.next(3.2, 20)), State::Low);
        assert_eq!(fsm.step(&seq.next(5.7, 20)), State::Candidate(1));
        assert_eq!(fsm.step(&seq.next(5.4, 20)), State::Candidate(2));
        assert_eq!(fsm.step(&seq.next(5.4, 20)), State::High);
        assert_eq!(fsm.step(&seq.next(3.5, 20)), State::Cooldown(1));
        assert_eq!(fsm.step(&seq.next(3.5, 20)), State::Cooldown(2));
        assert_eq!(fsm.step(&seq.next(4.1, 20)), State::Low);
    }

    #[test]
    fn fsm_candidate_reset() {
        let (mut seq, mut fsm) = new_seq_and_fsm(2, 2);

        assert_eq!(fsm.step(&seq.next(5.7, 20)), State::Candidate(1));
        assert_eq!(fsm.step(&seq.next(3.4, 20)), State::Low);
    }

    #[test]
    fn fsm_cooldown_reset() {
        let (mut seq, mut fsm) = new_seq_and_fsm(0, 2);

        assert_eq!(fsm.step(&seq.next(5.7, 20)), State::High);
        assert_eq!(fsm.step(&seq.next(3.7, 20)), State::Cooldown(1));
        assert_eq!(fsm.step(&seq.next(5.4, 20)), State::High);
    }
}
