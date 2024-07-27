# shack-amqp-monitor

History and Prometheus Metrics for Submarine Sensor data

## General Operation

This subscribes to an AMQP broker for input data updates, see the [alloy](../alloy/) project.
The data is published on Prometheus as well as a local HTTP endpoint, including a history of timestamped updates.

## License

GPL, see [LICENSE](LICENSE).