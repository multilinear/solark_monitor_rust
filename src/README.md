# solark_monitor2
Poles a solark inverter over modbus and places the results in influxDB. Also sends alerts to Matrix when thresholds are crossed.

This script is intended to run on a machine proximal to your solark, and
connected to it via a cable. It poles the solark over modbus gathering the
requested metrics. The metrics are also dumped in InfluxDB

## compatibility
This has been tested on a x86_64 Gentoo Linux machine running Rust 1.77.1. That
machine is connected to a Sol-Ark-15k-2P-N solar inverter. There is no reason it shouldn't work on other *nix devices at least, as well as working for RS485, but that has not been tested.

## Running/Installation
If you run `cargo run` in a checkout it will run the program.

To install run: `cargo build -r`. The resulting binary will be placed in
`target/release/solark_monitor2`. You can copy it wherever you want it.

Note that this program will not run out of the box without a valid configuration
file. See configuration below.

## Configuration
By default the monitor tries to read a config file from `/etc/solark_monitor.toml`, but
you can provide the filename to read from on the commandline `cargo run your-file.toml`

The example configuration file provided (solark_monitor.toml) should be pretty
self explanatory. But here's a rundown

### InfluxDB
These options should all be trivially obvious if you're familiar with influxDB.

### Solark
- port: This will typically be the port that your USB->Serial adapter is plugged
  into. `/dev/ttyUSB0` is a good guess.
- slave_id: I believe this is *always* 1, but I'm not positive so it's an option
- poll_secs: how often to poll the solark
- registers: "Registers" are a modbus concept. A register is constructed of
  16-bit chunks (nibbles). On the solark most registers are 1 nibble, but a few
  are larger. 
    - "datatype" specifies how many nibbles to read, at the moment
        Bool64Bit is the only multi-nibble type and it's used ot read if the system
        has any faults. We don't bother storing *what* the fault is.
    - "metric" simply names the metric we get from that register, this will be
        used for alerting and for naming data in InfluxDB

For definitions of the registers available see:
[[https://www.dth.net/solar/sol-ark/Modbus%20Sol-Ark%20V1.1%28Public%20Release%29.pdf]]

### Matrix
These options are self explanatory

### Alerting
The alert language leaves a lot to be desired, but it works for simple cases.
- Metric: says what metric to compare to
- msg: says what message to send if the alert fires
- check: can start with: '<' '>' '=' '<=' '>=' or '!=' the next part will be
  read as an integer to compare to.

A message is sent when an alert occurs, and another is sent when it "clears"
meaning when condition is no longer true.

`alert_timeout_secs` specifies a time when a message will be sent if the alert
is still true. This is useful as a "reminder" that things aren't back to normal
yet. I have mine set to 24 hours (in seconds).

## Getting alerts
You'll need to create an account for your bot and configure the bot to log into
it, and you'll need to put your account in the allowed_users list.

Now, log in to matrix with your preferred client and invite the bot to chat. It
may take a while due to matrix latency and polling frequency, but the bot should
join the chat eventually. If you invite the bot to a room everyone in the room
must be on the allowed_users list AND it must be invited by an allowed_user. If
not it won't join at all, or will leave the chat again.

This works transparently with encryption if you enable it.

## Design Choices
Having written a previous version of this in python, I got annoyed at how
python requires extensive testing due to it's dynamic nature, and I was finding
rare events were annoying to debug. I'd wanted to do something real rust anyway, so I decided to port what I had.

I've made some attempts to make this tool flexible for other users, but at root
it is a home project I built for myself. If others choose to use it I'd be happy
to expand the support or add features, but to avoid over-engineering, until
there's interest I'm sticking to just the features that I need.

## Implied Warrenty etc.

There is NO implied warrenty or fitness for use! If you break your god-awful
expensive inverter it's not my fault. I just wrote some code I'm using and am
sharing it to help others. This is the "read only" modbus API so it shouldn't
break anything, but "shouldn't" is never the same as "can't". So good luck.
