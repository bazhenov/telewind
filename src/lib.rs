pub mod models;
pub mod parser;
pub mod schema;

use diesel::prelude::*;
use diesel::{Connection, SqliteConnection};
use models::{NewSubscription, Subscription};
use parser::Observation;
use schema::subscriptions;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindState {
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
pub struct WindTracker {
    pub state: WindState,
    pub wind_sector: Sector,
    /// Number of steps require for FSM to reach [`WindState::High`] from [`WindState::Low`]
    pub candidate_steps: u8,
    /// Number of steps require for FSM to reset from [`WindState::High`] to [`WindState::Low`]
    pub cooldown_steps: u8,
    /// Target threshold for wind speed
    pub avg_speed_threshold: f32,
}

impl WindTracker {
    /// Returns true if target event is found (FSM reach [`WindState::High`] state)
    pub fn step(&mut self, observation: &Observation) -> bool {
        use WindState::*;

        let before_state = self.state;
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
        matches!(
            (before_state, self.state),
            (Low, High) | (Candidate(_), High)
        )
    }

    pub fn state(&self) -> WindState {
        self.state
    }
}

/// Circle sector
///
/// Can test if given angle (0-359 deg.) is in circle sector.
/// Sector is defined as two angles (from angle and to angle). Two angles
/// always given in clockwise order, so `Sector::new(270, 90)` is upper half circle and
/// `Sector::new(90, 270)` is lower.
pub struct Sector(u16, u16);

impl Sector {
    #[allow(dead_code)]
    pub const NORTH_180: Sector = Sector(270, 90);

    #[allow(dead_code)]
    pub const SOUTH_180: Sector = Sector(90, 270);

    #[allow(dead_code)]
    pub const EAST_180: Sector = Sector(0, 180);

    #[allow(dead_code)]
    pub const WEST_180: Sector = Sector(180, 0);

    #[allow(dead_code)]
    pub const NORTH_90: Sector = Sector(315, 45);

    #[allow(dead_code)]
    pub const EAST_90: Sector = Sector(45, 135);

    #[allow(dead_code)]
    pub const SOUTH_90: Sector = Sector(135, 225);

    #[allow(dead_code)]
    pub const WEST_90: Sector = Sector(225, 315);

    fn test(&self, angle: u16) -> bool {
        let angle = angle % 360;
        if self.0 <= self.1 {
            self.0 <= angle && angle <= self.1
        } else {
            self.0 <= angle || angle <= self.1
        }
    }
}

pub struct Subscriptions(pub SqliteConnection);

impl Subscriptions {
    pub fn new(database_url: &str) -> Self {
        let connection =
            SqliteConnection::establish(database_url).expect("Unable to open connection");
        Subscriptions(connection)
    }

    pub fn new_subscription(&mut self, user_id: i64) {
        let time = SystemTime::now();
        let time = time.duration_since(UNIX_EPOCH).unwrap().as_secs();
        let subscription = NewSubscription {
            user_id,
            created_at: time as i64,
        };
        diesel::insert_or_ignore_into(subscriptions::table)
            .values(&subscription)
            .execute(&mut self.0)
            .expect("Error saving new subscription");
    }

    pub fn list_subscriptions(&mut self) -> Vec<Subscription> {
        use schema::subscriptions::dsl::*;
        subscriptions
            .load(&mut self.0)
            .expect("Unable to read subscriptions")
    }

    pub fn remove_subscription(&mut self, user_id: i64) {
        use schema::subscriptions::dsl::{subscriptions, user_id as subsciption_user_id};
        diesel::delete(subscriptions)
            .filter(subsciption_user_id.eq(user_id))
            .execute(&mut self.0)
            .expect("Unable to remove subscription");
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

    fn step(fsm: &mut WindTracker, observation: &Observation) -> WindState {
        fsm.step(&observation);
        fsm.state()
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

        assert_eq!(step(&mut fsm, &seq.next(3.2, 180)), WindState::Low);
        assert_eq!(step(&mut fsm, &seq.next(5.7, 180)), WindState::Candidate(1));
        assert_eq!(step(&mut fsm, &seq.next(5.4, 180)), WindState::Candidate(2));
        assert_eq!(step(&mut fsm, &seq.next(5.4, 180)), WindState::High);
        assert_eq!(step(&mut fsm, &seq.next(3.5, 180)), WindState::Cooldown(1));
        assert_eq!(step(&mut fsm, &seq.next(3.5, 180)), WindState::Cooldown(2));
        assert_eq!(step(&mut fsm, &seq.next(4.1, 180)), WindState::Low);
    }

    #[test]
    fn fsm_directorion_mismatch() {
        let (mut seq, mut fsm) = new_seq_and_fsm(2, 2);

        assert_eq!(step(&mut fsm, &seq.next(5.0, 180)), WindState::Candidate(1));
        assert_eq!(step(&mut fsm, &seq.next(5.0, 0)), WindState::Low);
    }

    #[test]
    fn fsm_candidate_reset() {
        let (mut seq, mut fsm) = new_seq_and_fsm(2, 2);

        assert_eq!(step(&mut fsm, &seq.next(5.7, 180)), WindState::Candidate(1));
        assert_eq!(step(&mut fsm, &seq.next(3.4, 180)), WindState::Low);
    }

    #[test]
    fn fsm_cooldown_reset() {
        let (mut seq, mut fsm) = new_seq_and_fsm(0, 2);

        assert_eq!(step(&mut fsm, &seq.next(5.7, 180)), WindState::High);
        assert_eq!(step(&mut fsm, &seq.next(3.7, 180)), WindState::Cooldown(1));
        assert_eq!(step(&mut fsm, &seq.next(5.4, 180)), WindState::High);
    }

    #[test]
    fn sector() {
        let sector = Sector(0, 45);

        assert_eq!(true, sector.test(0));
        assert_eq!(true, sector.test(30));
        assert_eq!(true, sector.test(45));

        assert_eq!(false, sector.test(46));
        assert_eq!(false, sector.test(359));

        let sector = Sector(280, 90);

        assert_eq!(true, sector.test(290));
        assert_eq!(true, sector.test(0));
        assert_eq!(true, sector.test(45));
        assert_eq!(true, sector.test(90));

        assert_eq!(false, sector.test(180));
        assert_eq!(false, sector.test(279));
    }
}
