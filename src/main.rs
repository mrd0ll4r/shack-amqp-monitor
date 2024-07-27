use crate::config::Config;
use alloy::amqp::{ExchangeShackInput, RoutingKeySubscription};
use alloy::api::TimestampedInputValue;
use alloy::config::{InputValue, InputValueType, UniverseConfig};
use alloy::event::{AddressedEvent, Event, EventKind};
use anyhow::{Context, Result};
use flexi_logger::{DeferredNow, Logger, LoggerHandle, TS_DASHES_BLANK_COLONS_DOT_BLANK};
use futures::StreamExt;
use itertools::Either;
use log::{debug, error, info, Record};
use reqwest::Url;
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc::Sender;
use tokio::sync::Mutex;
use tokio::task;

mod config;
mod http;
mod prom;

type State = Arc<
    Mutex<
        HashMap<
            String,
            (
                Sender<Either<AddressedEvent, TimestampedInputValue>>,
                Arc<Mutex<VecDeque<Event>>>,
            ),
        >,
    >,
>;

#[tokio::main]
async fn main() -> Result<()> {
    info!("setting up logging...");
    set_up_logging().unwrap();
    debug!("set up logging");

    info!("reading config...");
    let cfg = Config::read_from_file("config.yaml").context("unable to read config")?;
    debug!("read config: {:?}", cfg);

    info!("starting prometheus...");
    let prometheus_addr = cfg
        .prometheus_listen_address
        .parse::<SocketAddr>()
        .context("unable to parse prometheus_listen_address")?;
    prom::start_prometheus(prometheus_addr)?;
    debug!("started prometheus");

    info!("getting universe config...");
    let universe_config = get_universe_config(&cfg.submarine_http_endpoint)
        .await
        .context("unable to get universe config")?;
    debug!("got universe config: {:?}", universe_config);

    info!("spawning tasks to populate prometheus metrics...");
    let senders = spawn_metrics_tasks(&universe_config, cfg.update_history_count)
        .context("unable to spawn metrics tasks")?;
    debug!("got {} senders", senders.len());

    let senders = Arc::new(Mutex::new(senders));

    info!("connecting to AMQP broker...");
    let shack_inputs =
        ExchangeShackInput::new(&cfg.amqp_server_address, &vec![RoutingKeySubscription::All])
            .await
            .context("unable to connect to AMQP server")?;
    debug!("connected to AMQP broker: {:?}", shack_inputs);

    info!("spawning task to handle incoming events...");
    let event_handler = task::spawn(handle_incoming_events(
        shack_inputs,
        cfg.update_history_count,
        senders.clone(),
    ));

    info!("getting initial values from submarine...");
    populate_metrics_with_initial_values(&cfg.submarine_http_endpoint, senders.clone())
        .await
        .context("unable to populate metrics with initial values")?;

    info!("starting HTTP server...");
    let http_server_address = cfg.http_server_listen_address.parse()?;
    let http_server = task::spawn(http::run_server(http_server_address, senders));
    info!("HTTP server is listening on http://{}", http_server_address);

    info!("sleeping forever (peacefully)... ");

    tokio::select! {
        _ = event_handler => {}
        _ = http_server => {}
    }

    Ok(())
}

async fn handle_incoming_events(
    mut exchange: ExchangeShackInput,
    update_history_count: u32,
    state: State,
) -> Result<()> {
    while let Some(res) = exchange.next().await {
        let (rk, msg) = res.context("unable to receive message")?;
        let alias = rk.alias.as_ref().unwrap();
        let input_type = rk.input_value_type.unwrap();
        debug!("got event for alias {}: {:?}", alias, msg);

        {
            let mut state = state.lock().await;
            if !state.contains_key(alias) {
                // New sensor?
                match spawn_single_metric_task(input_type, alias.to_string(), update_history_count)
                {
                    Err(err) => {
                        error!(
                            "unable to set up new receiver for alias {} (type {:?}): {:?}",
                            alias, input_type, err
                        );
                        continue;
                    }
                    Ok(receiver) => {
                        state.insert(alias.to_string(), receiver);
                    }
                }
            }
        }
        let state = state.lock().await;
        let sender = state.get(alias).expect("missing sender");
        sender
            .0
            .send(Either::Left(msg))
            .await
            .context("unable to send event")?;
    }

    Ok(())
}

async fn populate_metrics_with_initial_values(
    submarine_http_base_addr: &str,
    state: State,
) -> Result<()> {
    let mut u = Url::parse(submarine_http_base_addr)
        .context("unable to parse submarine HTTP base address")?;
    u.set_path("api/v1/universe/last_values");
    let resp: HashMap<String, Option<TimestampedInputValue>> = reqwest::get(u)
        .await
        .context("unable to get latest values from submarine")?
        .json()
        .await
        .context("unable to decode latest values")?;
    debug!("got latest values {:?}", resp);

    // Push each into the respective channel
    for (alias, val) in resp.into_iter() {
        match val {
            None => {
                debug!("got empty initial value for alias {}", alias)
            }
            Some(val) => {
                debug!("populating alias {} with initial value {:?}", alias, val);
                {
                    let state = state.lock().await;
                    let sender = state
                        .get(&alias)
                        .expect("missing sender for submarine alias");
                    sender
                        .0
                        .send(Either::Right(val))
                        .await
                        .context("unable to send initial value")?;
                }
            }
        }
    }

    Ok(())
}

fn spawn_metrics_tasks(
    universe_config: &UniverseConfig,
    update_history_count: u32,
) -> Result<
    HashMap<
        String,
        (
            Sender<Either<AddressedEvent, TimestampedInputValue>>,
            Arc<Mutex<VecDeque<Event>>>,
        ),
    >,
