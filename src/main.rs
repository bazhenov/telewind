use chrono::{DateTime, FixedOffset};
use clap::{Parser, Subcommand};
use dotenv::dotenv;
use futures::{stream, Stream, StreamExt};
use log::{debug, trace, warn};
use parser::{parse, Observation};
use std::{
    cmp::Reverse,
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
use tokio::time::{self, Interval, MissedTickBehavior};

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
    dotenv().ok();
    env_logger::init();
    if dotenv::var("TOKIO_CONSOLE_SUBSCRIBER").ok().is_some() {
        debug!("Initializaing tokio.rs console subscriber");
        console_subscriber::init();
    }

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
        let event_fired = fsm.step(&observation);
        let after_state = fsm.state();
        println!("{observation} {event_fired:>6}    {after_state:?}")
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
    /// Returns true if target event is found (FSM reach [`WindState::High`] state)
    fn step(&mut self, observation: &Observation) -> bool {
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

    fn state(&self) -> WindState {
        self.state
    }
}

/// Circle sector
///
/// Can test if given angle (0-359 deg.) is in circle sector.
/// Sector is defined as two angles (from angle and to angle). Two angles
/// always given in clockwise order, so `Sector::new(270, 90)` is upper half circle and
/// `Sector::new(90, 270)` is lower.
struct Sector(u16, u16);

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

/// Stream of new observations realtime
///
/// Parse remote URL with given interval and return new observations one by one
fn observation_stream(url: &str, interval: Interval) -> impl Stream<Item = Observation> {
    struct State {
        url: String,
        interval: Interval,
        // parsed but not yet processed observations in reverse order ()
        observations: Vec<Observation>,
        last_parse_time: Option<DateTime<FixedOffset>>,
    }

    async fn next_observation(mut state: State) -> Option<(Observation, State)> {
        loop {
            if let Some(observation) = state.observations.pop() {
                return Some((observation, state));
            }

            state.interval.tick().await;
            let body = reqwest::get(&state.url)
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            let mut last_observations = parse(&body);
            if !last_observations.is_empty() {
                last_observations.sort_by_key(|o| Reverse(o.time));

                state.observations = match state.last_parse_time {
                    Some(time) => last_observations
                        .into_iter()
                        .filter(|o| o.time > time)
                        .collect(),
                    // Take most recent observation at the start of the system
                    None => vec![last_observations.swap_remove(0)],
                };
                state.last_parse_time = state
                    .observations
                    .iter()
                    .map(|o| o.time)
                    .max()
                    .or(state.last_parse_time);
            }
        }
    }

    let state = State {
        url: url.to_owned(),
        interval,
        observations: vec![],
        last_parse_time: None,
    };

    stream::unfold(state, next_observation)
}

mod tg {
    use super::*;

    pub(crate) async fn run_bot(opts: Opts) {
        let token = env::var("TELEGRAM_BOT_TOKEN").expect("TELEGRAM_BOT_TOKEN not set");
        let bot = Arc::new(Bot::new(token));
        let mut users = HashSet::new();
        users.insert(ChatId(230741741));
        let users = Arc::new(Mutex::new(users));

        let subscription_loop_handle = tokio::task::Builder::new()
            .name("subscription loop")
            .spawn(subscription_loop(bot.clone(), users.clone()))
            .unwrap();
        let parse_loop_handle = tokio::task::Builder::new()
            .name("parse and notify loop")
            .spawn(parse_and_notify_loop(opts, bot, users))
            .unwrap();

        parse_loop_handle.await.unwrap();
        subscription_loop_handle.await.unwrap();
    }

    async fn parse_and_notify_loop(opts: Opts, bot: Arc<Bot>, users: Shared<Users>) {
        let mut fsm = WindTracker {
            state: WindState::Low,
            avg_speed_threshold: opts.speed,
            candidate_steps: 5,
            cooldown_steps: 5,
            wind_sector: Sector::NORTH_180,
        };
        let mut interval = time::interval(Duration::from_secs(55));
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

        let mut observations = Box::pin(observation_stream(&opts.url, interval));
        while let Some(obs) = observations.next().await {
            let event_fired = fsm.step(&obs);
            let after_state = fsm.state();
            trace!("Processing observation: {} ({:?})", obs, after_state);

            if event_fired {
                let users = users.lock().unwrap().iter().cloned().collect::<Vec<_>>();
                tg::notify(&obs, &bot, &users[..]).await;
            }
        }
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
        bot: Arc<Bot>,
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
            "Wind is growing up: {observation}. Sending notifications to {} users",
            users.len()
        );

        let message = format!("Wind is growing up: {observation}");
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
