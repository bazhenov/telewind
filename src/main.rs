use anyhow::Context;
use chrono::{DateTime, FixedOffset};
use clap::{Parser, Subcommand};
use dotenv::dotenv;
use futures::{stream, Stream, StreamExt};
use parser::{parse, Observation};
use std::{
    cmp::Reverse,
    env,
    sync::{Arc, Mutex},
    time::Duration,
};
use telewind::{parser, prelude::*, Sector, WindState, WindTracker};
use teloxide::{
    dispatching::UpdateFilterExt,
    dptree::{self, deps},
    prelude::Dispatcher,
    requests::Requester,
    types::{ChatId, ChatKind, MediaKind, Message, MessageKind, Update},
    Bot,
};
use tokio::time::{self, Interval, MissedTickBehavior};

type Shared<T> = Arc<Mutex<T>>;

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
async fn main() -> Result<()> {
    dotenv().ok();
    env_logger::init();
    if dotenv::var("TOKIO_CONSOLE_SUBSCRIBER").ok().is_some() {
        debug!("Initializaing tokio.rs console subscriber");
        console_subscriber::init();
    }

    let args = Args::parse();

    match args.action {
        Action::Parse(opts) => run_parse(&opts).await?,
        Action::RunTelegramBot(opts) => tg::run_bot(opts).await?,
    }
    Ok(())
}

async fn run_parse(opts: &Opts) -> Result<()> {
    let body = reqwest::get(&opts.url).await?.text().await?;

    let mut fsm = WindTracker {
        state: WindState::Low,
        wind_sector: Sector::EAST_90,
        candidate_steps: 2,
        cooldown_steps: 2,
        avg_speed_threshold: opts.speed,
    };

    let mut observations = parse(&body)?;
    observations.reverse();
    for observation in observations {
        let event_fired = fsm.step(&observation);
        let after_state = fsm.state();
        println!("{observation} {event_fired:>6}    {after_state:?}")
    }

    Ok(())
}

/// Stream of new observations realtime
///
/// Parse remote URL with given interval and return new observations one by one
fn observation_stream(url: &str, interval: Interval) -> impl Stream<Item = Result<Observation>> {
    struct State {
        url: String,
        interval: Interval,
        // parsed but not yet processed observations in reverse order ()
        observations: Vec<Observation>,
        last_parse_time: Option<DateTime<FixedOffset>>,
    }

    async fn read_data_using_http(url: &str) -> Result<String> {
        let body = reqwest::get(url)
            .await
            .context(ObservationsEndpointFailed(url.to_string()))?;
        Ok(body.text().await?)
    }

    async fn next_observation(mut state: State) -> Option<(Result<Observation>, State)> {
        loop {
            if let Some(observation) = state.observations.pop() {
                return Some((Ok(observation), state));
            }

            state.interval.tick().await;

            let response = match read_data_using_http(&state.url).await {
                Ok(body) => body,
                Err(e) => {
                    error!("Unable to read data from remote HTTP-endpoint. We'll keep trying...");
                    warn!("{}", e);
                    continue;
                }
            };
            let mut last_observations = match parse(&response) {
                Ok(observations) => observations,
                Err(e) => return Some((Err(e), state)),
            };
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
    use telewind::Subscriptions;
    use teloxide::types::MediaText;

    pub(crate) async fn run_bot(opts: Opts) -> Result<()> {
        let database_url = env::var("DATABASE_URL").context("DATABASE_URL not set")?;
        let subscriptions = Subscriptions::new(&database_url)?;

        let token = env::var("TELEGRAM_BOT_TOKEN").context("TELEGRAM_BOT_TOKEN not set")?;
        let bot = Arc::new(Bot::new(token));

        let subscriptions = Arc::new(Mutex::new(subscriptions));

        let subscription_loop_handle = tokio::task::Builder::new()
            .name("subscription loop")
            .spawn(subscription_loop(bot.clone(), subscriptions.clone()))?;
        let parse_loop_handle = tokio::task::Builder::new()
            .name("parse and notify loop")
            .spawn(parse_and_notify_loop(opts, bot, subscriptions))?;

        parse_loop_handle.await??;
        subscription_loop_handle.await?;

        Ok(())
    }

    async fn parse_and_notify_loop(
        opts: Opts,
        bot: Arc<Bot>,
        subscriptions: Shared<Subscriptions>,
    ) -> Result<()> {
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
            let obs = obs?;
            let event_fired = fsm.step(&obs);
            let after_state = fsm.state();
            trace!("Processing observation: {} ({:?})", obs, after_state);

            if event_fired {
                let users = subscriptions
                    .lock()
                    .unwrap()
                    .list_subscriptions()?
                    .into_iter()
                    .map(|s| ChatId(s.user_id))
                    .collect::<Vec<_>>();
                tg::notify(&obs, &bot, &users[..]).await?;
            }
        }
        Ok(())
    }

    async fn subscription_loop(bot: Arc<Bot>, users: Shared<Subscriptions>) {
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
        subscriptions: Shared<Subscriptions>,
    ) -> Result<()> {
        debug!("{:?}", &msg);
        if let ChatKind::Private { .. } = msg.chat.kind {
            let chat_id = msg.chat.id;
            if let MessageKind::Common(msg) = msg.kind {
                if let MediaKind::Text(MediaText { text, .. }) = msg.media_kind {
                    match text.as_str() {
                        "/subscribe" => {
                            debug!("Subscribing {:?}", chat_id);
                            subscriptions.lock().unwrap().new_subscription(chat_id.0)?;
                            bot.send_message(chat_id, "You are subscribed sucessfully!")
                                .await?;
                        }
                        "/unsubscribe" => {
                            debug!("Unsubscribing {:?}", chat_id);
                            subscriptions
                                .lock()
                                .unwrap()
                                .remove_subscription(chat_id.0)?;
                            bot.send_message(chat_id, "You are unsubscribed").await?;
                        }
                        _ => {}
                    }
                }
            }
        }

        Ok(())
    }

    pub(crate) async fn notify(
        observation: &Observation,
        bot: &Bot,
        users: &[ChatId],
    ) -> Result<()> {
        warn!(
            "Wind is growing up: {observation}. Sending notifications to {} users",
            users.len()
        );

        let message = format!("Wind is growing up: {observation}");
        for chat in users.iter() {
            bot.send_message(*chat, &message).await?;
        }

        Ok(())
    }
}