> {
    let mut senders = HashMap::new();

    for dev in universe_config.devices.iter() {
        for input_dev in dev.inputs.iter() {
            let (sender, queue) = spawn_single_metric_task(
                input_dev.input_type,
                input_dev.alias.clone(),
                update_history_count,
            )
            .context(format!(
                "unable to spawn metric task for {}",
                input_dev.alias
            ))?;

            senders.insert(input_dev.alias.clone(), (sender, queue));
        }
        for output_dev in dev.outputs.iter() {
            // We fake an input device for each of these.
            let (sender, queue) = spawn_single_metric_task(
                InputValueType::Continuous,
                output_dev.alias.clone(),
                update_history_count,
            )
            .context(format!(
                "unable to spawn metric task for {}",
                output_dev.alias
            ))?;

            senders.insert(output_dev.alias.clone(), (sender, queue));
        }
    }

    Ok(senders)
}

fn spawn_single_metric_task(
    input_type: InputValueType,
    alias: String,
    update_history_count: u32,
) -> Result<(
    Sender<Either<AddressedEvent, TimestampedInputValue>>,
    Arc<Mutex<VecDeque<Event>>>,
)> {
    let value_metric = match input_type {
        InputValueType::Binary => prom::BINARY.with_label_values(&[alias.as_str()]),
        InputValueType::Temperature => prom::TEMPERATURE.with_label_values(&[alias.as_str()]),
        InputValueType::Humidity => prom::HUMIDITY.with_label_values(&[alias.as_str()]),
        InputValueType::Pressure => prom::PRESSURE.with_label_values(&[alias.as_str()]),
        InputValueType::Gas => prom::GAS.with_label_values(&[alias.as_str()]),
        InputValueType::Continuous => prom::CONTINUOUS.with_label_values(&[alias.as_str()]),
    };
    let ok_metric = prom::VALUE_OK.with_label_values(&[alias.as_str()]);
    let update_ok_counter = prom::VALUE_UPDATES.with_label_values(&[alias.as_str(), "ok"]);
    let update_error_counter = prom::VALUE_UPDATES.with_label_values(&[alias.as_str(), "error"]);

    let (sender, mut receiver) =
        tokio::sync::mpsc::channel::<Either<AddressedEvent, TimestampedInputValue>>(1);
    let queue = Arc::new(Mutex::new(VecDeque::with_capacity(
        update_history_count as usize,
    )));
    let ret_queue = queue.clone();

    tokio::spawn(async move {
        let mut first = true;

        while let Some(val) = receiver.recv().await {
            match val {
                Either::Left(event) => {
                    // Update metric
                    match &event.event.inner {
                        Ok(inner) => {
                            match inner {
                                EventKind::Update { new_value } => {
                                    value_metric.set(input_value_to_f64(new_value));

                                    ok_metric.set(1_f64);
                                    update_ok_counter.inc();
                                }
                                // Ignore anything that's not an update
                                _ => {}
                            }
                        }
                        Err(_) => {
                            ok_metric.set(0_f64);
                            update_error_counter.inc();
                        }
                    }

                    // Push to history buffer
                    {
                        let mut queue = queue.lock().await;
                        while queue.len() >= update_history_count as usize {
                            queue.pop_front();
                        }
                        queue.push_back(event.event);
                    }
                }
                Either::Right(val) => {
                    // Only update metrics if we didn't get an event yet.
                    if !first {
                        continue;
                    }
                    match val.value {
                        Ok(val) => {
                            value_metric.set(input_value_to_f64(&val));
                            ok_metric.set(1_f64);
                        }
                        Err(_) => {
                            ok_metric.set(0_f64);
                        }
                    }
                }
            }

            first = false;
        }
    });

    Ok((sender, ret_queue))
}

fn input_value_to_f64(val: &InputValue) -> f64 {
    match val {
        InputValue::Binary(b) => {
            if *b {
                1_f64
            } else {
                0_f64
            }
        }
        InputValue::Temperature(t) => *t,
        InputValue::Humidity(h) => *h,
        InputValue::Pressure(p) => *p,
        InputValue::Gas(g) => *g,
        InputValue::Continuous(c) => *c as f64,
    }
}

async fn get_universe_config(submarine_http_base_addr: &str) -> Result<UniverseConfig> {
    let mut u = Url::parse(submarine_http_base_addr)
        .context("unable to parse submarine HTTP base address")?;
    u.set_path("api/v1/universe/config");
    let resp = reqwest::get(u)
        .await
        .context("unable to get universe config from submarine")?
        .json()
        .await
        .context("unable to decode universe config")?;

    Ok(resp)
}

fn log_format(
    w: &mut dyn std::io::Write,
    now: &mut DeferredNow,
    record: &Record,
) -> Result<(), std::io::Error> {
    write!(
        w,
        "[{}] {} [{}] {}:{}: {}",
        now.format(TS_DASHES_BLANK_COLONS_DOT_BLANK),
        record.level(),
        record.metadata().target(),
        //record.module_path().unwrap_or("<unnamed>"),
        record.file().unwrap_or("<unnamed>"),
        record.line().unwrap_or(0),
        &record.args()
    )
}

pub fn set_up_logging() -> Result<LoggerHandle, Box<dyn error::Error>> {
    let logger = Logger::try_with_env_or_str("info")?
        .use_utc()
        .format(log_format);

    let handle = logger.start()?;

    Ok(handle)
}
