use crate::Result;
use lazy_static::lazy_static;
use log::warn;
use prometheus::{
    register_gauge, register_gauge_vec, register_int_counter_vec, register_int_gauge,
    register_int_gauge_vec, Gauge, GaugeVec, IntCounterVec, IntGauge, IntGaugeVec,
};
use std::net::SocketAddr;
use std::thread;
use std::time::Duration;
use systemstat::{saturating_sub_bytes, Platform, System};

// Value-related metrics
lazy_static! {
    pub static ref BINARY: GaugeVec =
        register_gauge_vec!("input_binary", "binary input values by alias", &["alias"]).unwrap();
    pub static ref CONTINUOUS: GaugeVec = register_gauge_vec!(
        "input_continuous",
        "continuous 16-bit input values by alias",
        &["alias"]
    )
    .unwrap();
    pub static ref TEMPERATURE: GaugeVec = register_gauge_vec!(
        "input_temperature",
        "temperature in celsius by alias",
        &["alias"]
    )
    .unwrap();
    pub static ref HUMIDITY: GaugeVec =
        register_gauge_vec!("input_humidity", "relative humidity by alias", &["alias"]).unwrap();
    pub static ref PRESSURE: GaugeVec = register_gauge_vec!(
        "input_pressure",
        "air pressure in pascals by alias",
        &["alias"]
    )
    .unwrap();
    pub static ref GAS: GaugeVec =
        register_gauge_vec!("input_gas", "gas in ohms by alias", &["alias"]).unwrap();
    pub static ref VALUE_OK: GaugeVec = register_gauge_vec!(
        "input_value_ok",
        "whether the current input value is ok, by alias",
        &["alias"]
    )
    .unwrap();
    pub static ref VALUE_UPDATES: IntCounterVec = register_int_counter_vec!(
        "input_updates",
        "number of input value updates by alias and status",
        &["alias", "status"]
    )
    .unwrap();
}

// System-related metrics
lazy_static! {
    pub static ref SYSTEM_MEMORY_USED: IntGauge = register_int_gauge!(
        "system_memory_used",
        "amount of memory used (=total-free) in bytes"
    )
    .unwrap();
    pub static ref SYSTEM_LOAD_AVERAGE: GaugeVec =
        register_gauge_vec!("system_load_average", "Linux load average", &["duration"]).unwrap();
    pub static ref SYSTEM_CPU_TEMPERATURE: Gauge =
        register_gauge!("system_cpu_temperature", "CPU temperature in celsius").unwrap();
    pub static ref SYSTEM_NETWORK_STATS: IntGaugeVec = register_int_gauge_vec!(
        "system_network_stats",
        "network statistics by interface and value",
        &["interface", "value"]
    )
    .unwrap();
}

pub(crate) fn start_prometheus(addr: SocketAddr) -> Result<()> {
    thread::Builder::new()
        .name("prom-system-stats".to_string())
        .spawn(track_system_stats)?;
    prometheus_exporter::start(addr)?;
    Ok(())
}

fn track_system_stats() {
    let sys = System::new();
    let load_avg_one = SYSTEM_LOAD_AVERAGE
        .get_metric_with_label_values(&["1m"])
        .unwrap();
    let load_avg_five = SYSTEM_LOAD_AVERAGE
        .get_metric_with_label_values(&["5m"])
        .unwrap();
    let load_avg_fifteen = SYSTEM_LOAD_AVERAGE
        .get_metric_with_label_values(&["15m"])
        .unwrap();

    loop {
        match sys.memory() {
            Ok(mem) => {
                let used = saturating_sub_bytes(mem.total, mem.free);
                SYSTEM_MEMORY_USED.set(used.as_u64() as i64);
            }
            Err(x) => warn!("unable to get memory stats: {}", x),
        }

        match sys.load_average() {
            Ok(load_avg) => {
                load_avg_one.set(load_avg.one as f64);
                load_avg_five.set(load_avg.five as f64);
                load_avg_fifteen.set(load_avg.fifteen as f64)
            }
            Err(x) => warn!("unable to get load average: {}", x),
        }

        match sys.cpu_temp() {
            Ok(cpu_temp) => {
                SYSTEM_CPU_TEMPERATURE.set(cpu_temp as f64);
            }
            Err(x) => warn!("unable to get CPU temperature: {}", x),
        }

        match sys.networks() {
            Ok(netifs) => {
                for netif in netifs.values() {
                    let stats = sys.network_stats(&netif.name);
                    match stats {
                        Ok(stats) => {
                            SYSTEM_NETWORK_STATS
                                .get_metric_with_label_values(&[netif.name.as_str(), "rx_bytes"])
                                .unwrap()
                                .set(stats.rx_bytes.as_u64() as i64);
                            SYSTEM_NETWORK_STATS
                                .get_metric_with_label_values(&[netif.name.as_str(), "tx_bytes"])
                                .unwrap()
                                .set(stats.tx_bytes.as_u64() as i64);
                            SYSTEM_NETWORK_STATS
                                .get_metric_with_label_values(&[netif.name.as_str(), "rx_packets"])
                                .unwrap()
                                .set(stats.rx_packets as i64);
                            SYSTEM_NETWORK_STATS
                                .get_metric_with_label_values(&[netif.name.as_str(), "tx_packets"])
                                .unwrap()
                                .set(stats.tx_packets as i64);
                            SYSTEM_NETWORK_STATS
                                .get_metric_with_label_values(&[netif.name.as_str(), "rx_errors"])
                                .unwrap()
                                .set(stats.rx_errors as i64);
                            SYSTEM_NETWORK_STATS
                                .get_metric_with_label_values(&[netif.name.as_str(), "tx_errors"])
                                .unwrap()
                                .set(stats.tx_errors as i64);
                        }
                        Err(e) => {
                            warn!("unable to get stats for interface {}: {}", netif.name, e);
                        }
                    }
                }
            }
            Err(x) => warn!("unable to get interfaces: {}", x),
        }

        thread::sleep(Duration::from_secs(2));
    }
}
