use chrono::{DateTime, FixedOffset};
use clap::{Parser, Subcommand};
use dotenv::dotenv;
use log::{debug, trace, warn};
use parser::{parse, Observation};
use std::{
    collections::HashSet,
    env,
    sync::{Arc, Mutex},
    time::Duration,
};
use teloxide::{
    dispatching::UpdateFilterExt,
    dptree::{self, deps},
    prelude::Dispatcher,
    requests::Requester,
    types::{ChatId, ChatKind, MediaKind, Message, MessageKind, Update},
    Bot, RequestError,
};
use tokio::time::sleep;

mod parser;

type Shared<T> = Arc<Mutex<T>>;
type Users = HashSet<ChatId>;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    action: Action,
}

#[derive(Parser, Debug, Clone)]
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
    /// running telegram bot
    RunTelegramBot(Opts),
}

#[tokio::main]
async fn main() {
    env_logger::init();
    dotenv().ok();

    let args = Args::parse();

    match args.action {
        Action::Parse(opts) => run_parse(&opts).await,
        Action::RunTelegramBot(opts) => tg::run_bot(opts).await,
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

async fn parse_loop(opts: Opts, bot: Arc<Bot>, users: Shared<Users>) {
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
            trace!("Processing observation: {}", obs);

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
            let users = users.lock().unwrap().iter().cloned().collect::<Vec<_>>();
            tg::notify(&observation, &bot, &users[..]).await;
        }

        sleep(Duration::from_secs(10)).await;
    }
}

mod tg {
    use super::*;

    pub(crate) async fn run_bot(opts: Opts) {
        let token = env::var("TELEGRAM_BOT_TOKEN").expect("TELEGRAM_BOT_TOKEN not set");
        let bot = Arc::new(Bot::new(token));
        let users = Arc::new(Mutex::new(HashSet::new()));

        let subscription_loop_handle = tokio::spawn(subscription_loop(bot.clone(), users.clone()));
        let parse_loop_handle = tokio::spawn(parse_loop(opts, bot, users));

        parse_loop_handle.await.unwrap();
        subscription_loop_handle.await.unwrap();
    }

    async fn subscription_loop(bot: Arc<Bot>, users: Shared<Users>) {
        let handler =
            dptree::entry().branch(Update::filter_message().endpoint(subscription_handler));
        Dispatcher::builder(bot, handler)
            .dependencies(deps![users])
            .build()
            .dispatch()
            .await;
    }

    async fn subscription_handler(
        bot: Bot,
        msg: Message,
        users: Shared<Users>,
    ) -> Result<(), RequestError> {
        debug!("{:?}", &msg);
        if let ChatKind::Private { .. } = msg.chat.kind {
            let chat_id = msg.chat.id;
            if let MessageKind::Common(msg) = msg.kind {
                if let MediaKind::Text(_) = msg.media_kind {
                    debug!("Subscribing {:?}", chat_id);
                    users.lock().unwrap().insert(chat_id);
                    bot.send_message(chat_id, "You are subscribed sucessfully!")
                        .await?;
                }
            }
        }

        Ok(())
    }

    pub(crate) async fn notify(observation: &Observation, bot: &Bot, users: &[ChatId]) {
        warn!(
            "Wind is growing up: {speed:2.1} m/s, {direction:3}°",
            speed = observation.avg_speed,
            direction = observation.direction
        );

        let message = format!(
            "Wind is growing up: {speed:2.1} m/s, {direction:3}°",
            speed = observation.avg_speed,
            direction = observation.direction
        );
        for chat in users.iter() {
            bot.send_message(*chat, &message).await.unwrap();
        }
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
